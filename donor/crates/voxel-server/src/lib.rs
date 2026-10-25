//! Игровой сервер (mp/design.md §2.3–2.4, MP-1): tokio + axum — сеть, поток симуляции 20 Гц — мир.
//!
//! Почему axum (а не голый tokio-tungstenite): один маршрутизатор на /ws и /status (MP-2: доска «Сейчас в мире»,
//! проверка выкладки), заголовки при upgrade (Origin — защита от чужого сайта, §6), предел размера кадра; WebSocket
//! внутри — тот же tokio-tungstenite, им же пользуется бот, — одна сборка.
//!
//! Потоки (§2.4): симуляция — свой std::thread: Server (voxel-core net.rs) создаётся внутри него (держит Rc) и
//! ни с кем не делится. tokio (2 потока) держит соединения: входящие кадры — в общий std::sync::mpsc потока
//! симуляции, исходящие — в ограниченную очередь соединения; переполнилась (медленный клиент) — соединение
//! закрывается. Цикл симуляции: кадры обрабатываются по приходу (пинг точнее), тик — каждые 50 мс по часам
//! (next += 50 мс; отстали больше 2 с — пропуск и строка в журнал).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc as tmpsc;

use voxel_core::net::{ConnId, Peer, Server, Status};

/// Наибольший кадр клиента (§6: ≤ 64 КБ).
const MAX_FRAME: usize = 64 << 10;
/// Исходящих кадров в очереди соединения (≈ 12 с тиков): больше — клиент не успевает, отключить.
const OUT_QUEUE: usize = 256;
/// Тик сервера.
pub const TICK: Duration = Duration::from_millis(50);
/// Соединение, приславшее последний кадр больше стольких секунд назад, считается оборванным (Config.idle). Живой клиент
/// шлёт тик 20 раз в секунду, ушедший (Away) — Pong раз в секунду; обрыв сети без FIN сервер иначе замечает через
/// минуты, а ник всё это время занят — перезаход клиента получал бы «ник занят».
pub const IDLE_SECS: u64 = 15;

/// Настройки сервера.
#[derive(Clone, Debug)]
pub struct Config {
    pub seed: u32,
    /// Папка мира (нет — в памяти).
    pub data: Option<String>,
    /// Адрес /ws и /status (нет — без сети).
    pub listen: Option<String>,
    /// Отладка игрокам (полёт сквозь стены H, набор K, сдвиг J, время) — только для разработки.
    pub dev: bool,
    /// Остановиться через столько тиков (0 — работать до Ctrl+C).
    pub ticks: u64,
    /// Без пауз между тиками (проверка без сети).
    pub fast: bool,
    /// Разрешённые Origin, кроме localhost и 127.0.0.1 (сайт игры — MP-2).
    pub origins: Vec<String>,
    /// Автосохранение мира в папку data раз в столько секунд (0 — выключено; при остановке мир пишется всегда).
    pub autosave: u64,
    /// Секунд тишины, после которых соединение закрывается (IDLE_SECS).
    pub idle: u64,
}

impl Default for Config {
    fn default() -> Config {
        Config { seed: 1337, data: None, listen: None, dev: false, ticks: 0, fast: false, origins: Vec::new(), autosave: 30, idle: IDLE_SECS }
    }
}

/// Событие сети для потока симуляции.
enum NetEv {
    Open { net: u64, out: tmpsc::Sender<Out> },
    Frame { net: u64, bytes: Vec<u8> },
    Close { net: u64 },
    Stop,
}

/// Кадр соединению или «закрыть».
enum Out {
    Frame(Vec<u8>),
    Close,
}

#[derive(Clone)]
struct App {
    to_sim: mpsc::Sender<NetEv>,
    next: Arc<AtomicU64>,
    status: Arc<Mutex<String>>,
    origins: Arc<Vec<String>>,
}

/// Запущенный сервер (проверки): адрес, остановка, ожидание потока симуляции.
pub struct Running {
    pub addr: SocketAddr,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    sim: Option<std::thread::JoinHandle<Result<String, String>>>,
    to_sim: mpsc::Sender<NetEv>,
    serve: tokio::task::JoinHandle<()>,
}

impl Running {
    /// Остановить: сеть, затем симуляцию (запись мира, если есть папка). Ответ — итог потока симуляции.
    pub async fn stop(mut self) -> Result<String, String> {
        if let Some(s) = self.stop.take() {
            let _ = s.send(());
        }
        let _ = self.serve.await;
        let _ = self.to_sim.send(NetEv::Stop);
        let sim = self.sim.take().unwrap();
        tokio::task::spawn_blocking(move || sim.join().unwrap_or_else(|_| Err("поток симуляции упал".into()))).await.unwrap()
    }
}

/// Поднять сервер: поток симуляции и (если задан listen) /ws и /status. Мир открывается в потоке симуляции; не
/// открылся — ошибка сразу.
pub async fn start(cfg: Config) -> Result<Running, String> {
    let (to_sim, from_net) = mpsc::channel::<NetEv>();
    let status = Arc::new(Mutex::new(String::from("{}")));
    let (ready_tx, ready_rx) = mpsc::channel::<Result<u32, String>>();
    let sim_cfg = cfg.clone();
    let st = status.clone();
    let sim = std::thread::Builder::new()
        .name("sim".into())
        .spawn(move || sim_loop(sim_cfg, from_net, st, ready_tx))
        .map_err(|e| e.to_string())?;
    let seed = tokio::task::spawn_blocking(move || ready_rx.recv().unwrap_or_else(|_| Err("поток симуляции не поднялся".into())))
        .await
        .map_err(|e| e.to_string())??;
    let addr_s = cfg.listen.clone().unwrap_or_else(|| "127.0.0.1:0".into());
    let listener = tokio::net::TcpListener::bind(&addr_s).await.map_err(|e| format!("{addr_s}: {e}"))?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    let app = App { to_sim: to_sim.clone(), next: Arc::new(AtomicU64::new(1)), status, origins: Arc::new(cfg.origins.clone()) };
    let router = Router::new().route("/ws", get(ws_upgrade)).route("/status", get(status_json)).with_state(app);
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let serve = tokio::spawn(async move {
        let _ = axum::serve(listener, router).with_graceful_shutdown(async move {
            let _ = stop_rx.await;
        })
        .await;
    });
    eprintln!(
        "voxel-server: сборка {}, сид {seed}, ws://{addr}/ws, http://{addr}/status{}{}",
        voxel_core::net::build(),
        if cfg.dev { ", отладка" } else { "" },
        match (&cfg.data, cfg.autosave) {
            (Some(d), s) if s > 0 => format!(", мир {d} (автосохранение {s} с)"),
            (Some(d), _) => format!(", мир {d} (без автосохранения)"),
            _ => ", мир в памяти".into(),
        }
    );
    Ok(Running { addr, stop: Some(stop_tx), sim: Some(sim), to_sim, serve })
}

/// Без сети: поток симуляции здесь же, cfg.ticks тиков (проверка мира и записи).
pub fn run_offline(cfg: Config) -> Result<String, String> {
    let (_tx, rx) = mpsc::channel::<NetEv>();
    let (ready, _r) = mpsc::channel();
    sim_loop(cfg, rx, Arc::new(Mutex::new(String::new())), ready)
}

/// Остановлен ли поток симуляции сам (cfg.ticks).
impl Running {
    pub fn sim_done(&self) -> bool {
        self.sim.as_ref().is_none_or(|h| h.is_finished())
    }
}

/// Ждать сигнала остановки: Ctrl+C (SIGINT) и, на Linux, SIGTERM (`docker stop`, systemd). Дальше вызывающий зовёт
/// `Running::stop()` — тот останавливает сеть и пишет мир (последнее сохранение), потом процесс выходит сам.
pub async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(e) => {
                eprintln!("voxel-server: SIGTERM не ловится ({e}), только Ctrl+C");
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Чужой сайт не подключает браузер игрока (§6): Origin нет (бот) или localhost / 127.0.0.1 / из списка.
fn origin_ok(h: &HeaderMap, allow: &[String]) -> bool {
    let Some(o) = h.get("origin").and_then(|v| v.to_str().ok()) else { return true };
    let host = o.split("://").nth(1).unwrap_or(o).split(['/', ':']).next().unwrap_or("");
    host == "localhost" || host == "127.0.0.1" || allow.iter().any(|a| a == o || a == host)
}

async fn ws_upgrade(State(app): State<App>, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    if !origin_ok(&headers, &app.origins) {
        return (StatusCode::FORBIDDEN, "чужой Origin").into_response();
    }
    ws.max_message_size(MAX_FRAME).max_frame_size(MAX_FRAME).on_upgrade(move |s| conn_task(s, app))
}

async fn status_json(State(app): State<App>) -> Response {
    let body = app.status.lock().map(|s| s.clone()).unwrap_or_default();
    ([("content-type", "application/json; charset=utf-8"), ("access-control-allow-origin", "*")], body).into_response()
}

/// Соединение: читатель — кадры в поток симуляции; писатель — из очереди соединения в сокет.
async fn conn_task(socket: WebSocket, app: App) {
    let net = app.next.fetch_add(1, Ordering::Relaxed);
    let (mut tx, mut rx) = socket.split();
    let (out_tx, mut out_rx) = tmpsc::channel::<Out>(OUT_QUEUE);
    if app.to_sim.send(NetEv::Open { net, out: out_tx }).is_err() {
        return;
    }
    let writer = tokio::spawn(async move {
        while let Some(m) = out_rx.recv().await {
            match m {
                Out::Frame(b) => {
                    if tx.send(Message::Binary(b.into())).await.is_err() {
                        break;
                    }
                }
                Out::Close => {
                    let _ = tx.send(Message::Close(None)).await;
                    break;
                }
            }
        }
    });
    while let Some(Ok(m)) = rx.next().await {
        match m {
            Message::Binary(b) => {
                if app.to_sim.send(NetEv::Frame { net, bytes: b.to_vec() }).is_err() {
                    break;
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
    let _ = app.to_sim.send(NetEv::Close { net });
    let _ = writer.await;
}

/// Поток симуляции: Server создаётся здесь (Rc внутри), кадры — по приходу, тик — по часам.
fn sim_loop(cfg: Config, rx: mpsc::Receiver<NetEv>, status: Arc<Mutex<String>>, ready: mpsc::Sender<Result<u32, String>>) -> Result<String, String> {
    let mut server = match Server::open(cfg.seed, cfg.data.as_deref(), cfg.dev) {
        Ok(s) => s,
        Err(e) => {
            let _ = ready.send(Err(e.clone()));
            return Err(e);
        }
    };
    server.set_strict_nicks(true); // ник 3–16, латиница и кириллица (G2)
    server.set_autosave(cfg.autosave * 1000 / TICK.as_millis() as u64);
    if let Ok(mut s) = status.lock() {
        *s = status_text(&server.status(), 0.0, 0.0); // /status отвечает с первой секунды (сборка, мир), не «{}»
    }
    let _ = ready.send(Ok(server.seed()));
    let mut conns: HashMap<u64, (ConnId, tmpsc::Sender<Out>)> = HashMap::new();
    let mut by_conn: HashMap<ConnId, u64> = HashMap::new();
    // Когда от соединения приходил кадр в последний раз (тишина дольше cfg.idle — обрыв).
    let mut heard: HashMap<u64, Instant> = HashMap::new();
    let mut next = Instant::now() + TICK;
    let (mut ticks, mut busy, mut worst) = (0u64, Duration::ZERO, Duration::ZERO);
    let mut window = (0u64, Duration::ZERO);
    'run: loop {
        // Кадры до следующего тика (fast — без ожидания).
        loop {
            let wait = if cfg.fast { Duration::ZERO } else { next.saturating_duration_since(Instant::now()) };
            let ev = if wait.is_zero() { rx.try_recv().ok() } else { rx.recv_timeout(wait).ok() };
            let Some(ev) = ev else {
                if cfg.fast || Instant::now() >= next {
                    break;
                }
                continue;
            };
            match ev {
                NetEv::Open { net, out } => {
                    let c = server.open_conn();
                    conns.insert(net, (c, out));
                    by_conn.insert(c, net);
                    heard.insert(net, Instant::now());
                }
                NetEv::Frame { net, bytes } => {
                    if let Some(&(c, _)) = conns.get(&net) {
                        heard.insert(net, Instant::now());
                        server.frame(c, &bytes);
                    }
                }
                NetEv::Close { net } => {
                    heard.remove(&net);
                    if let Some((c, _)) = conns.remove(&net) {
                        by_conn.remove(&c);
                        server.close_conn(c);
                    }
                }
                NetEv::Stop => break 'run,
            }
            send_out(&mut server, &mut conns, &mut by_conn);
        }
        let t = Instant::now();
        server.tick();
        let dt = t.elapsed();
        busy += dt;
        worst = worst.max(dt);
        ticks += 1;
        send_out(&mut server, &mut conns, &mut by_conn);
        if ticks % 20 == 0 {
            let limit = Duration::from_secs(cfg.idle.max(1));
            let dead: Vec<u64> = heard.iter().filter(|(_, t)| t.elapsed() > limit).map(|(&n, _)| n).collect();
            for net in dead {
                heard.remove(&net);
                if let Some((c, tx)) = conns.remove(&net) {
                    eprintln!("voxel-server: соединение {net} молчит больше {} с — закрыто", limit.as_secs());
                    by_conn.remove(&c);
                    let _ = tx.try_send(Out::Close);
                    server.close_conn(c);
                }
            }
            let mspt = (busy - window.1).as_secs_f64() * 1e3 / (ticks - window.0) as f64;
            window = (ticks, busy);
            if let Ok(mut s) = status.lock() {
                *s = status_text(&server.status(), mspt, worst.as_secs_f64() * 1e3);
            }
        }
        if cfg.ticks > 0 && ticks >= cfg.ticks {
            break;
        }
        next += TICK;
        let now = Instant::now();
        if now > next + Duration::from_secs(2) {
            eprintln!("voxel-server: не успеваем — пропуск {:.1} с", (now - next).as_secs_f64());
            next = now;
        }
    }
    let mut out = format!(
        "тиков {ticks}, MSPT {:.3} (худший {:.3})",
        if ticks > 0 { busy.as_secs_f64() * 1e3 / ticks as f64 } else { 0.0 },
        worst.as_secs_f64() * 1e3
    );
    // Остановка: последняя запись (всё изменённое и все игроки) и дозапись очереди потока записи.
    if cfg.data.is_some() {
        out += &format!("; запись: {}", server.shutdown()?);
    }
    Ok(out)
}

/// Кадры соединениям; закрытые сервером — закрыть; очередь переполнена — отключить.
fn send_out(server: &mut Server, conns: &mut HashMap<u64, (ConnId, tmpsc::Sender<Out>)>, by_conn: &mut HashMap<ConnId, u64>) {
    let (frames, close) = server.take();
    let mut slow = Vec::new();
    for (c, b) in frames {
        let Some(net) = by_conn.get(&c) else { continue };
        let Some((_, tx)) = conns.get(net) else { continue };
        if tx.try_send(Out::Frame(b)).is_err() {
            slow.push(c);
        }
    }
    for c in close.into_iter().chain(slow) {
        if let Some(net) = by_conn.remove(&c) {
            if let Some((_, tx)) = conns.remove(&net) {
                let _ = tx.try_send(Out::Close);
            }
            server.close_conn(c);
        }
    }
}

/// /status: JSON без зависимостей (ники экранируются).
fn status_text(s: &Status, mspt: f64, worst: f64) -> String {
    let esc = |t: &str| {
        let mut o = String::new();
        for ch in t.chars() {
            match ch {
                '"' => o += "\\\"",
                '\\' => o += "\\\\",
                c if (c as u32) < 0x20 => o += &format!("\\u{:04x}", c as u32),
                c => o.push(c),
            }
        }
        o
    };
    let players: Vec<String> = s
        .players
        .iter()
        .map(|(n, rtt)| format!("{{\"nick\":\"{}\",\"rtt\":{}}}", esc(n), rtt.map_or("null".into(), |v| v.to_string())))
        .collect();
    format!(
        "{{\"time\":{},\"players\":[{}],\"items\":{},\"edits\":{},\"mspt\":{:.3},\"worst\":{:.3},\"corrections\":{},\"teleports\":{},\"rejected\":{},\"build\":\"{}\"}}",
        s.time,
        players.join(","),
        s.items,
        s.edits,
        mspt,
        worst,
        s.corrections,
        s.teleports,
        s.rejected,
        esc(s.build)
    )
}

// ---------------------------------------------------------------- клиент без браузера (бот, проверки)

/// Клиент по WebSocket: Peer тикает 20 Гц, перед каждым тиком script(peer, номер тика после входа) ставит ввод; кадры
/// сервера применяются по приходу (Pong — сразу). ticks — сколько тиков после входа (0 — пока соединение живо).
pub async fn run_peer(url: &str, seed: u32, nick: &str, ticks: u32, script: impl FnMut(&mut Peer, u32)) -> Result<Peer, String> {
    run_peer_as(url, seed, nick, [0; 16], ticks, script).await
}

/// То же со своим токеном (128 бит): ник закрепляется за ним, чужой токен получает отказ. Нули — гость.
pub async fn run_peer_as(url: &str, seed: u32, nick: &str, token: [u8; 16], ticks: u32, mut script: impl FnMut(&mut Peer, u32)) -> Result<Peer, String> {
    let (ws, _) = tokio_tungstenite::connect_async(url).await.map_err(|e| format!("{url}: {e}"))?;
    let (mut tx, mut rx) = ws.split();
    use tokio_tungstenite::tungstenite::Message as M;
    let mut peer = Peer::with_token(seed, nick, token);
    let mut iv = tokio::time::interval(TICK);
    iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut k = 0u32;
    loop {
        for f in peer.take() {
            tx.send(M::Binary(f.into())).await.map_err(|e| e.to_string())?;
        }
        if let Some(r) = peer.rejected() {
            return Err(r);
        }
        tokio::select! {
            m = rx.next() => match m {
                Some(Ok(M::Binary(b))) => peer.recv(&b)?,
                Some(Ok(M::Close(_))) | None => return if ticks == 0 { Ok(peer) } else { Err("соединение закрыто".into()) },
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(e.to_string()),
            },
            _ = iv.tick() => {
                if peer.ready() {
                    script(&mut peer, k);
                    peer.tick();
                    k += 1;
                    if ticks > 0 && k >= ticks {
                        for f in peer.take() {
                            tx.send(M::Binary(f.into())).await.map_err(|e| e.to_string())?;
                        }
                        let _ = tx.send(M::Close(None)).await;
                        return Ok(peer);
                    }
                }
            }
        }
    }
}
