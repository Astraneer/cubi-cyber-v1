//! Проверки сохранения (Ф2): туда-обратно (отпечаток состояния до и после совпадает), продолжение тиков после
//! загрузки идёт так же, как без сохранения, save_dirty пишет только изменённое, обрыв записи (journal), FileStorage во
//! временной папке, удаление правок, совпавших с генератором, ошибки вместо паники на чужих и порченых данных.
//! Пишет и грузит сервер (ServerSim); сцены и тики — через Game (клиент + локальный сервер), загруженный мир — новый
//! клиент входит на загруженный сервер (Game::from_server).

use super::*;
use crate::cell::{block_of, cell_of, Cell, F_NORTH, F_SOUTH};
use crate::game::screen::Menu;
use crate::abi::{EV_FURNACE_CRACKLE, EV_FURNACE_FX};
use crate::game::Game;
use crate::world::World;
use crate::registry::ids::*;
use crate::registry::items::*;
use crate::save::storage::{decode_part, encode_part, MemStorage};
use crate::world::blockentity::{FURNACE_FUEL, FURNACE_IN, FURNACE_OUT};
use crate::world::stack::Container;

const NICK: &str = "Тестер";

/// Пол из камня 9×9 на y = 100 высоко над рельефом, игрок стоит на нём в (0.5, 101, 0.5).
fn scene() -> Game {
    let w = World::new(1337);
    for x in -4..=4 {
        for z in -4..=4 {
            w.set_block(x, 100, z, STONE);
        }
    }
    let mut g = Game::with_nick(&w, NICK);
    no_mobs(&mut g);
    g.teleport(0.5, 101.0, 0.5);
    g.body(|p| {
        p.on_ground = true;
        p.vel = [0.0, -0.0784, 0.0];
    });
    g.set_look(30.0, 10.0);
    g.tick();
    g
}

/// Мобы не сохраняются (стада — раз за сессию, ASP-16 §6.8): сверка «после загрузки тикает так же» — без них.
fn no_mobs(g: &mut Game) {
    g.server.sim.sim.mobs.herds = false;
    g.server.sim.sim.mobs.natural = false;
}

/// Загрузить мир сервера и войти игроком nick новым клиентом.
fn load(st: &mut dyn Storage, nick: &str) -> Result<(Game, Saver), SaveError> {
    let (mut s, sv) = Saver::load(st, nick)?;
    s.sim.mobs.herds = false;
    s.sim.mobs.natural = false;
    Ok((Game::from_server(s, nick), sv))
}

/// Сцена со всем, что сохраняется: горящая печь с железом, сундук с вещами (и изношенной киркой), предметы на полу
/// (и в соседнем куске), падающий песок в воздухе, тики в очереди (рядом и в замёрзшем куске вне зоны), правки с
/// состоянием, инвентарь с бронёй и второй рукой, время суток.
fn busy() -> Game {
    let mut g = scene();
    let furnace = [2, 101, 2];
    g.world_edit(furnace[0], furnace[1], furnace[2], cell_of(FURNACE, F_NORTH as u16));
    {
        let sw = g.sw();
        let mut w = sw.d.borrow_mut();
        let f = w.be_mut().furnace_mut(furnace).unwrap();
        f.set(FURNACE_IN, ItemStack::of(I_RAW_IRON, 5));
        f.set(FURNACE_FUEL, ItemStack::of(I_COAL, 2));
    }
    let chest = [-2, 101, 2];
    g.world_edit(chest[0], chest[1], chest[2], cell_of(CHEST, F_SOUTH as u16));
    {
        let sw = g.sw();
        let mut w = sw.d.borrow_mut();
        let c = w.be_mut().chest_mut(chest).unwrap();
        c.set(0, ItemStack::of(COBBLE as ItemId, 40));
        c.set(13, ItemStack { item: I_WOODEN_PICKAXE, count: 1, damage: 17 });
        c.set(26, ItemStack::of(I_STICK, 3));
    }
    g.world_edit(1, 101, -3, cell_of(LOG, 1)); // бревно с осью — правка с состоянием
    g.world_edit(-3, 101, -3, PLANKS as Cell);
    g.world_edit(200, 120, 40, GRAVEL as Cell); // кусок вне зоны: тик стоит в очереди
    g.kit();
    g.set_slot(39, ItemStack::of(I_LEATHER_HELMET + 4, 1));
    g.set_slot(40, ItemStack::of(TORCH as ItemId, 5));
    g.select(3);
    g.set_time_of_day(0.3);
    for _ in 0..30 {
        g.tick();
    }
    g.on_server(|sv, _| {
        let s = &mut sv.sim;
        s.items.drop_from_block([1, 101, 1], ItemStack::of(DIRT as ItemId, 3), &mut s.rng, &mut s.ids);
        s.items.drop_from_block([-1, 101, 0], ItemStack::of(I_STICK, 2), &mut s.rng, &mut s.ids);
        s.items.drop_from_block([-20, 90, 3], ItemStack::of(I_COAL, 1), &mut s.rng, &mut s.ids); // кусок (−2, 0)
    });
    g.world_edit(3, 125, 3, SAND as Cell); // через 2 тика — падающая сущность, в момент записи летит
    for _ in 0..3 {
        g.tick();
    }
    g.world_edit(-3, 112, -3, SAND as Cell); // тик в очереди в момент записи
    g.take_events();
    assert!(g.sim().world.d.borrow().be().furnace(furnace).unwrap().burning(), "печь горит");
    assert!(!g.sim().falling.is_empty() && g.sim().ticks.len() >= 2 && g.sim().items.list.len() >= 3, "есть что сохранять");
    g
}

/// Отпечаток состояния сервера по разделам — прямо из структур игры, мимо кода сохранения (иначе проверка была бы по
/// кругу).
/// Позиции и скорости — биты f64. Не входит производное: вода у предмета и «внутри блока» (досчитываются в начале
/// его тика), prev, крышки и смотрящие сундуков, рука, HUD, ломание, ввод.
fn snapshot(g: &Game) -> Vec<(&'static str, String)> {
    let s = g.sim();
    let b = |v: [f64; 3]| format!("{:x},{:x},{:x}", v[0].to_bits(), v[1].to_bits(), v[2].to_bits());
    let mut out = Vec::new();
    out.push((
        "время и ГСЧ",
        format!(
            "{} {} {} {:x} {:x} {:x} {} {} {}",
            s.time, s.day, s.day_cycle, s.rng.0, s.tick_rng.0, g.server.sim.fx_rng.0, s.ids.0, s.ticks.seq(), g.me().id
        ),
    ));
    let mut w = s.world.d.borrow_mut();
    let mut chunks: Vec<(i32, i32)> = w.edited_chunks().collect();
    chunks.sort();
    let edits: Vec<String> = chunks.iter().map(|&(x, z)| format!("{x},{z}:{:?}", w.chunk_edits(x, z))).collect();
    out.push(("правки", format!("{} {}", w.edit_count(), edits.join(" "))));
    let mut bes: Vec<String> = w
        .be()
        .iter()
        .map(|(p, seq, be)| match be {
            BlockEntity::Furnace(f) => format!("{p:?}#{seq} печь {:?} {} {} {} {}", f.slots, f.burn, f.fuel_total, f.cook, f.cook_total),
            BlockEntity::Chest(c) => format!("{p:?}#{seq} сундук {:?}", c.slots),
            BlockEntity::Mek(m) => format!("{p:?}#{seq} машина {}", m.saved_key()),
        })
        .collect();
    bes.sort();
    out.push(("блок-сущности", format!("{} {}", w.be().next_seq(), bes.join(" "))));
    out.push(("порядок печей", format!("{:?}", w.be_mut().ticking_list())));
    drop(w);
    let items: Vec<String> = s
        .items
        .list
        .iter()
        .map(|e| {
            format!(
                "#{} {:?} {} {} {} {} {} {} {} {} {:x} {}",
                e.id, e.stack, b(e.pos), b(e.vel), e.on_ground, e.age, e.item_age, e.spin_age, e.pickup_delay, e.thrown, e.offset.to_bits(), e.removed
            )
        })
        .collect();
    out.push(("предметы", items.join(" | ")));
    let falling: Vec<String> =
        s.falling.iter().map(|f| format!("#{} {:x} {} {} {} {}", f.id, f.cell, b(f.pos), b(f.vel), f.on_ground, f.age)).collect();
    out.push(("падающие", falling.join(" | ")));
    let mut tc: Vec<(i32, i32)> = s.ticks.chunks().collect();
    tc.sort();
    let ticks: Vec<String> = tc.iter().map(|&(x, z)| format!("{:?}", s.ticks.chunk_entries(x, z))).collect();
    out.push(("тики", format!("{} {}", s.ticks.len(), ticks.join(" "))));
    let me = g.me();
    let p = &me.player;
    out.push((
        "игрок",
        format!(
            "{} {} {:x} {:x} {} {:x} {} {}",
            b(p.pos),
            b(p.vel),
            p.yaw.to_bits(),
            p.pitch.to_bits(),
            p.on_ground,
            p.fall.to_bits(),
            p.flying,
            p.crouch
        ),
    ));
    let inv: Vec<String> = (0..41).map(|i| format!("{:?}", me.inv.get(i))).collect();
    out.push(("инвентарь", format!("{} {}", me.inv.selected, inv.join(" "))));
    out
}

fn assert_same(a: &Game, b: &Game, when: &str) {
    for ((name, x), (_, y)) in snapshot(a).into_iter().zip(snapshot(b)) {
        assert_eq!(x, y, "{when}: раздел «{name}» разошёлся");
    }
}

fn saved(g: &Game) -> MemStorage {
    let mut st = MemStorage::default();
    Saver::new().save_all(&g.server.sim, &mut st).unwrap();
    st
}

#[test]
fn round_trip_restores_state() {
    let g = busy();
    let mut st = saved(&g);
    assert!(st.map.contains_key("meta") && st.map.contains_key("player/тестер"));
    assert!(st.map.keys().filter(|k| k.starts_with("chunk/")).count() >= 4, "{:?}", st.map.keys().collect::<Vec<_>>());
    assert!(st.map.contains_key("chunk/12,2"), "тик гравия в замёрзшем куске");
    let (g2, _) = load(&mut st, NICK).unwrap();
    assert_same(&g, &g2, "после загрузки");
    // Записанное из загруженного — те же байты.
    assert_eq!(saved(&g2).map, st.map);
    // Сутки, которые стоят, — тоже состояние.
    let mut g3 = busy();
    g3.set_day_cycle(false);
    g3.sync(); // команда отладки — до сервера на границе тика
    let (g4, _) = load(&mut saved(&g3), NICK).unwrap();
    assert!(!g4.sim().day_cycle);
    // Нового игрока ставит точка появления, мир тот же.
    let (g5, _) = load(&mut st, "Другой").unwrap();
    assert_eq!(snapshot(&g5)[1], snapshot(&g)[1]);
    assert!(g5.ctl().inv.main.iter().all(|s| s.is_empty()));
}

#[test]
fn ticks_after_load_match() {
    // Запись посреди всего, затем 400 тиков у обоих: печь плавит, песок падает и ложится, предметы лежат и сливаются,
    // гравий вне зоны ждёт. Отпечатки и события совпадают на каждом тике.
    let mut a = busy();
    let mut st = saved(&a);
    let (mut b, _) = load(&mut st, NICK).unwrap();
    let furnace = [2, 101, 2];
    let out0 = a.sw().d.borrow().be().furnace(furnace).unwrap().slots[FURNACE_OUT].n();
    // Частицы и треск печи — случайная выборка клиента (свой ГСЧ эффектов у каждого клиента): не сверяются.
    let sim_events = |g: &mut Game| -> Vec<i32> { g.take_events().chunks(5).filter(|e| e[0] != EV_FURNACE_FX && e[0] != EV_FURNACE_CRACKLE).flatten().copied().collect() };
    for t in 1..=400 {
        a.tick();
        b.tick();
        assert_eq!(sim_events(&mut a), sim_events(&mut b), "события тика {t}");
        if t % 20 == 0 || t < 5 {
            assert_same(&a, &b, &format!("тик {t}"));
        }
    }
    assert_same(&a, &b, "конец");
    let out1 = a.sw().d.borrow().be().furnace(furnace).unwrap().slots[FURNACE_OUT].n();
    assert!(out1 > out0, "печь плавила: {out0} → {out1}");
    assert!(a.sim().falling.is_empty(), "песок лёг");
    assert_eq!(a.sim().world.get_block(3, 101, 3), SAND);
    // И записи после этого — те же байты.
    assert_eq!(saved(&a).map, saved(&b).map);
}

/// Хранилище-обёртка: считает записи и «обрывается» после заданного числа.
struct Flaky {
    inner: MemStorage,
    puts: usize,
    left: Option<usize>,
}

impl Flaky {
    fn new() -> Flaky {
        Flaky { inner: MemStorage::default(), puts: 0, left: None }
    }
}

impl Storage for Flaky {
    fn get(&self, key: &str) -> io::Result<Option<Vec<u8>>> {
        self.inner.get(key)
    }
    fn put(&mut self, key: &str, value: &[u8]) -> io::Result<()> {
        if let Some(n) = &mut self.left {
            if *n == 0 {
                return Err(io::Error::other("обрыв записи"));
            }
            *n -= 1;
        }
        self.puts += 1;
        self.inner.put(key, value)
    }
    fn delete(&mut self, key: &str) -> io::Result<()> {
        if self.left == Some(0) {
            return Err(io::Error::other("обрыв записи"));
        }
        self.inner.delete(key)
    }
    fn list(&self, prefix: &str) -> io::Result<Vec<String>> {
        self.inner.list(prefix)
    }
}

#[test]
fn save_dirty_writes_changes_only() {
    let mut g = scene();
    g.world_edit(2, 101, 2, cell_of(CHEST, F_SOUTH as u16));
    let mut st = Flaky::new();
    let mut sv = Saver::new();
    let r = sv.save_all(&g.server.sim, &mut st).unwrap();
    assert_eq!((r.puts, r.deletes), (st.inner.map.len(), 0));
    assert!(!st.inner.map.contains_key(JOURNAL), "журнал после записи убран");
    // Ничего не менялось — ничего не пишется.
    assert_eq!(sv.save_dirty(&g.server.sim, &mut st).unwrap(), SaveReport::default());
    // Одна правка — её кусок и meta (счётчик правок).
    g.world_edit(40, 120, 40, PLANKS as Cell);
    let before = st.inner.map.clone();
    let r = sv.save_dirty(&g.server.sim, &mut st).unwrap();
    let changed: Vec<&String> = st.inner.map.iter().filter(|(k, v)| before.get(*k) != Some(v)).map(|(k, _)| k).collect();
    assert_eq!(changed, vec!["chunk/2,2", "meta"]);
    assert_eq!(r.puts, 2);
    // Сундук тронули через окно — кусок сундука.
    g.sw().d.borrow_mut().be_mut().container([2, 101, 2]).unwrap().set(5, ItemStack::of(I_COAL, 7));
    let before = st.inner.map.clone();
    sv.save_dirty(&g.server.sim, &mut st).unwrap();
    let changed: Vec<&String> = st.inner.map.iter().filter(|(k, v)| before.get(*k) != Some(v)).map(|(k, _)| k).collect();
    assert_eq!(changed, vec!["chunk/0,0"]);
    // Предмет (в куске (−2, 0), где больше ничего нет) ушёл в кусок (1, 0): первый удалён, второй записан.
    {
        let s = &mut g.server.sim.sim;
        s.items.drop_from_block([-20, 101, 0], ItemStack::of(I_STICK, 1), &mut s.rng, &mut s.ids);
    }
    sv.save_dirty(&g.server.sim, &mut st).unwrap();
    assert!(st.inner.map.contains_key("chunk/-2,0"));
    g.server.sim.sim.items.list[0].pos = [20.5, 101.0, 0.5];
    let r = sv.save_dirty(&g.server.sim, &mut st).unwrap();
    assert!(!st.inner.map.contains_key("chunk/-2,0"), "кусок без предмета удалён");
    assert!(st.inner.map.contains_key("chunk/1,0"));
    assert_eq!((r.puts, r.deletes), (1, 1));
    // Правка вернулась к генератору — кусок пуст и удаляется.
    g.world_edit(40, 120, 40, AIR as Cell);
    let r = sv.save_dirty(&g.server.sim, &mut st).unwrap();
    assert!(!st.inner.map.contains_key("chunk/2,2"));
    assert_eq!(r.deletes, 1);
    // Игрок пошёл — пишется только игрок (сдвиг — команда отладки: до сервера — на границе тика).
    g.teleport(1.5, 101.0, 1.5);
    g.sync();
    let before = st.inner.map.clone();
    sv.save_dirty(&g.server.sim, &mut st).unwrap();
    let changed: Vec<&String> = st.inner.map.iter().filter(|(k, v)| before.get(*k) != Some(v)).map(|(k, _)| k).collect();
    assert_eq!(changed, vec!["player/тестер"]);
    // Загрузка видит последнее.
    let (g2, _) = load(&mut st, NICK).unwrap();
    assert_same(&g, &g2, "после автосохранений");
}

#[test]
fn torn_write_recovers_whole() {
    // Обрыв посреди записи пачки: журнал записан — загрузка дописывает всё (новое состояние); журнал не записан —
    // в хранилище целиком прежнее.
    for left in [0, 1, 2, 3] {
        let mut g = busy();
        let mut st = Flaky::new();
        let mut sv = Saver::new();
        sv.save_all(&g.server.sim, &mut st).unwrap();
        let old = load(&mut MemStorage { map: st.inner.map.clone() }, NICK).unwrap().0;
        for _ in 0..40 {
            g.tick(); // песок лёг, печь дальше, предметы двигались
        }
        g.world_edit(-2, 101, -2, PLANKS as Cell);
        st.left = Some(left);
        assert!(sv.save_dirty(&g.server.sim, &mut st).is_err(), "обрыв после {left}");
        assert!(sv.full, "после ошибки — полная запись");
        st.left = None;
        let (back, _) = load(&mut st, NICK).unwrap();
        if left == 0 {
            assert_same(&old, &back, "журнал не записан — прежнее");
        } else {
            assert_same(&g, &back, &format!("журнал записан, обрыв после {left}"));
        }
        assert!(!st.inner.map.contains_key(JOURNAL));
        // Следующая запись — полная и проходит.
        assert!(sv.save_dirty(&g.server.sim, &mut st).is_ok());
        assert_same(&g, &load(&mut st, NICK).unwrap().0, "после полной записи");
    }
}

#[test]
fn carried_items_return_to_inventory() {
    // Окно открыто, на курсоре стек, в сетке 2×2 — ещё один: в записи они в инвентаре (как при закрытии окна).
    let mut g = scene();
    g.open_screen(Menu::player());
    g.on_server(|sv, i| {
        let m = sv.players[i].screen.as_mut().unwrap();
        m.cursor = ItemStack::of(I_COAL, 9);
        m.grid.cells[0] = ItemStack::of(LOG as ItemId, 2);
    });
    let mut st = saved(&g);
    let (g2, _) = load(&mut st, NICK).unwrap();
    assert!(g2.ctl().screen.is_none() && g2.me().screen.is_none());
    let n = |it: ItemId| g2.ctl().inv.main.iter().filter(|s| s.item == it).map(|s| s.n()).sum::<i32>();
    assert_eq!((n(I_COAL), n(LOG as ItemId)), (9, 2));
}

#[test]
fn matching_edits_are_dropped() {
    // Разлив воды и возврат: вода по 20 клеткам воздуха над землёй, потом воздух обратно — правок нет, кусок не
    // числится правленым, пачка воркерам пуста, в хранилище куска нет.
    let mut g = scene();
    let w = g.sw(); // мир сервера (без зеркала: сырая запись — только в него)
    let ground = |x: i32, z: i32| (0..H).rev().find(|&y| w.get_block(x, y, z) != AIR).unwrap();
    let (x0, z0) = (40, 40); // кусок (2, 2), вдали от пола сцены
    let mut cells = Vec::new();
    for i in 0..20 {
        let (x, z) = (x0 + i % 5, z0 + i / 5);
        let y = ground(x, z) + 1;
        assert_eq!(w.get_block(x, y, z), AIR);
        cells.push([x, y, z]);
        w.set_block(x, y, z, WATER);
    }
    assert!(w.d.borrow().has_edits(2, 2));
    for p in &cells {
        w.set_block(p[0], p[1], p[2], AIR);
    }
    assert!(!w.d.borrow().has_edits(2, 2), "вода ушла — правок нет");
    assert!(w.tile_edits(0, 0, 4).chunks(4).all(|e| e[0] < 32 || e[2] < 32), "пачка без следов разлива");
    // Блок сломан и поставлен назад по правилам игры (камень глубоко — без соседей с опорой); ячейка с другим
    // состоянием — правка.
    let deep = w.get_cell(x0, 5, z0);
    assert_ne!(block_of(deep), AIR);
    g.world_edit(x0, 5, z0, AIR as Cell);
    assert_eq!(w.d.borrow().chunk_edits(2, 2).len(), 1);
    g.world_edit(x0, 5, z0, deep);
    assert!(!w.d.borrow().has_edits(2, 2), "поставлен назад — правки нет");
    let y = ground(x0, z0);
    let top = w.get_cell(x0, y, z0);
    w.set_cell(x0, y, z0, cell_of(block_of(top), 3));
    assert_eq!(w.d.borrow().chunk_edits(2, 2).len(), 1, "то же, но с состоянием — правка");
    w.set_cell(x0, y, z0, top);
    // Правка из хранилища (ячейка генератора неизвестна до загрузки области): возврат тоже её убирает.
    let mut sv = Saver::new();
    let mut st = MemStorage::default();
    w.set_block(x0, y, z0, PLANKS);
    sv.save_all(&g.server.sim, &mut st).unwrap();
    assert!(st.map.contains_key("chunk/2,2"));
    let (g2, mut sv2) = Saver::load(&mut st, NICK).unwrap();
    let w2 = g2.sim.world.share();
    assert_eq!(w2.get_block(x0, y, z0), PLANKS);
    w2.set_cell(x0, y, z0, top);
    assert!(!w2.d.borrow().has_edits(2, 2));
    sv2.save_dirty(&g2, &mut st).unwrap();
    assert!(!st.map.contains_key("chunk/2,2"), "пустой кусок удалён из хранилища");
    // Счётчик правок — «сколько сделано», не «сколько лежит»: JS по нему шлёт воркерам пачки.
    assert!(w.edit_count() > 40);
}

#[test]
fn file_storage_in_temp_dir() {
    let dir = std::env::temp_dir().join(format!("voxel-f2-{}-{:x}", std::process::id(), hash(b"file_storage_in_temp_dir")));
    let _ = std::fs::remove_dir_all(&dir);
    let mut fs = storage::FileStorage::open(&dir).unwrap();
    // Ключи → файлы внутри папки, туда и обратно.
    for key in ["meta", "chunk/-3,5", "player/вася", "player/Вася", "player/con", "player/..", "a/../../b", "player/й ё.x"] {
        let p = fs.path(key).unwrap();
        assert!(p.starts_with(&dir), "{key} → {p:?}");
        fs.put(key, key.as_bytes()).unwrap();
        assert_eq!(fs.get(key).unwrap().as_deref(), Some(key.as_bytes()), "{key}");
    }
    assert_ne!(fs.path("player/вася").unwrap(), fs.path("player/Вася").unwrap());
    assert!(fs.path("player/вася").unwrap().ends_with("вася"), "строчная кириллица — как есть");
    assert!(fs.path("player/con").unwrap().ends_with("%63on"), "имя устройства Windows");
    assert!(fs.path("player/..").unwrap().ends_with("%2E%2E"));
    assert_eq!(fs.list("player/").unwrap(), vec!["player/..", "player/con", "player/Вася", "player/вася", "player/й ё.x"]);
    assert_eq!(fs.list("chunk/").unwrap(), vec!["chunk/-3,5"]);
    assert_eq!(fs.list("").unwrap().len(), 8);
    // Перезапись — целиком; временный файл после сбоя list не видит; удаление; отсутствующее — None.
    fs.put("chunk/-3,5", &[1, 2, 3]).unwrap();
    assert_eq!(fs.get("chunk/-3,5").unwrap(), Some(vec![1, 2, 3]));
    std::fs::write(dir.join("chunk").join("7,7.tmp"), b"half").unwrap();
    std::fs::write(dir.join("chunk").join("Junk"), b"x").unwrap();
    assert_eq!(fs.list("chunk/").unwrap(), vec!["chunk/-3,5"]);
    fs.delete("chunk/-3,5").unwrap();
    fs.delete("chunk/-3,5").unwrap();
    assert_eq!(fs.get("chunk/-3,5").unwrap(), None);
    assert!(fs.put("", b"").is_err() && fs.put("a//b", b"").is_err() && fs.put("a/b/c/d/e", b"").is_err());
    assert!(fs.put("bad\nkey", b"").is_err());
    // Мир целиком через файлы — сохранить, «остановить», открыть заново, загрузить (MP-2).
    let _ = std::fs::remove_dir_all(&dir);
    let g = busy();
    let mut fs = storage::FileStorage::open(&dir).unwrap();
    Saver::new().save_all(&g.server.sim, &mut fs).unwrap();
    drop(fs);
    let mut fs = storage::FileStorage::open(&dir).unwrap();
    let (g2, _) = load(&mut fs, NICK).unwrap();
    assert_same(&g, &g2, "через файлы");
    assert_eq!(saved(&g2).map, saved(&g).map);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn key_names() {
    for p in ["meta", "-3,5", "вася", "Вася", "con", "com1", "..", "й", "ё", "a b", "%", "x.tmp"] {
        let e = encode_part(p);
        assert!(!e.contains('.') && !e.contains('/') && !e.contains(' '), "{p} → {e}");
        assert_eq!(decode_part(&e).as_deref(), Some(p), "{p} → {e}");
    }
    assert_eq!(encode_part("Вася"), "%D0%92ася");
    assert_eq!((decode_part("x.tmp"), decode_part("%61"), decode_part("%4a"), decode_part("")), (None, None, None, None));
    assert_eq!(parse_chunk_key("chunk/-3,5"), Some((-3, 5)));
    assert_eq!((parse_chunk_key("chunk/+3,5"), parse_chunk_key("chunk/3"), parse_chunk_key("chunk/03,5")), (None, None, None));
    assert_eq!(player_key("Вася").unwrap(), "player/вася");
    assert!(player_key("").is_err() && player_key("a/b").is_err() && player_key(&"я".repeat(33)).is_err());
}

#[test]
fn bad_data_is_an_error() {
    let g = scene();
    let st = saved(&g);
    // Нет мира.
    assert!(matches!(Saver::load(&mut MemStorage::default(), NICK), Err(SaveError::NoWorld)));
    // Другой генератор.
    let mut s = MemStorage { map: st.map.clone() };
    let (_, rest) = version(&s.map["meta"]).unwrap();
    let mut meta: MetaV1 = body(rest).unwrap();
    meta.gen_version += 1;
    s.map.insert("meta".into(), encode(META_V, &meta));
    assert!(matches!(Saver::load(&mut s, NICK), Err(SaveError::GenVersion { .. })));
    // Версия значения новее сборки.
    let mut s = MemStorage { map: st.map.clone() };
    let key = s.map.keys().find(|k| k.starts_with("chunk/")).unwrap().clone();
    let (_, rest) = version(&s.map[&key]).unwrap();
    let mut v = postcard::to_allocvec(&(CHUNK_V + 1)).unwrap();
    v.extend_from_slice(rest);
    s.map.insert(key.clone(), v);
    assert!(matches!(Saver::load(&mut s, NICK), Err(SaveError::Version { .. })));
    // Обрезанное и мусор — ошибка разбора, не паника.
    for cut in [1usize, 3, 10] {
        let mut s = MemStorage { map: st.map.clone() };
        let v = s.map[&key].clone();
        s.map.insert(key.clone(), v[..v.len().saturating_sub(cut)].to_vec());
        assert!(matches!(Saver::load(&mut s, NICK), Err(SaveError::Corrupt { .. })), "обрезано на {cut}");
    }
    let mut s = MemStorage { map: st.map.clone() };
    s.map.insert(key, vec![1, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
    assert!(Saver::load(&mut s, NICK).is_err());
    let mut s = MemStorage { map: st.map.clone() };
    s.map.insert("chunk/nope".into(), vec![1]);
    assert!(matches!(Saver::load(&mut s, NICK), Err(SaveError::BadKey(_))));
}

#[test]
fn sizes() {
    // Размеры типичных значений (для отчёта; cargo test sizes -- --nocapture).
    let mut g = scene();
    let st0 = saved(&g);
    // Домик 5×5, стены 3 в высоту из досок с проёмом, пол на земле, печь и сундук с 10 стеками: кусок (2, 2).
    let w = g.sw();
    let (x0, z0) = (36, 36);
    let y0 = (0..H).rev().find(|&y| w.get_block(x0 + 2, y, z0 + 2) != AIR).unwrap() + 1;
    let mut n = 0;
    for dy in 0..3 {
        for dx in 0..5 {
            for dz in 0..5 {
                let edge = dx == 0 || dx == 4 || dz == 0 || dz == 4;
                if edge && !(dx == 2 && dz == 0 && dy < 2) {
                    g.world_edit(x0 + dx, y0 + dy, z0 + dz, PLANKS as Cell);
                    n += 1;
                }
            }
        }
    }
    g.world_edit(x0 + 1, y0, z0 + 3, cell_of(FURNACE, F_SOUTH as u16));
    g.world_edit(x0 + 3, y0, z0 + 3, cell_of(CHEST, F_NORTH as u16));
    {
        let mut wd = w.d.borrow_mut();
        let c = wd.be_mut().chest_mut([x0 + 3, y0, z0 + 3]).unwrap();
        for i in 0..10 {
            c.set(i, ItemStack::of(COBBLE as ItemId + (i % 3) as ItemId, 64));
        }
    }
    // Шахта 2×1 вниз на 16 — воздух вместо камня: кусок (−3, −3).
    let (mx, mz) = (-40, -40);
    let top = (0..H).rev().find(|&y| w.get_block(mx, y, mz) != AIR).unwrap();
    for dy in 0..16 {
        for dx in 0..2 {
            w.set_block(mx + dx, top - dy, mz, AIR);
        }
    }
    g.kit();
    g.sync();
    let st = saved(&g);
    let len = |k: &str| st.map.get(k).map_or(0, |v| v.len());
    println!(
        "размеры: meta {} Б; домик ({n} досок, печь, сундук с 10 стеками) {} Б; шахта (32 клетки воздуха) {} Б; \
         кусок пола сцены (5×5 камня) {} Б; игрок с набором K ({} стеков) {} Б, пустой игрок {} Б",
        len("meta"),
        len("chunk/2,2"),
        len("chunk/-3,-3"),
        len("chunk/0,0"),
        g.ctl().inv.main.iter().filter(|s| !s.is_empty()).count(),
        len("player/тестер"),
        st0.map["player/тестер"].len()
    );
    assert!(len("chunk/2,2") < 1024 && len("chunk/-3,-3") < 256 && len("player/тестер") < 512);
}

/// Запись игрока версии 4 несёт хеш токена и режим игры (G17); записи версий 1–3 читаются (токена нет — ник закрепится за первым, кто войдёт
/// с токеном); порченая запись не мешает загрузке остальных (load_all), а по имени — ошибка (load).
#[test]
fn player_record_versions_and_token() {
    use crate::game::server::ServerPlayer;
    use crate::save::token::token_hash;
    let mut sim = ServerSim::new(World::new(1337));
    let mut p = ServerPlayer::new(7, "Вася");
    p.token = token_hash("вася", &[5; 16]);
    *p.inv.at(3) = ItemStack::of(I_COAL, 9);
    p.player.pos = [1.5, 70.0, -2.5];
    crate::game::rules::apply_mode(&mut p.player, crate::game::rules::GameMode::Spectator);
    sim.parked.push(p);
    let mut st = MemStorage::default();
    Saver::new().save_all(&sim, &mut st).unwrap();
    assert_eq!(version(&st.map["player/вася"]).unwrap().0, 4, "запись игрока — версия 4");
    // Версия 4 туда-обратно: токен, номер сущности, инвентарь, позиция, режим (наблюдатель — снова сквозь стены).
    let (g, _) = Saver::load_all(&mut st).unwrap();
    assert_eq!(g.parked.len(), 1);
    let v = &g.parked[0];
    assert_eq!((v.id, v.token, v.player.pos, v.inv.get(3)), (7, token_hash("вася", &[5; 16]), [1.5, 70.0, -2.5], ItemStack::of(I_COAL, 9)));
    assert_eq!((v.player.mode, v.player.noclip), (crate::game::rules::GameMode::Spectator, true));

    // Записи прежних версий (как их писали сборки до токенов) и порча.
    let old = PlayerV1 {
        nick: "Старик".into(),
        pos: [3.0, 64.0, 3.0],
        vel: [0.0; 3],
        yaw: 90.0,
        pitch: 0.0,
        on_ground: true,
        fall: 0.0,
        flying: false,
        crouch: false,
        selected: 2,
        inv: vec![(2, StackV1 { item: I_STICK, count: 4, damage: 0 })],
        carried: vec![],
    };
    st.put("player/старик", &encode(1, &old)).unwrap();
    st.put("player/дед", &encode(2, &PlayerV2 { id: 9, p: PlayerV1 { nick: "Дед".into(), ..old.clone() } })).unwrap();
    st.put("player/третий", &encode(3, &PlayerV3 { id: 11, token: None, p: PlayerV1 { nick: "Третий".into(), ..old.clone() } })).unwrap();
    st.put("player/мусор", &[0xFF, 0xFF, 0xFF]).unwrap();
    let (g, sv) = Saver::load_all(&mut st).expect("порченая запись игрока не кладёт загрузку");
    let by = |n: &str| g.parked.iter().find(|p| p.nick == n).unwrap_or_else(|| panic!("нет {n}"));
    assert_eq!((by("Старик").token, by("Старик").inv.get(2), by("Старик").player.pos), (None, ItemStack::of(I_STICK, 4), [3.0, 64.0, 3.0]));
    assert_eq!((by("Дед").token, by("Дед").id), (None, 9), "версия 2: номер сущности есть, токена нет");
    assert_eq!((by("Третий").id, by("Третий").player.mode), (11, crate::game::rules::GameMode::Survival), "версия 3: режим — выживание");
    assert_eq!(by("Вася").token, token_hash("вася", &[5; 16]));
    assert_eq!(sv.unreadable.len(), 1, "порченая запись пропущена и названа: {:?}", sv.unreadable);
    assert!(sv.unreadable[0].contains("player/мусор"), "{:?}", sv.unreadable);
    assert!(Saver::load(&mut st, "мусор").is_err(), "по имени порча — ошибка, как раньше");
    assert!(st.map.contains_key("player/мусор"), "файл порченой записи не тронут");
    // Запись новее сборки — пропуск с названной причиной, не молчаливая перезапись.
    st.put("player/будущий", &encode(9, &old)).unwrap();
    let (_, sv) = Saver::load_all(&mut st).unwrap();
    assert_eq!(sv.unreadable.len(), 2);

    // Старая запись при следующей записи становится версией 4 (токен закрепится при входе).
    let (g, mut sv) = Saver::load_all(&mut st).unwrap();
    let mut out = MemStorage::default();
    sv.save_all(&g, &mut out).unwrap();
    assert_eq!(version(&out.map["player/дед"]).unwrap().0, 4);
}

#[test]
fn machine_in_item_survives_save() {
    // G9: данные машины в предмете (номер в damage стека, ключ mek_items) — после записи и загрузки та же запись.
    use crate::registry::blocks::MekKind;
    use crate::world::mek::MekBe;
    let mut g = scene();
    let mut m = MekBe::new(MekKind::Enrich);
    m.energy = 12_345;
    m.slots[0] = ItemStack::of(I_RAW_IRON, 5);
    let id = g.server.sim.sim.mek_items.stash(&m);
    let mut st = saved(&g);
    assert!(st.map.contains_key(super::MEK_ITEMS));
    let (g2, _) = load(&mut st, NICK).unwrap();
    assert_eq!(g2.sim().mek_items, g.sim().mek_items);
    let mut n = MekBe::new(MekKind::Enrich);
    g2.sim().mek_items.get(id).unwrap().apply(&mut n);
    assert_eq!((n.energy, n.slots[0]), (12_345, ItemStack::of(I_RAW_IRON, 5)));
    assert_eq!(saved(&g2).map, st.map);
}

#[test]
fn machine_sides_upgrades_survive_save() {
    // G11: стороны, выталкивание и улучшения — в машине (запись MekM3) и в предмете (ключ mek_items3).
    use crate::registry::blocks::MekKind;
    use crate::world::mek::{MekBe, UP_ENERGY, UP_SPEED};
    let mut g = scene();
    let p = [2, 70, 2];
    {
        let mut w = g.server.sim.sim.world.d.borrow_mut();
        w.set_cell(p[0], p[1], p[2], crate::cell::cell_of(crate::registry::ids::ENRICHMENT_CHAMBER, 3));
        let m = w.be_mut().mek_mut(p).unwrap();
        m.sides = [2, 1, 3, 4, 0, 1];
        m.eject = true;
        m.upgrades = [4, 2, 0];
        m.energy = 30_000;
    }
    let mut it = MekBe::new(MekKind::Smelter);
    it.upgrades = [1, 0, 0];
    it.eject = true;
    let id = g.server.sim.sim.mek_items.stash(&it);
    assert_ne!(id, 0, "одни улучшения — уже данные предмета");
    let mut st = saved(&g);
    let (g2, _) = load(&mut st, NICK).unwrap();
    let w = g2.sim().world.d.borrow();
    let m = w.be().mek(p).unwrap();
    assert_eq!((m.sides, m.eject, m.upgrades, m.energy), ([2, 1, 3, 4, 0, 1], true, [4, 2, 0], 30_000));
    assert_eq!(m.upgrades[UP_SPEED] + m.upgrades[UP_ENERGY], 6);
    let d = g2.sim().mek_items.get(id).unwrap();
    assert_eq!((d.upgrades, d.eject), ([1, 0, 0], true));
}

#[test]
fn separator_purifier_gas_survive_save() {
    // G12: баки газов сепаратора и их режимы, вода, улучшения газа и израсходованный кислород очистителя — в машине
    // (запись MekM4) и в предмете (ключ mek_items4).
    use crate::registry::blocks::MekKind;
    use crate::registry::ids::{ELECTROLYTIC_SEPARATOR, PURIFICATION_CHAMBER};
    use crate::world::mek::{recipes::*, MekBe, Tank};
    let mut g = scene();
    let (a, b) = ([2, 70, 2], [3, 70, 2]);
    {
        let mut w = g.server.sim.sim.world.d.borrow_mut();
        w.set_cell(a[0], a[1], a[2], crate::cell::cell_of(ELECTROLYTIC_SEPARATOR, 3));
        w.set_cell(b[0], b[1], b[2], crate::cell::cell_of(PURIFICATION_CHAMBER, 3));
        let m = w.be_mut().mek_mut(a).unwrap();
        m.tank = Tank { sub: SUB_WATER, amount: 7_000 };
        m.gas = [Tank { sub: SUB_HYDROGEN, amount: 300 }, Tank { sub: SUB_OXYGEN, amount: 150 }];
        m.gas_mode = [2, 1];
        let m = w.be_mut().mek_mut(b).unwrap();
        m.upgrades = [3, 0, 2];
        m.gas_used = 77;
        m.progress = 80;
        m.tank = Tank { sub: SUB_OXYGEN, amount: 99 };
    }
    let mut it = MekBe::new(MekKind::Separator);
    it.gas = [Tank { sub: SUB_HYDROGEN, amount: 40 }, Tank::default()];
    it.gas_mode = [1, 0];
    let id = g.server.sim.sim.mek_items.stash(&it);
    assert_ne!(id, 0, "газ в баке — уже данные предмета");
    let mut st = saved(&g);
    let (g2, _) = load(&mut st, NICK).unwrap();
    let w = g2.sim().world.d.borrow();
    let m = w.be().mek(a).unwrap();
    assert_eq!((m.kind, m.tank.amount, m.gas[0].amount, m.gas[1].sub, m.gas_mode), (MekKind::Separator, 7_000, 300, SUB_OXYGEN, [2, 1]));
    let m = w.be().mek(b).unwrap();
    assert_eq!((m.kind, m.upgrades, m.gas_used, m.progress, m.tank.amount), (MekKind::Purifier, [3, 0, 2], 77, 80, 99));
    let d = g2.sim().mek_items.get(id).unwrap();
    assert_eq!((d.gas[0].amount, d.gas_mode), (40, [1, 0]));
}
