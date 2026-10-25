//! Бот — клиент без браузера (MP-1): тот же ClientSim, что в браузере, по WebSocket. Для проверок без рук: второй
//! игрок ходит, ломает и ставит.
//!
//!   cargo run -p voxel-server --bin bot -- [ws://127.0.0.1:8121/ws] [--nick Bot] [--script circle|line|come|still|sneak|swing|sneakwalk|sprint|box|fight] [--look ЯУ,НАКЛОН]
//!                                          [--ticks N] [--seed N] [--reconnect] [--spawn X,Z | --at X,Y,Z] [--say ТЕКСТ]…
//! circle — круг 3 с (6°/тик), каждые 10 с — смотрит вниз-вперёд, ломает блок и ставит на его место землю (набор
//! отладки — нужен сервер с --dev); line — вперёд; come — 3 блока вперёд, разворот лицом к точке появления, машет
//! рукой (снимок модели); still — стоит. Строка состояния — раз в 5 с. --reconnect — после обрыва или отказа входит
//! снова через 2 с (проверка «сервер упал и вернулся»; ник тот же). --spawn / --at — сразу встать на землю у (X, Z) или
//! в точку (X, Y, Z) (нужен сервер с --dev): сцены «двое по сети» ставят бота перед камерой или под землёй.
//! box — стоит и смотрит (--at X,Y,Z и --look ЯУ,НАКЛОН — на сундук, печь или верстак), открывает окно ПКМ и без остановки
//! кладёт и берёт вещи случайными кликами (G8: двое у одного сундука, второй — вы в браузере; набор отладки — `--dev`).
//! fight — PvP-противник (G10): идёт к ближайшему игроку и бьёт ЛКМ (железный меч — нужен сервер с `--dev`), умер —
//! возрождается; свои сердца раз в секунду в stderr («bot: сердца 14.0»).
//! --say ТЕКСТ (можно несколько) — бот пишет в чат: первое через 3 с после входа, дальше раз в 4 с (текст «/…» — команда).
//! Все строки чата, что бот получает, и состав Tab печатаются в стандартный вывод ошибок («bot: чат: …»).

use voxel_core::net::Peer;
use voxel_core::{Hold, Press};
use voxel_server::run_peer;

struct Args {
    url: String,
    nick: String,
    script: String,
    ticks: u32,
    seed: u32,
    reconnect: bool,
    spawn: Option<[f64; 3]>,
    at: Option<[f64; 3]>,
    say: Vec<String>,
    look: Option<[f64; 2]>,
}

fn nums<const N: usize>(k: &str, v: &str) -> Result<[f64; N], String> {
    let p: Vec<f64> = v.split(',').map(|t| t.trim().parse::<f64>()).collect::<Result<_, _>>().map_err(|e| format!("{k}: {e}"))?;
    p.try_into().map_err(|_| format!("{k}: нужно {N} числа через запятую"))
}

fn args() -> Result<Args, String> {
    let mut a = Args { url: "ws://127.0.0.1:8121/ws".into(), nick: "Bot".into(), script: "circle".into(), ticks: 0, seed: 1337, reconnect: false, spawn: None, at: None, say: Vec::new(), look: None };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let mut val = || it.next().ok_or_else(|| format!("{k}: нет значения"));
        match k.as_str() {
            "--nick" => a.nick = val()?,
            "--script" => a.script = val()?,
            "--ticks" => a.ticks = val()?.parse().map_err(|e| format!("--ticks: {e}"))?,
            "--seed" => a.seed = val()?.parse().map_err(|e| format!("--seed: {e}"))?,
            "--reconnect" => a.reconnect = true,
            "--spawn" => {
                let [x, z] = nums::<2>("--spawn", &val()?)?;
                a.spawn = Some([x, 0.0, z]);
            }
            "--at" => a.at = Some(nums::<3>("--at", &val()?)?),
            "--say" => a.say.push(val()?),
            "--look" => a.look = Some(nums::<2>("--look", &val()?)?),
            u if u.starts_with("ws://") || u.starts_with("wss://") => a.url = u.into(),
            _ => return Err(format!("неизвестный ключ {k}")),
        }
    }
    Ok(a)
}

/// Сценарий: что нажато в тике k.
struct Script {
    kind: String,
    yaw: f32,
    /// Ломание: с какого тика, с какой целью.
    breaking: Option<(u32, [i32; 3])>,
    broke: u32,
    placed: u32,
    spawn: Option<[f64; 3]>,
    at: Option<[f64; 3]>,
    say: Vec<String>,
    /// Взгляд (yaw°, pitch°) для сценариев без своего: still, sneak, swing, sneakwalk, sprint.
    look: Option<[f64; 2]>,
    /// Сколько строк чата уже напечатано.
    chat_seen: u32,
    tab_seen: String,
    /// Случайные клики сценария box.
    rng: u64,
}

impl Script {
    fn rand(&mut self, n: u32) -> i32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng % n as u64) as i32
    }
    /// box: окно закрыто — ПКМ раз в 2 с; открыто — 1–2 случайных клика за тик (взять, положить, Shift, половина).
    fn boxed(&mut self, p: &mut Peer, k: u32) {
        p.set_keys(0);
        if !p.window_open() {
            if k % 40 == 20 {
                p.press(Press::Use as u32);
            }
            return;
        }
        for _ in 0..1 + self.rand(2) {
            let slot = self.rand(p.window_slots().max(1) as u32);
            match self.rand(10) {
                0..=5 => p.window_click(slot, self.rand(2), 0),
                6..=7 => p.window_click(slot, 0, 1),
                8 => p.window_click(slot, self.rand(9), 2),
                _ => p.window_click(slot, 0, 6),
            }
        }
    }

    fn step(&mut self, p: &mut Peer, k: u32) {
        // Чат: свои реплики по расписанию, чужие и системные — в вывод; Tab — когда состав или пинг изменились.
        if k >= 60 && (k - 60) % 80 == 0 {
            if let Some(t) = self.say.get(((k - 60) / 80) as usize) {
                p.chat_send(t);
            }
        }
        if p.chat_total() < self.chat_seen {
            self.chat_seen = 0;
        }
        for (kind, text) in p.chat_since(self.chat_seen) {
            eprintln!("bot: чат[{kind}]: {text}");
        }
        self.chat_seen = p.chat_total();
        if k % 50 == 0 {
            let tab = p.tab().iter().map(|(n, ping, you)| format!("{n}{} {ping} мс", if *you { " (я)" } else { "" })).collect::<Vec<_>>().join(", ");
            if tab != self.tab_seen {
                eprintln!("bot: Tab: {tab}");
                self.tab_seen = tab;
            }
        }
        if k == 0 && self.kind == "circle" {
            p.give(dirt(), 64);
        }
        if k == 0 && self.kind == "box" {
            p.give(dirt(), 200);
            p.give(voxel_core::api::item_id("stone"), 100);
        }
        if k == 0 {
            if let Some(s) = self.spawn {
                p.spawn(s[0], s[2]);
            }
            if let Some(t) = self.at {
                p.teleport(t);
            }
        }
        if let (Some([yaw, pitch]), "still" | "sneak" | "swing" | "sneakwalk" | "sprint" | "box") = (self.look, self.kind.as_str()) {
            p.set_look(yaw as f32, pitch as f32);
        }
        match self.kind.as_str() {
            "still" => p.set_keys(0),
            "box" => self.boxed(p, k),
            // Позы для снимков моделей: присед на месте; взмах каждые 12 тиков; бег вперёд с Shift (присед на ходу).
            "sneak" => p.set_keys(Hold::Sneak as u32),
            "swing" => {
                p.set_keys(0);
                if k % 12 == 0 {
                    p.click();
                }
            }
            "sneakwalk" => p.set_keys(Hold::Sneak as u32 | Hold::Fwd as u32),
            "sprint" => p.set_keys(Hold::Fwd as u32 | Hold::Sprint as u32),
            "fight" => self.fight(p, k),
            "line" => p.set_keys(Hold::Fwd as u32),
            "come" => {
                p.set_look(if k < 14 { 0.0 } else { 180.0 }, 0.0);
                p.set_keys(if k < 14 { Hold::Fwd as u32 } else { 0 });
                if k >= 14 && k % 20 == 0 {
                    p.click(); // взмах в воздух
                }
            }
            _ => self.circle(p, k),
        }
        if k % 100 == 0 {
            let q = p.pos();
            let others: Vec<String> = p.remotes().iter().map(|(_, n, r)| format!("{n} ({:.1}, {:.1}, {:.1})", r[0], r[1], r[2])).collect();
            let st = p.stats();
            eprintln!(
                "bot: тик {k}, ступни ({:.1}, {:.1}, {:.1}), сломал {}, поставил {}, откатов {}, телепортов {}; рядом: {}",
                q[0],
                q[1],
                q[2],
                self.broke,
                self.placed,
                st[0],
                st[1],
                if others.is_empty() { "никого".into() } else { others.join(", ") }
            );
        }
    }
    /// fight (G10, PvP): идёт к ближайшему игроку, смотрит ему в грудь и бьёт ЛКМ раз в 12 тиков (перезарядка меча); с
    /// набором отладки (`--dev`) берёт железный меч; умер — возрождается. Сердца — в вывод раз в секунду.
    fn fight(&mut self, p: &mut Peer, k: u32) {
        if k == 0 {
            p.give(voxel_core::api::item_id("iron_sword"), 1);
        }
        let (hp, dead) = p.hp();
        if k % 20 == 0 {
            eprintln!("bot: сердца {hp:.1}{}", if dead { " (мёртв)" } else { "" });
        }
        if dead {
            p.set_keys(0);
            p.respawn();
            return;
        }
        let me = p.pos();
        let Some((_, nick, to)) = p.remotes().into_iter().min_by(|a, b| dist2(me, a.2).total_cmp(&dist2(me, b.2))) else {
            p.set_keys(0);
            return;
        };
        let (dx, dy, dz) = (to[0] - me[0], to[1] + 1.0 - (me[1] + 1.62), to[2] - me[2]);
        let flat = (dx * dx + dz * dz).sqrt();
        p.set_look((-dx).atan2(dz).to_degrees() as f32, (-dy).atan2(flat).to_degrees() as f32);
        p.set_keys(if flat > 2.0 { Hold::Fwd as u32 } else { 0 });
        if flat < 3.0 && k % 12 == 0 {
            p.click();
            if k % 60 == 0 {
                eprintln!("bot: бью {nick}");
            }
        }
    }
    fn circle(&mut self, p: &mut Peer, k: u32) {
        if let Some((from, pos)) = self.breaking {
            p.set_look(self.yaw, 50.0);
            if p.block(pos[0], pos[1], pos[2]) == 0 {
                // Сломан — поставить землю на место (ПКМ по блоку под ним).
                self.breaking = None;
                self.broke += 1;
                p.set_keys(0);
                p.press(Press::Use as u32);
                self.placed += 1;
            } else if k - from > 120 {
                self.breaking = None; // слишком твёрдо — дальше
                p.set_keys(0);
            } else {
                p.set_keys(Hold::Attack as u32);
            }
            return;
        }
        if k % 200 >= 198 {
            // Прицелиться (луч цели считается в тике) и на следующем тике начать.
            p.set_look(self.yaw, 50.0);
            p.set_keys(0);
            if k % 200 == 199 {
                if let Some(t) = p.target() {
                    self.breaking = Some((k, t));
                    p.click();
                }
            }
            return;
        }
        self.yaw += 6.0;
        p.set_look(self.yaw, 0.0);
        p.set_keys(Hold::Fwd as u32 | if k % 40 == 0 { Hold::Jump as u32 } else { 0 });
    }
}

fn dist2(a: [f64; 3], b: [f64; 3]) -> f64 {
    (0..3).map(|i| (a[i] - b[i]).powi(2)).sum()
}

fn dirt() -> u16 {
    voxel_core::api::item_id("dirt")
}

fn main() {
    let a = match args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tokio");
    eprintln!("bot: {} → {} ({})", a.nick, a.url, a.script);
    loop {
        let mut s = Script { kind: a.script.clone(), yaw: 0.0, breaking: None, broke: 0, placed: 0, spawn: a.spawn, at: a.at, say: a.say.clone(), look: a.look, chat_seen: 0, tab_seen: String::new(), rng: 0x9E37_79B9_7F4A_7C15 };
        let r = rt.block_on(run_peer(&a.url, a.seed, &a.nick, a.ticks, |p, k| s.step(p, k)));
        // Без --ticks бот работает, пока жива связь: закрытие сервером — обрыв, а не «готово».
        let r = if a.reconnect && a.ticks == 0 { r.and_then(|_| Err("сервер закрыл соединение".to_string())) } else { r };
        match r {
            Ok(p) => {
                let st = p.stats();
                eprintln!("bot: готово — сломал {}, поставил {}, откатов {}, телепортов {}", s.broke, s.placed, st[0], st[1]);
                break;
            }
            Err(e) if a.reconnect => {
                eprintln!("bot: {e} — снова через 2 с");
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
            Err(e) => {
                eprintln!("bot: {e}");
                std::process::exit(1);
            }
        }
    }
}
