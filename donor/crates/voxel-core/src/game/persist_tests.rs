//! Проверки постоянного мира и входа (N1c, mp/design.md §2.5, §3.2, §3.5, §4.6): сервер (net::Server) над папкой мира
//! (FileStorage) и клиенты (net::Peer) без задержки — кадры байтами, как по сети.
//!   restart_*        — сохранил → перезапустил → мир, инвентари и позиции на месте; автосохранение в потоке записи
//!                      переживает «падение» (без shutdown); сбой записи чинится полной записью;
//!   token_*          — ник закрепляется за токеном; чужой токен, гость и очищенный браузер; тот же токен вытесняет
//!                      прежний сеанс (переподключение после обрыва);
//!   outdated_*       — другая сборка получает «обновите страницу», даже когда Hello изменился;
//!   edit_budget_*    — правки кусков при входе и смене куска уходят порциями по 32 КБ;
//!   pickup_*         — летящую копию подбора видят все, кто следит за предметом.

use std::path::PathBuf;

use crate::cell::Cell;
use crate::net::{ConnId, Peer, Server};
use crate::proto::{decode, encode, Hello, Msg, Reject, C2S, PROTO, S2C};
use crate::registry::ids::*;
use crate::registry::items::*;
use crate::save::storage::{FileStorage, Storage};
use crate::world::stack::ItemStack;

const SEED: u32 = 1337;

/// Чистая папка мира во временной (уникальная для теста и процесса).
fn world_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("voxel-n1c-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn open(dir: &PathBuf) -> Server {
    let mut sv = Server::open(SEED, Some(dir.to_str().unwrap()), true).expect("мир открылся");
    sv.set_autosave(0);
    sv
}

/// Токен из одного байта (проверки).
fn tok(b: u8) -> [u8; 16] {
    [b; 16]
}

/// Сервер и клиенты в одном потоке без задержки.
struct Rig {
    sv: Server,
    peers: Vec<(Peer, ConnId)>,
    /// Соединения, которые сервер закрыл (отказ, вытеснение).
    closed: Vec<ConnId>,
    /// Байт ChunkEdits всем клиентам с прошлого тика (вход считается в следующий тик) и по тикам.
    edit_bytes: usize,
    per_tick: Vec<usize>,
}

impl Rig {
    fn new(sv: Server) -> Rig {
        Rig { sv, peers: Vec::new(), closed: Vec::new(), edit_bytes: 0, per_tick: Vec::new() }
    }

    /// Площадка из камня 16 × 28 на y = 100 высоко над рельефом (x −8…7, z −8…19): игрок стоит, не падая.
    fn floor(&mut self) {
        self.floor_at(-8, 8, -8, 20);
    }
    fn floor_at(&mut self, x0: i32, x1: i32, z0: i32, z1: i32) {
        for x in x0..x1 {
            for z in z0..z1 {
                self.sv.sim.world_edit([x, 100, z], STONE as Cell);
            }
        }
    }

    /// Новое соединение клиента с ником и токеном; номер в peers.
    fn connect(&mut self, nick: &str, token: [u8; 16]) -> usize {
        let c = self.sv.open_conn();
        self.peers.push((Peer::with_token(SEED, nick, token), c));
        self.pump();
        self.peers.len() - 1
    }

    fn pump(&mut self) {
        for (peer, c) in &mut self.peers {
            if self.closed.contains(c) {
                continue;
            }
            for b in peer.take() {
                self.sv.frame(*c, &b);
            }
        }
        let (out, close) = self.sv.take();
        for (c, b) in out {
            if let Some(S2C::Bundle(bundle)) = decode::<S2C>(&b).ok() {
                for m in &bundle.msgs {
                    if matches!(m, Msg::ChunkEdits { .. }) {
                        self.edit_bytes += encode(m).len();
                    }
                }
            }
            if let Some((peer, _)) = self.peers.iter_mut().find(|p| p.1 == c) {
                peer.recv(&b).expect("кадр сервера разобрался");
            }
        }
        for c in close {
            self.sv.close_conn(c);
            self.closed.push(c);
        }
    }

    /// Тик: клиенты (кто готов) → сервер → кадры.
    fn tick(&mut self) {
        for (peer, c) in &mut self.peers {
            if !self.closed.contains(c) && peer.ready() {
                peer.tick();
            }
        }
        self.pump();
        self.sv.tick();
        self.pump();
        self.per_tick.push(std::mem::take(&mut self.edit_bytes));
    }

    fn ticks(&mut self, n: usize) {
        for _ in 0..n {
            self.tick();
        }
    }

    /// Ждать, пока все живые клиенты войдут и получат правки вокруг.
    fn until_ready(&mut self) {
        for _ in 0..400 {
            if self.peers.iter().filter(|p| !self.closed.contains(&p.1)).all(|p| p.0.ready()) {
                return;
            }
            self.tick();
        }
        panic!("не вошли: {:?}", self.peers.iter().map(|p| (p.0.id(), p.0.rejected())).collect::<Vec<_>>());
    }

    /// Состояние игрока по номеру в peers на сервере: (ступни, содержимое 41 ячейки, выбранная).
    fn state(&self, k: usize) -> ([u64; 3], Vec<ItemStack>, usize) {
        let id = self.peers[k].0.id();
        let p = self.sv.sim.player(id).expect("игрок на сервере");
        (p.player.pos.map(f64::to_bits), (0..41).map(|i| p.inv.get(i)).collect(), p.inv.selected)
    }

    fn rejected(&self, k: usize) -> Option<Reject> {
        self.peers[k].0.client.sim.stats.reject
    }
}

/// Игрок в состоянии parked после загрузки: (ступни, ячейки, выбранная).
fn parked(sv: &Server, nick: &str) -> ([u64; 3], Vec<ItemStack>, usize) {
    let p = sv.sim.parked.iter().find(|p| p.nick == nick).unwrap_or_else(|| panic!("игрок «{nick}» не загрузился"));
    (p.player.pos.map(f64::to_bits), (0..41).map(|i| p.inv.get(i)).collect(), p.inv.selected)
}

/// Бревно с осью — правка с состоянием.
fn cell_log() -> Cell {
    crate::cell::cell_of(LOG, 1)
}

// ---------------------------------------------------------------- хранение

/// Пуля N1c: сохранил → перезапустил → то же состояние. Мир (правки, время), два игрока с инвентарями и позициями;
/// потом тот же токен входит и получает всё это в Welcome.
#[test]
fn restart_restores_world_players_inventories() {
    let dir = world_dir("restart");
    let mut rig = Rig::new(open(&dir));
    rig.floor();
    let (a, b) = (rig.connect("Алиса", tok(1)), rig.connect("Боб", tok(2)));
    rig.until_ready();
    rig.peers[a].0.teleport([0.5, 101.0, 0.5]);
    rig.peers[b].0.teleport([4.5, 101.0, 4.5]);
    rig.peers[a].0.give(I_COAL, 7);
    rig.peers[a].0.give(COBBLE as ItemId, 33);
    rig.peers[b].0.give(I_STICK, 3);
    rig.sv.sim.world_edit([3, 101, 3], PLANKS as Cell);
    rig.sv.sim.world_edit([-3, 101, 5], cell_log());
    rig.ticks(30);
    let (sa, sb) = (rig.state(a), rig.state(b));
    assert!(sa.1.iter().any(|s| s.item == I_COAL && s.count == 7), "уголь у Алисы до остановки");
    let time = rig.sv.sim.sim.time;
    let edits = rig.sv.status().edits;
    assert!(edits >= 16 * 28, "площадка и две правки в мире: {edits}");
    let r = rig.sv.shutdown().expect("остановка записала мир");
    assert!(r.contains("puts"), "{r}");
    drop(rig);

    // Перезапуск: тот же каталог.
    let sv = open(&dir);
    assert_eq!(sv.sim.sim.time, time, "время мира продолжается с места остановки");
    assert_eq!(sv.status().edits, edits, "правки мира на месте");
    assert_eq!(sv.sim.sim.world.d.borrow_mut().get_block(3, 101, 3), PLANKS, "доски на месте");
    assert_eq!(parked(&sv, "Алиса"), sa, "Алиса: позиция и инвентарь побитово");
    assert_eq!(parked(&sv, "Боб"), sb, "Боб: позиция и инвентарь побитово");

    // Тот же токен входит и получает это же в Welcome (клиент встаёт где вышел).
    let mut rig = Rig::new(sv);
    let a2 = rig.connect("Алиса", tok(1));
    rig.until_ready();
    let pos = rig.peers[a2].0.pos();
    assert_eq!(pos.map(f64::to_bits), sa.0, "позиция в Welcome");
    assert_eq!(rig.peers[a2].0.count(I_COAL), 7);
    assert_eq!(rig.peers[a2].0.count(COBBLE as ItemId), 33);
    assert_eq!(rig.peers[a2].0.block(3, 101, 3), PLANKS, "клиент видит правки мира");
    rig.sv.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Автосохранение идёт само, в потоке записи; «падение» без shutdown теряет только то, что после него.
#[test]
fn restart_after_crash_keeps_last_autosave() {
    let dir = world_dir("crash");
    let mut sv = open(&dir);
    sv.set_autosave(20);
    let mut rig = Rig::new(sv);
    rig.floor();
    let a = rig.connect("Алиса", tok(1));
    rig.until_ready();
    rig.peers[a].0.teleport([0.5, 101.0, 0.5]);
    rig.peers[a].0.give(I_COAL, 5);
    rig.sv.sim.world_edit([2, 101, 2], PLANKS as Cell);
    // Автосохранение — каждые 20 тиков сервера; до него мир лежит в памяти.
    let before = rig.sv.sim.sim.time;
    while rig.sv.sim.sim.time < before + 21 {
        rig.tick();
    }
    rig.sv.sync_writes().expect("поток записи успел");
    // Это уже после автосохранения: «падение» его потеряет.
    rig.sv.sim.world_edit([-2, 101, -2], cell_log());
    rig.peers[a].0.give(I_STICK, 9);
    rig.tick();
    assert!(rig.state(a).1.iter().any(|s| s.item == I_STICK), "палки у Алисы есть в памяти");
    drop(rig); // без shutdown: как kill -9 (очередь потока записи дописывается, новых пачек не будет)

    let sv = open(&dir);
    assert_eq!(sv.sim.sim.world.d.borrow_mut().get_block(2, 101, 2), PLANKS, "правка до автосохранения на месте");
    assert_eq!(sv.sim.sim.world.d.borrow_mut().get_block(-2, 101, -2), AIR, "правка после автосохранения потеряна");
    let (_, inv, _) = parked(&sv, "Алиса");
    assert!(inv.iter().any(|s| s.item == I_COAL && s.count == 5), "уголь Алисы сохранён");
    assert!(inv.iter().all(|s| s.item != I_STICK), "палки после автосохранения потеряны");
    drop(sv);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Выход пишет запись игрока сразу, не дожидаясь автосохранения: сервер «упал» следом — вещи и позиция на месте.
#[test]
fn restart_player_saved_on_leave_without_autosave() {
    let dir = world_dir("leave");
    let mut rig = Rig::new(open(&dir));
    rig.floor();
    let a = rig.connect("Алиса", tok(1));
    rig.until_ready();
    rig.peers[a].0.teleport([1.5, 101.0, 2.5]);
    rig.peers[a].0.give(I_COAL, 4);
    rig.ticks(10);
    let want = rig.state(a);
    let (_, c) = rig.peers[a];
    rig.sv.close_conn(c); // выход
    rig.sv.sync_writes().unwrap();
    drop(rig); // ни автосохранения, ни shutdown
    let sv = open(&dir);
    assert_eq!(parked(&sv, "Алиса"), want);
    drop(sv);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Сбой записи (папка недоступна) не роняет сервер; следующая запись — полная и всё наверстывает.
#[test]
fn restart_write_failure_recovers_with_full_write() {
    let dir = world_dir("failure");
    let mut rig = Rig::new(open(&dir));
    rig.floor();
    let a = rig.connect("Алиса", tok(1));
    rig.until_ready();
    rig.peers[a].0.give(I_COAL, 2);
    rig.ticks(3);
    // Вместо папки chunk — файл: куски писать некуда.
    std::fs::write(dir.join("chunk"), b"x").unwrap();
    let err = rig.sv.save().expect_err("запись не удалась");
    assert!(err.contains("запись"), "{err}");
    std::fs::remove_file(dir.join("chunk")).unwrap();
    rig.sv.save().expect("следующая запись прошла");
    rig.sv.shutdown().unwrap();
    drop(rig);
    let sv = open(&dir);
    assert!(sv.status().edits >= 16 * 28, "площадка легла после сбоя: {}", sv.status().edits);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------- токены

/// Ник закреплён за токеном: чужой токен и гость получают отказ, хозяин входит; запись на диске не хранит сам токен.
#[test]
fn token_pins_nick_across_restart() {
    let dir = world_dir("token");
    let mut rig = Rig::new(open(&dir));
    rig.floor();
    let a = rig.connect("Алиса", tok(1));
    let guest = rig.connect("Гость", [0; 16]);
    rig.until_ready();
    rig.peers[a].0.give(I_COAL, 3);
    rig.ticks(5);
    let (_, ca) = rig.peers[a];
    let (_, cg) = rig.peers[guest];
    rig.sv.close_conn(ca);
    rig.sv.close_conn(cg);
    rig.sv.shutdown().unwrap();
    drop(rig);

    // В записи — хеш: ни токена, ни его кусков.
    let st = FileStorage::open(&dir).unwrap();
    let rec = st.get("player/алиса").unwrap().expect("запись Алисы");
    assert!(!rec.windows(16).any(|w| w == tok(1)), "токен в открытом виде в записи");
    drop(st);

    let mut rig = Rig::new(open(&dir));
    rig.floor();
    let thief = rig.connect("алиса", tok(2)); // регистр ника не важен
    let anon = rig.connect("АЛИСА", [0; 16]);
    rig.pump();
    assert_eq!(rig.rejected(thief), Some(Reject::BadToken), "чужой токен");
    assert_eq!(rig.rejected(anon), Some(Reject::BadToken), "гость к закреплённому нику");
    assert!(rig.peers[thief].0.rejected().unwrap().contains("другим игроком"), "понятный текст");
    assert!(rig.sv.sim.player(rig.peers[thief].0.id()).is_none() && rig.sv.sim.players.is_empty(), "никто не вошёл");
    // Хозяин входит и находит вещи.
    let owner = rig.connect("Алиса", tok(1));
    // Гость вернулся на свободный ник — входит; токен им не закрепляется, ник свободен для настоящего владельца.
    let guest2 = rig.connect("Гость", [0; 16]);
    rig.until_ready();
    assert_eq!(rig.rejected(owner), None);
    assert_eq!(rig.peers[owner].0.count(I_COAL), 3, "вещи Алисы");
    // Хозяйка в игре: чужой токен — BadToken (повтор не поможет), а не NickTaken (клиент повторял бы вечно).
    let thief2 = rig.connect("Алиса", tok(3));
    rig.pump();
    assert_eq!(rig.rejected(thief2), Some(Reject::BadToken), "чужой токен при хозяйке в игре");
    assert_eq!(rig.rejected(guest2), None, "свободный ник открыт гостю");
    let claim = rig.connect("Гость2", tok(7));
    let (_, cg2) = rig.peers[guest2];
    rig.sv.close_conn(cg2);
    rig.until_ready();
    let taker = rig.connect("Гость", tok(8)); // токен закрепляет ник, который гость занимал без токена
    rig.until_ready();
    assert_eq!(rig.rejected(taker), None);
    assert_eq!(rig.rejected(claim), None);
    rig.sv.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Тот же ник и токен уже вошли с другого соединения (обрыв, который сервер ещё не заметил, перезагрузка вкладки): старое
/// закрывается, новое входит на его место с теми же вещами; другие игроки видят его и дальше. Чужой токен — BadToken (повтор не поможет).
#[test]
fn token_same_session_replaces_old_connection() {
    let mut rig = Rig::new(Server::new(SEED, true));
    rig.floor();
    let a = rig.connect("Алиса", tok(1));
    let b = rig.connect("Боб", tok(2));
    rig.until_ready();
    rig.peers[a].0.teleport([0.5, 101.0, 0.5]);
    rig.peers[b].0.teleport([3.5, 101.0, 3.5]);
    rig.peers[a].0.give(I_COAL, 6);
    rig.ticks(15);
    assert!(rig.peers[b].0.remotes().iter().any(|r| r.1 == "Алиса"), "Боб видит Алису");
    let id = rig.peers[a].0.id();

    // Чужой токен при живом сеансе — отказ, Алиса остаётся.
    let thief = rig.connect("Алиса", tok(9));
    rig.pump();
    assert!(matches!(rig.rejected(thief), Some(Reject::BadToken)), "{:?}", rig.rejected(thief));
    assert!(!rig.closed.contains(&rig.peers[a].1), "сеанс Алисы цел");

    // Тот же токен — вытесняет.
    let a2 = rig.connect("Алиса", tok(1));
    rig.until_ready();
    assert!(rig.closed.contains(&rig.peers[a].1), "старое соединение закрыто");
    assert!(rig.peers[a].0.rejected().is_some_and(|t| t.contains("с другого")), "старому сказали, почему: {:?}", rig.peers[a].0.rejected());
    assert_eq!(rig.rejected(a2), None);
    assert_eq!(rig.peers[a2].0.id(), id, "тот же номер сущности игрока");
    assert_eq!(rig.peers[a2].0.count(I_COAL), 6, "вещи на месте");
    assert_eq!(rig.sv.sim.players.len(), 2, "двое на сервере");
    rig.ticks(15);
    let names: Vec<String> = rig.peers[b].0.remotes().into_iter().map(|r| r.1).collect();
    assert_eq!(names, vec!["Алиса".to_string()], "Боб по-прежнему видит ровно одну Алису");
}

// ---------------------------------------------------------------- Outdated

/// Кадр на новое соединение: ответы сервера и закрыл ли он соединение.
fn raw(sv: &mut Server, frame: &[u8]) -> (Vec<S2C>, bool) {
    let c = sv.open_conn();
    sv.frame(c, frame);
    let (out, close) = sv.take();
    (out.into_iter().filter(|(k, _)| *k == c).map(|(_, b)| decode::<S2C>(&b).expect("ответ разобрался")).collect(), close.contains(&c))
}

fn hello(proto: u16, build: &str) -> Hello {
    Hello { proto, build: build.into(), nick: "Вася".into(), token: tok(3), view: 8, probe: crate::gen::probe() }
}

#[test]
fn outdated_client_is_told_to_reload() {
    let mut sv = Server::new(SEED, false);
    let reload = |r: &[S2C]| matches!(r, [S2C::Reject { code: Reject::Outdated, text }] if text.contains("Обновите страницу"));
    // Другой протокол.
    let (r, closed) = raw(&mut sv, &encode(&C2S::Hello(hello(PROTO + 1, crate::proto::BUILD))));
    assert!(reload(&r) && closed, "{r:?}");
    // Другая сборка (если номер сборки известен).
    if crate::proto::BUILD != "dev" {
        let (r, closed) = raw(&mut sv, &encode(&C2S::Hello(hello(PROTO, "0badc0d"))));
        assert!(reload(&r) && closed, "{r:?}");
        assert!(matches!(&r[..], [S2C::Reject { text, .. }] if text.contains("0badc0d")), "в тексте — их сборка");
    }
    // Клиент старой сборки: Hello другой формы (после build всё иное) не разбирается целиком — всё равно «обновите».
    let mut old = encode(&C2S::Hello(hello(PROTO - 1, "0badc0d")));
    old.truncate(old.len() - 12);
    old.extend_from_slice(&[0xFF; 4]);
    assert!(decode::<C2S>(&old).is_err());
    let (r, closed) = raw(&mut sv, &old);
    assert!(reload(&r) && closed, "{r:?}");
    // Мусор — не «устарел», а битый кадр.
    let (r, closed) = raw(&mut sv, &[7, 7, 7, 7]);
    assert!(matches!(&r[..], [S2C::Kick { .. }]) && closed, "{r:?}");
    // Своя сборка входит.
    let (r, closed) = raw(&mut sv, &encode(&C2S::Hello(hello(PROTO, crate::proto::BUILD))));
    assert!(!closed && !r.iter().any(|m| matches!(m, S2C::Reject { .. })), "{r:?}");
    // «dev» с любой сборкой не спорит (номер неизвестен).
    let (r, _) = raw(&mut sv, &encode(&C2S::Hello(Hello { nick: "Петя".into(), ..hello(PROTO, "dev") })));
    assert!(!r.iter().any(|m| matches!(m, S2C::Reject { .. })), "{r:?}");
}

// ---------------------------------------------------------------- бюджет правок

/// Правки подземных кусков вокруг (x, z — в кусках): по n клеток в куске, не пересекаются с площадкой на y = 100.
fn fill_chunks(sv: &mut Server, cx: std::ops::RangeInclusive<i32>, cz: std::ops::RangeInclusive<i32>, n: i32) {
    let w = sv.sim.sim.world.d.clone();
    let mut w = w.borrow_mut();
    for x in cx {
        for z in cz.clone() {
            for k in 0..n {
                w.set_cell(x * 16 + k % 16, 10 + (k / 16) % 20, z * 16 + (k / 320) % 16, COBBLE as Cell);
            }
        }
    }
    w.take_log();
}

/// Вход в мир с сотнями килобайт правок: кадры идут порциями ≤ 32 КБ на тик, ближние куски — первыми, игрок тикает,
/// как только пришли его кусок и соседи; в конце у клиента то же, что у сервера (и живая правка ждущего куска — тоже).
#[test]
fn edit_budget_spreads_join_over_ticks() {
    let mut rig = Rig::new(Server::new(SEED, true));
    fill_chunks(&mut rig.sv, -4..=4, -4..=4, 3500);
    let total: usize = rig.sv.sim.sim.world.d.borrow().edited_chunks().map(|c| encode(&Msg::ChunkEdits { cx: c.0, cz: c.1, edits: crate::proto::delta_keys(&rig.sv.sim.sim.world.d.borrow().chunk_edits(c.0, c.1)) }).len()).sum();
    assert!(total > 4 * crate::game::server::EDIT_BUDGET, "правок достаточно, чтобы не уместиться в один тик: {total} Б");
    let a = rig.connect("Алиса", tok(1));
    rig.until_ready();
    let first_ready = rig.per_tick.len();
    // Живая правка куска, который ещё ждёт очереди: клиент получит его целиком уже с ней.
    let far = (4, 4);
    rig.sv.sim.world_edit([far.0 * 16 + 1, 12, far.1 * 16 + 1], PLANKS as Cell);
    rig.ticks(200);
    let budget = crate::game::server::EDIT_BUDGET;
    let worst = rig.per_tick.iter().copied().max().unwrap();
    assert!(worst <= budget, "за тик ушло {worst} Б правок при бюджете {budget}");
    let busy = rig.per_tick.iter().filter(|&&b| b > 0).count();
    assert!(busy >= 6, "правки шли порциями: {busy} тиков");
    assert!(first_ready >= 1);
    let (server_edits, client_edits) = (rig.sv.status().edits, rig.peers[a].0.world.d.borrow().edits_len());
    assert_eq!(client_edits, server_edits, "у клиента все правки сервера");
    assert_eq!(rig.peers[a].0.block(far.0 * 16 + 1, 12, far.1 * 16 + 1), PLANKS, "живая правка ждущего куска дошла");
}

/// Смена куска: новые куски радиуса встают в очередь (после EditIndex), ближние к новому месту — первыми; бюджет тот же.
#[test]
fn edit_budget_applies_when_crossing_chunks() {
    let mut rig = Rig::new(Server::new(SEED, true));
    rig.floor_at(320, 336, 0, 16);
    // Правки далеко от места входа: у входа их нет, ни одного куска в радиусе.
    fill_chunks(&mut rig.sv, 20..=26, -3..=3, 3500);
    let a = rig.connect("Алиса", tok(1));
    rig.until_ready();
    assert_eq!(rig.sv.status().edits, rig.sv.sim.sim.world.d.borrow().edits_len());
    let before = rig.per_tick.len();
    // Шаг через 300 блоков к правкам: сервер принимает телепорт отладки, куски радиуса идут порциями.
    rig.peers[a].0.teleport([20.0 * 16.0 + 8.0, 101.0, 8.0]);
    rig.ticks(120);
    let budget = crate::game::server::EDIT_BUDGET;
    let tail = &rig.per_tick[before..];
    assert!(tail.iter().all(|&b| b <= budget), "наибольший за тик: {:?}", tail.iter().max());
    assert!(tail.iter().filter(|&&b| b > 0).count() >= 4, "правки шли несколькими тиками");
    assert!(tail.iter().sum::<usize>() > budget, "и всего больше одного бюджета");
}

// ---------------------------------------------------------------- подбор

/// Летящую копию подбора видят все, кто следит за предметом, — и она летит к подобравшему, а не к смотрящему; далёкий
/// игрок (вне слежения) её не получает.
#[test]
fn pickup_flying_copy_is_seen_by_everyone_nearby() {
    let mut rig = Rig::new(Server::new(SEED, true));
    rig.floor();
    rig.floor_at(-8, 8, 300, 310);
    let (a, b, c) = (rig.connect("Алиса", tok(1)), rig.connect("Боб", tok(2)), rig.connect("Вера", tok(3)));
    rig.until_ready();
    rig.peers[a].0.teleport([0.5, 101.0, 0.5]);
    rig.peers[b].0.teleport([5.5, 101.0, 5.5]);
    rig.peers[c].0.teleport([0.5, 101.0, 305.5]);
    rig.ticks(15);
    // Предмет лежит в стороне от всех: Боб и Алиса его видят (Spawn), подобрать никто не может. Потом Алиса подходит.
    let id = {
        let s = &mut rig.sv.sim.sim;
        let id = s.ids.next();
        let mut e = s.items.make(id, [0.5, 101.0, 8.0], ItemStack::of(I_COAL, 3), &mut crate::game::GameRng(5));
        e.vel = [0.0; 3];
        s.items.list.push(e);
        id
    };
    rig.ticks(4);
    for k in [a, b] {
        assert!(rig.peers[k].0.items() > 0, "игрок {k} видит предмет до подбора");
    }
    assert_eq!(rig.peers[c].0.items(), 0, "Вера далеко от предмета");
    rig.peers[a].0.teleport([0.5, 101.0, 7.6]);
    let mut seen = [false; 3];
    let mut flying_to_alice = false;
    let alice = rig.peers[a].0.id();
    for _ in 0..8 {
        rig.tick();
        for (k, seen) in seen.iter_mut().enumerate() {
            let anims = &rig.peers[k].0.client.sim.items.anims;
            if let Some(an) = anims.first() {
                *seen = true;
                // Для Алисы копия летит к ней (to = None), для Боба — к Алисе по её номеру.
                match (k, an.to) {
                    (0, None) => {}
                    (1, Some(t)) if t == alice => {
                        flying_to_alice = true;
                        let draws = rig.peers[k].0.client.sim.item_draws(0.5);
                        assert_eq!(draws.len(), 8, "копия попала в отрисовку Боба (сам предмет уже убран)");
                    }
                    other => panic!("копия у игрока {k} летит к {other:?}"),
                }
            }
        }
    }
    assert_eq!(rig.peers[a].0.count(I_COAL), 3, "Алиса подобрала");
    assert_eq!(seen, [true, true, false], "видят подобравшая и Боб рядом; Вера в 300 блоках — нет");
    assert!(flying_to_alice);
    let _ = id;
}
