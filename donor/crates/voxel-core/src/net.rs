//! Сеть на байтах (MP-1): обёртки ядра для нативного сервера и клиентов без браузера. Сокеты, потоки и время — у
//! вызывающего (crates/voxel-server: tokio + axum); здесь — соединения, разбор и сборка кадров (proto.rs), вход,
//! выход, тик.
//!   Server — ServerSim и соединения: open / frame / close / tick / take. Создаётся в потоке симуляции (держит Rc).
//!           Хранение (MP-2, design.md §2.5): open грузит мир и всех игроков из папки; каждые AUTOSAVE_TICKS тиков (30 с)
//!           тик собирает пачку изменённого, а пишет её отдельный поток (save/writer.rs); вход и выход игрока пишут его
//!           запись сразу; shutdown (SIGTERM, остановка) — последняя запись и дозапись очереди.
//!           Админка (G17, game/server/admin.rs): роли — файл roles.txt в папке мира (читается при открытии и при
//!           изменении на диске, пишется после /op, /ban, /mute), журнал админ-команд — admin.log там же (время UTC),
//!           /kick и /ban закрывают соединение, /save — запись мира.
//!   Peer   — клиент (ClientSim) со своим миром: бот и проверки (те же правила, что у браузера, — тот же код).

use std::collections::BTreeMap;

use crate::abi::{K_LOCKED, P_ATTACK};
use crate::game::client::Client;
use crate::game::server::ServerSim;
use crate::proto::{decode, encode, hello_head, outdated, Reject, C2S, S2C};
use crate::world::World;

/// Автосохранение: раз в столько тиков (30 с при 20 Гц).
pub const AUTOSAVE_TICKS: u64 = 600;

/// Номер сборки (git rev; «dev» — неизвестен): /status и журнал сервера, сверка при выкладке.
pub fn build() -> &'static str {
    crate::proto::BUILD
}

/// Номер соединения.
pub type ConnId = u64;

struct Conn {
    player: Option<u32>,
    out: Vec<Vec<u8>>,
    /// Закрыть после отправки (отказ во входе, битый кадр).
    close: bool,
}

/// Состояние сервера для /status и журнала.
#[derive(Clone, Debug, Default)]
pub struct Status {
    pub time: u64,
    pub players: Vec<(String, Option<u32>)>,
    pub items: usize,
    pub edits: usize,
    pub corrections: u32,
    pub teleports: u32,
    pub rejected: u32,
    pub build: &'static str,
}

/// Хранилище сервера: мир в папке. Чтение (list) — отсюда, запись — потоком записи; Saver помнит, что уже лежит.
#[cfg(not(target_arch = "wasm32"))]
struct Store {
    fs: crate::save::storage::FileStorage,
    saver: crate::save::Saver,
    writer: crate::save::writer::Writer,
    /// Автосохранение раз в every тиков (0 — выключено), прошло с прошлого.
    every: u64,
    since: u64,
    /// Папка мира (roles.txt, admin.log) и время изменения roles.txt, каким его прочли или записали.
    dir: std::path::PathBuf,
    roles_mtime: Option<std::time::SystemTime>,
}

/// Игровой сервер на байтах.
pub struct Server {
    pub(crate) sim: ServerSim,
    conns: BTreeMap<ConnId, Conn>,
    next: ConnId,
    #[cfg(not(target_arch = "wasm32"))]
    t0: std::time::Instant,
    #[cfg(not(target_arch = "wasm32"))]
    store: Option<Store>,
}

impl Server {
    /// Новый мир с сидом (debug — разрешить команды отладки: полёт, набор, сдвиг, время).
    pub fn new(seed: u32, debug: bool) -> Server {
        let mut sim = ServerSim::new(World::new(seed));
        sim.debug = debug;
        Server::with(sim)
    }

    fn with(mut sim: ServerSim) -> Server {
        sim.raise_crossroads();
        sim.ping = true;
        sim.name = "voxel-server".into();
        Server {
            sim,
            conns: BTreeMap::new(),
            next: 1,
            #[cfg(not(target_arch = "wasm32"))]
            t0: std::time::Instant::now(),
            #[cfg(not(target_arch = "wasm32"))]
            store: None,
        }
    }

    /// Мир из папки data: мир и все записи игроков (нет мира — новый с сидом seed; у существующего сид свой, из meta).
    /// Запись — автосохранение в tick, shutdown, save.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open(seed: u32, data: Option<&str>, debug: bool) -> Result<Server, String> {
        use crate::save::{storage::FileStorage, writer::Writer, SaveError, Saver};
        let Some(dir) = data else { return Ok(Server::new(seed, debug)) };
        let mut fs = FileStorage::open(std::path::Path::new(dir)).map_err(|e| e.to_string())?;
        let (mut sim, saver) = match Saver::load_all(&mut fs) {
            Ok(x) => x,
            Err(SaveError::NoWorld) => (ServerSim::new(World::new(seed)), Saver::new()),
            Err(e) => return Err(e.to_string()),
        };
        for u in &saver.unreadable {
            eprintln!("voxel-server: запись игрока пропущена, файл не тронут: {u}");
        }
        sim.debug = debug;
        let writer = Writer::spawn(fs.clone()).map_err(|e| format!("поток записи: {e}"))?;
        let fresh = saver.is_fresh();
        let dir = std::path::PathBuf::from(dir);
        let (roles, roles_mtime) = read_roles(&dir).map_err(|e| format!("{}: {e}", ROLES_FILE))?;
        sim.admin.roles = roles;
        let mut s = Server::with(sim);
        s.store = Some(Store { fs, saver, writer, every: AUTOSAVE_TICKS, since: 0, dir, roles_mtime });
        if fresh {
            // Новый мир ложится на диск сразу (meta): иначе записи игроков до первого автосохранения «без мира» не прочтутся.
            s.store_batch(true, true)?;
        }
        Ok(s)
    }

    /// Правило ников сетевого сервера (G2): 3–16 знаков, латиница, кириллица, цифры, «_» (`social::nick_ok`). Включает
    /// voxel-server; проверки ядра с ником «A» работают без него.
    pub fn set_strict_nicks(&mut self, on: bool) {
        self.sim.strict_nicks = on;
    }

    /// Автосохранение раз в ticks тиков (0 — выключить; по умолчанию AUTOSAVE_TICKS). Без папки мира — ничего.
    pub fn set_autosave(&mut self, ticks: u64) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(st) = self.store.as_mut() {
            st.every = ticks;
        }
        #[cfg(target_arch = "wasm32")]
        let _ = ticks;
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn clock(&mut self) {
        self.sim.now_ms = self.t0.elapsed().as_millis() as u64;
    }
    #[cfg(target_arch = "wasm32")]
    fn clock(&mut self) {}

    /// Новое соединение (до Hello — без игрока).
    pub fn open_conn(&mut self) -> ConnId {
        let id = self.next;
        self.next += 1;
        self.conns.insert(id, Conn { player: None, out: Vec::new(), close: false });
        id
    }

    /// Кадр от соединения: Hello — вход (отказ — Reject и закрыть), дальше — сообщения игрока; битый кадр — закрыть.
    pub fn frame(&mut self, c: ConnId, b: &[u8]) {
        self.clock();
        let Some(conn) = self.conns.get_mut(&c) else { return };
        // Клиент другой сборки узнаётся по началу Hello, даже если остальное в кадре уже не разбирается: ему — «обновите
        // страницу», а не «пакет не разобрался».
        if conn.player.is_none() {
            if let Some(text) = hello_head(b).and_then(|(proto, build)| outdated(proto, &build)) {
                conn.out.push(encode(&S2C::Reject { code: Reject::Outdated, text }));
                conn.close = true;
                return;
            }
        }
        let m: C2S = match decode(b) {
            Ok(m) => m,
            Err(e) => {
                conn.out.push(encode(&S2C::Kick { text: e.to_string() }));
                conn.close = true;
                return;
            }
        };
        match (conn.player, m) {
            (None, C2S::Hello(h)) => self.hello(c, h),
            // Действия между тиками — только у локального сервера (проверки); по сети — внутри Tick.
            (Some(_), C2S::Acts(_)) => {}
            (Some(id), m) => self.sim.handle(id, m),
            (None, _) => {}
        }
        self.admin_io();
    }

    /// Владелец без файла ролей (проверки, сервер в памяти): ник получает права владельца, пока сервер работает.
    pub fn add_owner(&mut self, nick: &str) {
        self.sim.admin.roles.owners.insert(nick.to_lowercase());
    }

    /// После кадра и тика (G17): выкинуть (/kick, /ban) — Kick и закрыть соединение; натив — журнал админ-команд,
    /// файл ролей, /save, перечитать roles.txt, если его поправили руками.
    fn admin_io(&mut self) {
        for (id, text) in std::mem::take(&mut self.sim.admin.kicks) {
            if let Some(conn) = self.conns.values_mut().find(|k| k.player == Some(id)) {
                conn.out.push(encode(&S2C::Kick { text }));
                conn.close = true;
                conn.player = None;
            }
            self.leave(id);
        }
        #[cfg(not(target_arch = "wasm32"))]
        self.admin_files();
        #[cfg(target_arch = "wasm32")]
        self.sim.admin.log.clear();
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn admin_files(&mut self) {
        let lines = std::mem::take(&mut self.sim.admin.log);
        for l in &lines {
            eprintln!("voxel-server: админ: {l}");
        }
        let Some(st) = self.store.as_mut() else {
            self.sim.admin.roles_dirty = false;
            self.sim.admin.save_req = false;
            return;
        };
        if !lines.is_empty() {
            use std::io::Write;
            let now = utc_now();
            let text: String = lines.iter().map(|l| format!("{now} {l}\n")).collect();
            let r = std::fs::OpenOptions::new().create(true).append(true).open(st.dir.join(ADMIN_LOG)).and_then(|mut f| f.write_all(text.as_bytes()));
            if let Err(e) = r {
                eprintln!("voxel-server: {ADMIN_LOG}: {e}");
            }
        }
        if std::mem::take(&mut self.sim.admin.roles_dirty) {
            match write_roles(&st.dir, &self.sim.admin.roles) {
                Ok(t) => st.roles_mtime = t,
                Err(e) => eprintln!("voxel-server: {ROLES_FILE}: {e}"),
            }
        }
        if std::mem::take(&mut self.sim.admin.save_req) {
            if let Err(e) = self.store_batch(false, false) {
                eprintln!("voxel-server: /save: {e}");
            }
        }
    }

    /// roles.txt поправили руками (владелец на VPS) — перечитать: раз в ROLES_CHECK тиков по времени изменения.
    #[cfg(not(target_arch = "wasm32"))]
    fn roles_reload(&mut self) {
        let Some(st) = self.store.as_mut() else { return };
        if self.sim.sim.time % ROLES_CHECK != 0 {
            return;
        }
        let m = std::fs::metadata(st.dir.join(ROLES_FILE)).and_then(|m| m.modified()).ok();
        if m == st.roles_mtime {
            return;
        }
        match read_roles(&st.dir) {
            Ok((r, t)) => {
                st.roles_mtime = t;
                eprintln!("voxel-server: {ROLES_FILE} перечитан: владельцев {}, админов {}, банов {}", r.owners.len(), r.admins.len(), r.bans.len());
                self.sim.admin.roles = r;
            }
            Err(e) => eprintln!("voxel-server: {ROLES_FILE}: {e}"),
        }
    }

    /// Вход соединения c: отказ — Reject и закрыть; тот же игрок (ник и токен) уже вошёл с другого соединения (обрыв,
    /// который сервер ещё не заметил, перезагрузка вкладки) — старое закрывается, новое входит на его место.
    fn hello(&mut self, c: ConnId, h: crate::proto::Hello) {
        if outdated(h.proto, &h.build).is_none() {
            if let Some(old) = self.sim.same_session(&h) {
                if let Some(oc) = self.conns.iter().find(|(_, k)| k.player == Some(old)).map(|(&k, _)| k) {
                    let o = self.conns.get_mut(&oc).expect("соединение есть");
                    o.out.push(encode(&S2C::Kick { text: "Вы вошли с другого окна или устройства.".into() }));
                    o.close = true;
                    o.player = None;
                }
                self.leave(old);
                self.sim.forget_for_others(old);
            }
        }
        let conn = self.conns.get_mut(&c).expect("соединение есть");
        match self.sim.join(&h) {
            Ok(id) => {
                conn.player = Some(id);
                self.save_player(&h.nick);
            }
            Err((code, text)) => {
                conn.out.push(encode(&S2C::Reject { code, text }));
                conn.close = true;
            }
        }
    }

    /// Игрок id выходит: сервер убирает его из мира, запись — сразу (инвентарь и позиция не ждут автосохранения).
    fn leave(&mut self, id: u32) {
        let nick = self.sim.player(id).map(|p| p.nick.clone());
        self.sim.leave(id);
        if let Some(n) = nick {
            self.save_player(&n);
        }
    }

    /// Соединение закрылось: игрок выходит.
    pub fn close_conn(&mut self, c: ConnId) {
        if let Some(conn) = self.conns.remove(&c) {
            if let Some(id) = conn.player {
                self.leave(id);
            }
        }
    }

    /// Тик сервера (20 Гц у вызывающего); раз в autosave тиков — автосохранение.
    pub fn tick(&mut self) {
        self.clock();
        #[cfg(not(target_arch = "wasm32"))]
        let t0 = std::time::Instant::now();
        self.sim.tick();
        #[cfg(not(target_arch = "wasm32"))]
        {
            // MSPT для /tps: сглаженное время тика симуляции.
            let ms = t0.elapsed().as_secs_f32() * 1000.0;
            let a = &mut self.sim.admin;
            a.mspt = if a.mspt == 0.0 { ms } else { a.mspt * 0.95 + ms * 0.05 };
            self.persist();
            self.roles_reload();
        }
        self.admin_io();
    }

    /// Кадры к отправке: (соединение, байты) по порядку; и соединения, которые после них закрыть.
    pub fn take(&mut self) -> (Vec<(ConnId, Vec<u8>)>, Vec<ConnId>) {
        let mut out = Vec::new();
        let mut close = Vec::new();
        for (&c, conn) in &mut self.conns {
            out.extend(conn.out.drain(..).map(|b| (c, b)));
            if let Some(p) = conn.player.and_then(|id| self.sim.player_mut(id)) {
                out.extend(p.outbox.drain(..).map(|m| (c, encode(&m))));
            }
            if conn.close {
                close.push(c);
            }
        }
        for c in &close {
            self.conns.remove(c);
        }
        (out, close)
    }

    pub fn status(&self) -> Status {
        let s = &self.sim;
        Status {
            time: s.sim.time,
            players: s.players.iter().map(|p| (p.nick.clone(), p.rtt_ms)).collect(),
            items: s.sim.items.list.len(),
            edits: s.sim.world.d.borrow().edits_len(),
            corrections: s.stats.corrections,
            teleports: s.stats.teleports,
            rejected: s.stats.rejected,
            build: crate::proto::BUILD,
        }
    }

    pub fn seed(&self) -> u32 {
        self.sim.sim.world.seed()
    }

    // ------------------------------------------------------------ хранение (MP-2)

    /// Запись игрока nick (вход закрепил токен, выход унёс вещи) — потоку записи сразу, не ждать автосохранения.
    fn save_player(&mut self, nick: &str) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let Server { sim, store, .. } = self;
            let Some(st) = store.as_mut() else { return };
            match st.saver.collect_players(sim, &[nick.to_string()]) {
                Ok(b) => {
                    if !st.writer.submit(b) {
                        st.saver.failed();
                        eprintln!("voxel-server: поток записи не принимает пачку, следующая запись будет полной");
                    }
                }
                Err(e) => eprintln!("voxel-server: запись игрока «{nick}»: {e}"),
            }
        }
        #[cfg(target_arch = "wasm32")]
        let _ = nick;
    }

    /// После тика: ошибки потока записи — в журнал и «пишем всё заново»; пора — автосохранение.
    #[cfg(not(target_arch = "wasm32"))]
    fn persist(&mut self) {
        let Some(st) = self.store.as_mut() else { return };
        let errs = st.writer.take_errors();
        for e in &errs {
            eprintln!("voxel-server: {e}; следующая запись будет полной");
        }
        if !errs.is_empty() {
            st.saver.failed();
        }
        if st.every == 0 {
            return;
        }
        st.since += 1;
        if st.since >= st.every {
            st.since = 0;
            if let Err(e) = self.store_batch(false, false) {
                eprintln!("voxel-server: автосохранение: {e}");
            }
        }
    }

    /// Собрать пачку и отдать потоку записи; wait — дождаться записи и вернуть итог. full — все куски мира (иначе —
    /// изменённое; после неудачной записи — тоже всё).
    #[cfg(not(target_arch = "wasm32"))]
    fn store_batch(&mut self, full: bool, wait: bool) -> Result<String, String> {
        let Server { sim, store, .. } = self;
        let Some(st) = store.as_mut() else { return Ok("хранилища нет".into()) };
        let everything = st.saver.take_full() || full;
        let batch = if everything {
            // Список кусков хранилища читается, пока поток записи молчит: иначе увидим середину прошлой пачки.
            if !st.writer.sync() {
                return Err("поток записи не отвечает".into());
            }
            st.saver.collect_full(sim, &st.fs)
        } else {
            st.saver.collect_dirty(sim)
        }
        .map_err(|e| e.to_string())?;
        let report = batch.report();
        if !st.writer.submit(batch) {
            st.saver.failed();
            return Err("поток записи не принимает пачку".into());
        }
        if wait {
            if !st.writer.sync() {
                st.saver.failed();
                return Err("поток записи не ответил".into());
            }
            if let Some(e) = st.writer.take_errors().into_iter().next() {
                st.saver.failed();
                return Err(e);
            }
        }
        Ok(format!("{report:?}"))
    }

    /// Дождаться записи всего, что уже отдано потоку записи (проверки, команда `save`).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn sync_writes(&mut self) -> Result<(), String> {
        let Some(st) = self.store.as_mut() else { return Ok(()) };
        if !st.writer.sync() {
            return Err("поток записи не отвечает".into());
        }
        match st.writer.take_errors().into_iter().next() {
            Some(e) => {
                st.saver.failed();
                Err(e)
            }
            None => Ok(()),
        }
    }

    /// Записать изменённое и дождаться (нет папки — ничего).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn save(&mut self) -> Result<String, String> {
        self.store_batch(false, true)
    }

    /// Записать весь мир заново и дождаться: куски, которых в мире больше нет, удаляются (команда `save` админа,
    /// починка после сбоя записи).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn save_all(&mut self) -> Result<String, String> {
        self.store_batch(true, true)
    }

    /// Остановка (SIGTERM, Ctrl+C, конец проверки): последняя запись — всё, что изменилось, и все игроки — и дозапись
    /// очереди; поток записи закрывается. Дальше сервер только без папки.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn shutdown(&mut self) -> Result<String, String> {
        if self.store.is_none() {
            return Ok("хранилища нет".into());
        }
        let r = self.store_batch(false, true);
        let st = self.store.take().expect("хранилище есть");
        let late = st.writer.finish();
        match (r, late.into_iter().next()) {
            (Ok(s), None) => Ok(s),
            (Ok(_), Some(e)) | (Err(e), _) => Err(e),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
use crate::game::server::admin::{Roles, ADMIN_LOG, ROLES_FILE};

/// Проверять, не поправили ли roles.txt руками, раз в столько тиков (5 с).
#[cfg(not(target_arch = "wasm32"))]
const ROLES_CHECK: u64 = 100;

/// Прочитать roles.txt папки dir (нет файла — пусто) и время его изменения.
#[cfg(not(target_arch = "wasm32"))]
fn read_roles(dir: &std::path::Path) -> std::io::Result<(Roles, Option<std::time::SystemTime>)> {
    let path = dir.join(ROLES_FILE);
    match std::fs::read_to_string(&path) {
        Ok(t) => Ok((Roles::parse(&t), std::fs::metadata(&path).and_then(|m| m.modified()).ok())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((Roles::default(), None)),
        Err(e) => Err(e),
    }
}

/// Записать roles.txt атомарно (временный файл и rename); ответ — новое время изменения.
#[cfg(not(target_arch = "wasm32"))]
fn write_roles(dir: &std::path::Path, r: &Roles) -> std::io::Result<Option<std::time::SystemTime>> {
    let path = dir.join(ROLES_FILE);
    let tmp = dir.join(format!("{ROLES_FILE}.tmp"));
    std::fs::write(&tmp, r.to_text())?;
    std::fs::rename(&tmp, &path)?;
    Ok(std::fs::metadata(&path).and_then(|m| m.modified()).ok())
}

/// Назначить владельца ника в roles.txt папки мира dir (voxel-server --set-owner): остальное в файле сохраняется.
/// Работающий сервер подхватит правку сам (ROLES_CHECK).
#[cfg(not(target_arch = "wasm32"))]
pub fn set_owner(dir: &str, nick: &str) -> Result<String, String> {
    let nick = nick.trim();
    if nick.is_empty() || nick.contains(char::is_whitespace) {
        return Err("ник без пробелов".into());
    }
    let dir = std::path::Path::new(dir);
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let (mut r, _) = read_roles(dir).map_err(|e| format!("{ROLES_FILE}: {e}"))?;
    r.owners.insert(nick.to_lowercase());
    write_roles(dir, &r).map_err(|e| format!("{ROLES_FILE}: {e}"))?;
    Ok(format!("владельцы: {}", r.owners.iter().cloned().collect::<Vec<_>>().join(", ")))
}

/// Время UTC для журнала: «2026-10-23T18:04:05Z» (без зависимостей: дни → дата по Говарду Хиннанту).
#[cfg(not(target_arch = "wasm32"))]
fn utc_now() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
    let (days, rem) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + (m <= 2) as i64;
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem / 60 % 60, rem % 60)
}

/// Клиент без браузера (бот, проверки): свой мир сида seed, ник; кадры — байтами.
pub struct Peer {
    pub(crate) client: Client,
    pub(crate) world: World,
}

impl Peer {
    pub fn new(seed: u32, nick: &str) -> Peer {
        Peer::with_token(seed, nick, [0; 16])
    }
    /// Со своим токеном (128 бит из localStorage браузера): ник закрепляется за ним. Нули — гость.
    pub fn with_token(seed: u32, nick: &str, token: [u8; 16]) -> Peer {
        let world = World::new(seed);
        Peer { client: Client::with_token(&world, nick, token), world }
    }
    /// Кадр сервера. Ошибка разбора — текст.
    pub fn recv(&mut self, b: &[u8]) -> Result<(), String> {
        self.client.recv(b).map_err(|e| e.to_string())
    }
    /// Накопленные кадры серверу.
    pub fn take(&mut self) -> Vec<Vec<u8>> {
        self.client.take_outbox()
    }
    /// Тик клиента (20 Гц у вызывающего): предсказанный тик и «клиентская» фаза.
    pub fn tick(&mut self) {
        self.client.sim.tick();
        self.client.sim.tick_visual();
    }
    /// Вошёл и готов тикать (правки вокруг пришли).
    pub fn ready(&self) -> bool {
        self.client.sim.ready()
    }
    /// Номер своей сущности (0 — не вошёл).
    pub fn id(&self) -> u32 {
        self.client.sim.me
    }
    /// Отказ во входе или выброс: текст.
    pub fn rejected(&self) -> Option<String> {
        self.client.sim.stats.rejected.clone()
    }
    /// Удержания (маска abi::Hold) — к ним всегда добавляется «курсор захвачен».
    pub fn set_keys(&mut self, mask: u32) {
        self.client.sim.set_keys(mask | K_LOCKED);
    }
    /// Нажатие (abi::Press).
    pub fn press(&mut self, code: u32) {
        self.client.sim.press(code);
    }
    /// Клик ЛКМ (начать ломание).
    pub fn click(&mut self) {
        self.client.sim.press(P_ATTACK);
    }
    pub fn set_look(&mut self, yaw: f32, pitch: f32) {
        self.client.sim.set_look(yaw, pitch);
    }
    /// Ступни.
    pub fn pos(&self) -> [f64; 3] {
        self.client.sim.ctl.player.pos
    }
    /// Сдвиг тела в обход физики (проверки: «читер»).
    pub fn cheat_move(&mut self, d: [f64; 3]) {
        let p = &mut self.client.sim.ctl.player;
        for k in 0..3 {
            p.pos[k] += d[k];
        }
    }
    /// Блок в мире клиента.
    pub fn block(&self, x: i32, y: i32, z: i32) -> u16 {
        self.world.get_block(x, y, z)
    }
    /// Цель взгляда (клетка) после последнего тика.
    pub fn target(&self) -> Option<[i32; 3]> {
        self.client.sim.ctl.target.map(|h| h.pos)
    }
    /// Другие игроки: (номер, ник, ступни в момент отрисовки на конец этого тика).
    pub fn remotes(&self) -> Vec<(u32, String, [f64; 3])> {
        self.client.sim.remotes.iter().map(|(&id, r)| (id, r.nick.clone(), r.at(r.t).0)).collect()
    }
    /// Tab (G2): (ник, пинг мс — −1 не измерен, это вы) по нику без учёта регистра.
    pub fn tab(&self) -> Vec<(String, i32, bool)> {
        let (list, names) = (self.client.sim.tab_list(), self.client.sim.tab_names());
        names.split('\n').zip(list.chunks(4)).filter(|(n, _)| !n.is_empty()).map(|(n, c)| (n.to_string(), c[1], c[2] != 0)).collect()
    }
    /// Сколько строк чата пришло с начала сеанса.
    pub fn chat_total(&self) -> u32 {
        self.client.sim.chat_total()
    }
    /// Строки чата после первых seen: (вид proto::CHAT_*, текст).
    pub fn chat_since(&self, seen: u32) -> Vec<(u8, String)> {
        let text = self.client.sim.chat_since(seen);
        text.split('\n').filter_map(|l| l.split_once('|')).map(|(k, t)| (k.parse().unwrap_or(0), t.to_string())).collect()
    }
    /// Отправить сообщение в чат.
    pub fn chat_send(&mut self, text: &str) {
        self.client.sim.chat_send(text)
    }
    /// Предметов вокруг (зеркало сервера).
    pub fn items(&self) -> usize {
        self.client.sim.items.list.len()
    }
    /// Сколько штук предмета item в инвентаре.
    pub fn count(&self, item: u16) -> i32 {
        (0..=40).map(|i| self.client.sim.ctl.inv.get(i)).filter(|s| s.id() == item).map(|s| s.n()).sum()
    }
    /// [откатов предсказаний, телепортов от сервера, кадров с сервера].
    pub fn stats(&self) -> [u32; 3] {
        let s = &self.client.sim.stats;
        [s.rollbacks, s.teleports, s.frames_in]
    }
    /// Вкладка скрыта или меню заморозило кадры (on) / игрок вернулся (!on): сервер тикает его сам; после возврата тики
    /// стоят, пока не придёт Resume. Тики на время «ухода» вызывающий не вызывает (как браузер без кадров).
    pub fn set_away(&mut self, on: bool) {
        self.client.sim.set_away(on);
    }
    /// Радиус правок мира в кусках («блоки до»): при смене — C2S::View; сервер ограничивает своим VIEW_MAX.
    pub fn set_view(&mut self, radius: u8) {
        self.client.sim.set_view(radius);
    }
    /// Сколько раз сервер вернул игрока после ухода (Resume).
    pub fn resumes(&self) -> u32 {
        self.client.sim.stats.resumes
    }
    /// Положить предмет (команда отладки; сервер — с --dev).
    pub fn give(&mut self, item: u16, count: i32) {
        self.client.sim.give(item, count);
    }
    /// Переставить (команда отладки).
    pub fn teleport(&mut self, p: [f64; 3]) {
        self.client.sim.teleport(p[0], p[1], p[2]);
    }
    /// Поставить на землю у (x, z) (команда отладки; сервер — с --dev).
    pub fn spawn(&mut self, x: f64, z: f64) {
        self.client.sim.spawn(x, z);
    }
    /// Окно (сундук, печь, верстак, машина) открыто.
    pub fn window_open(&self) -> bool {
        self.client.sim.ctl.screen.is_some()
    }
    /// Число слотов открытого окна (0 — окна нет).
    pub fn window_slots(&self) -> usize {
        self.client.sim.ctl.screen.as_ref().map_or(0, |m| m.slots.len())
    }
    /// Клик по слоту окна (то же, что делает screen.js): слот, кнопка, действие (abi::SlotAction: 0 — взять / положить,
    /// 1 — Shift, 2 — число, 4 — Q, 6 — двойной).
    pub fn window_click(&mut self, slot: i32, button: i32, action: i32) {
        self.client.sim.screen_click(slot, button, action)
    }
    /// Закрыть окно (Esc).
    pub fn window_close(&mut self) {
        self.client.sim.close_screen()
    }
    /// Свои сердца (20 — полные) и «мёртв» (Msg::Vitals).
    pub fn hp(&self) -> (f32, bool) {
        let v = &self.client.sim.vit;
        (v.hp, v.dead)
    }
    /// Кнопка возрождения (работает с 20-го тика смерти).
    pub fn respawn(&mut self) {
        self.client.sim.respawn();
    }
    /// Отладочный полёт.
    pub fn set_flying(&mut self, on: bool) {
        self.client.sim.set_flying(on);
    }
}
