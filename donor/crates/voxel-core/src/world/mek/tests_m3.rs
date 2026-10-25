//! Эталоны Mekanism M3 «Ветер и склад» (G11): ветряк (MEK-03 §12: M03-12…15), планшет энергии (MEK-12 §12: M12-3, M12-9),
//! многоблок 1 × 5 (MEK-03 §3.5). cargo test -p voxel-core mek.

use super::*;
use crate::cell::{block_of, cell_of, state_of, F_DOWN, F_SOUTH};
use crate::game::sim::Sim;
use crate::game::view::Events;
use crate::registry::blocks::{bdef, main_of};
use crate::registry::ids::*;
use crate::registry::items::*;
use crate::world::ticks::Zone;
use crate::world::World;

/// Мир-заглушка: воздух, кроме заданных клеток; сток «берёт всё» на заданных гранях.
#[derive(Default)]
struct Air {
    blocks: Vec<(Pos, Cell)>,
    sink_faces: Vec<u8>,
    taken: Vec<(u8, u64)>,
}

impl MekWorld for Air {
    fn cell(&mut self, p: Pos) -> Cell {
        self.blocks.iter().find(|(q, _)| *q == p).map_or(AIR as Cell, |(_, c)| *c)
    }
    fn push_energy(&mut self, _p: Pos, f: u8, n: u64) -> u64 {
        if self.sink_faces.contains(&f) {
            self.taken.push((f, n));
            n
        } else {
            0
        }
    }
}

#[test]
fn wind_power_by_height_m03_12() {
    // M03-12: основание Y → Дж/т (мир 0–127, minY 24).
    for (y, j) in [(20, 60), (44, 157), (49, 178), (60, 223), (63, 235), (64, 239), (100, 386), (105, 406), (120, 467), (123, 480)] {
        assert_eq!(wind_power(y), j, "основание {y}");
    }
}

#[test]
fn wind_tick_sky_m03_14_15() {
    // M03-14: буфер пуст, на t5 блок над верхом — выработка держится до пересчёта (ticker 20), затем 0 и «не активна»;
    // блок убрали на t30 → с t41 снова 178.
    let p = [0, 49, 0];
    let mut m = MekBe::new(MekKind::Wind);
    let mut w = Air::default();
    let mut got = Vec::new();
    for t in 1..=45 {
        if t == 5 {
            w.blocks.push(([0, 49 + 10, 0], STONE as Cell));
        }
        if t == 30 {
            w.blocks.clear();
        }
        let before = m.energy;
        m.tick(p, F_SOUTH, &mut w);
        got.push((t, m.energy - before, m.active.logic));
    }
    for &(t, d, on) in &got {
        let want = if (21..=40).contains(&t) { 0 } else { 178 };
        assert_eq!(d, want, "t{t}: {got:?}");
        assert_eq!(on, want > 0, "t{t}");
    }
    assert_eq!(m.tip(2), "Ветер есть");
    // Листва над верхом — небо закрыто (небесный свет 14); стекло — нет.
    let mut leafy = Air { blocks: vec![([0, 70, 0], LEAVES as Cell)], ..Default::default() };
    assert!(!sky_open(&mut leafy, [0, 53, 0]));
    let mut glass = Air { blocks: vec![([0, 70, 0], GLASS as Cell)], ..Default::default() };
    assert!(sky_open(&mut glass, [0, 53, 0]));
    // M03-15: буфер полон — «активна», выработка в окне 178, буфер не растёт; отдача 960 с лица и низа.
    let mut m = MekBe::new(MekKind::Wind);
    m.energy = 200_000;
    let mut w = Air::default();
    m.tick(p, F_SOUTH, &mut w);
    assert_eq!((m.energy, m.produced, m.active.logic), (200_000, 178, true));
    assert_eq!(m.def().max_out, 960);
    let mut w = Air { sink_faces: vec![F_DOWN], ..Default::default() };
    m.tick(p, F_SOUTH, &mut w);
    assert_eq!(w.taken, vec![(F_DOWN, 960)], "лицо не берёт — всё вниз");
    assert_eq!(energy_io(MekKind::Wind, F_SOUTH, F_DOWN), (false, true));
    assert_eq!(energy_io(MekKind::Wind, F_SOUTH, crate::cell::F_EAST), (false, false));
    assert!(m.tip(0).contains("Вырабатывает: 178 Дж/т"), "{}", m.tip(0));
    assert_eq!(m.tip(1), "Выработка: 178 Дж/т");
}

#[test]
fn tablet_slots_m12_3_9() {
    // M12-9: полный планшет в слоте энергии пустой камеры — 5 000 за тик, камера полна на t4.
    let mut m = MekBe::new(MekKind::Enrich);
    m.slots[CH_ENERGY] = with_energy(ItemStack::of(I_ENERGY_TABLET, 1), TABLET_CAP);
    let mut w = Air::default();
    for t in 1..=4 {
        m.tick([0, 0, 0], F_SOUTH, &mut w);
        assert_eq!(m.energy, 5000 * t);
    }
    assert_eq!(stack_energy(&m.slots[CH_ENERGY]), TABLET_CAP - 20_000);
    // M12-3: куб 4 МДж, пустой планшет в «+» — +4 000 за тик, полон после t250.
    let mut c = MekBe::new(MekKind::Cube);
    c.energy = c.def().cap;
    c.slots[1] = ItemStack::of(I_ENERGY_TABLET, 1);
    let mut full_at = 0;
    for t in 1..=260 {
        c.tick([0, 0, 0], F_SOUTH, &mut w);
        if full_at == 0 && stack_energy(&c.slots[1]) == TABLET_CAP {
            full_at = t;
        }
    }
    assert_eq!(full_at, 250);
    // Ветряк заряжает планшет своей выработкой; планшет в «−» куба отдаёт ≤ 4 000 за тик.
    let mut g = MekBe::new(MekKind::Wind);
    g.slots[0] = ItemStack::of(I_ENERGY_TABLET, 1);
    for _ in 0..3 {
        g.tick([0, 49, 0], F_SOUTH, &mut w);
    }
    assert_eq!(stack_energy(&g.slots[0]), 340, "178 + 178 за два тика, кратно 20");
    let mut c = MekBe::new(MekKind::Cube);
    c.slots[0] = with_energy(ItemStack::of(I_ENERGY_TABLET, 1), 10_000);
    c.tick([0, 0, 0], F_SOUTH, &mut w);
    assert_eq!(c.energy, 4000);
    // Слоты: планшет — в зарядку и в энергию, редстоун — только в энергию.
    assert!(role_accepts(MekKind::Wind, Role::Charge, I_ENERGY_TABLET));
    assert!(!role_accepts(MekKind::Wind, Role::Charge, I_REDSTONE));
    assert!(role_accepts(MekKind::Enrich, Role::Energy, I_ENERGY_TABLET));
    assert_eq!(idef(I_ENERGY_TABLET).max_stack, 1);
}

fn sim() -> (Sim, World, Zone) {
    let world = World::new(1);
    let s = Sim::new(&world, 1);
    let mut zone = Zone::default();
    zone.add_square(0, 0, 4);
    (s, world, zone)
}

#[test]
fn wind_multiblock_parts_and_break() {
    // Многоблок 1 × 5: основание и 4 части; часть указывает на основание; замена части ломает ветряк с дропом.
    let (mut s, world, zone) = sim();
    let mut ev = Events::default();
    let b = [3, 100, 3];
    {
        let d = world.d.clone();
        let mut w = d.borrow_mut();
        s.set_world(&mut w, b, cell_of(WIND_GENERATOR, F_SOUTH as u16), &mut ev);
        for dy in 1..=4 {
            s.set_world(&mut w, [b[0], b[1] + dy, b[2]], cell_of(BOUNDING, dy as u16), &mut ev);
        }
        let top = w.get_cell(b[0], b[1] + 4, b[2]);
        assert_eq!(main_of(top, [b[0], b[1] + 4, b[2]]), b);
        assert_eq!(bdef(BOUNDING).on_use, crate::registry::blocks::OnUse::Machine);
    }
    for _ in 0..40 {
        s.tick_world(zone.clone(), &mut ev);
    }
    {
        let mut w = world.d.borrow_mut();
        let m = w.be().mek(b).unwrap();
        assert_eq!(m.energy, 40 * wind_power(100));
        assert_ne!(state_of(w.get_cell(b[0], b[1], b[2])) & crate::registry::blocks::ST_LIT, 0, "крутится — «активна»");
    }
    // Часть заменили камнем — основание ломается (дроп — ветряк с энергией), остальные части снимаются.
    {
        let d = world.d.clone();
        let mut w = d.borrow_mut();
        s.set_world(&mut w, [b[0], b[1] + 2, b[2]], STONE as Cell, &mut ev);
        assert_eq!(block_of(w.get_cell(b[0], b[1], b[2])), AIR);
        for dy in [1, 3, 4] {
            assert_eq!(block_of(w.get_cell(b[0], b[1] + dy, b[2])), AIR, "часть {dy}");
        }
    }
    let drops: Vec<_> = s.items.list.iter().map(|e| e.stack).collect();
    assert_eq!(drops.len(), 1, "{drops:?}");
    assert_eq!(drops[0].item, WIND_GENERATOR as ItemId);
    assert_eq!(s.mek_items.get(drops[0].damage).unwrap().energy, 40 * wind_power(100));
}

// ---------------------------------------------------------------- стороны и выталкивание (MEK-09 §14)

use super::sides::*;
use crate::cell::{F_EAST, F_NORTH, F_UP, F_WEST};

#[test]
fn sides_dirs_modes_defaults_m89_1_3_10() {
    // M89-1: таблица §5.2 и обратимость.
    for f in 0..6u8 {
        for r in 0..6 {
            assert_eq!(rel_of(f, dir_of(f, r)), r);
        }
    }
    assert_eq!([R_FRONT, R_LEFT, R_RIGHT, R_BACK].map(|r| dir_of(F_NORTH, r)), [F_NORTH, F_EAST, F_WEST, F_SOUTH]);
    assert_eq!([R_LEFT, R_RIGHT, R_TOP, R_BOTTOM].map(|r| dir_of(F_UP, r)), [F_EAST, F_WEST, F_SOUTH, F_NORTH]);
    // M89-2, M89-3: перебор режимов.
    let mut m = S_NONE;
    let mut seen = Vec::new();
    for _ in 0..5 {
        m = cycle_mode(MekKind::Enrich, m, false);
        seen.push(m);
    }
    assert_eq!(seen, vec![S_IN, S_OUT, S_IO, S_ENERGY, S_NONE]);
    assert_eq!(cycle_mode(MekKind::Enrich, S_NONE, true), S_ENERGY);
    assert_eq!((cycle_mode(MekKind::Infuser, S_ENERGY, false), cycle_mode(MekKind::Infuser, S_EXTRA, false)), (S_EXTRA, S_NONE));
    // M89-10: новая камера с лицом N — W выход, S энергия, остальные вход.
    let c = MekBe::new(MekKind::Enrich);
    let by_dir: Vec<u8> = (0..6u8).map(|d| c.side_at(F_NORTH, d)).collect();
    assert_eq!(by_dir, vec![S_IN, S_IN, S_IN, S_ENERGY, S_OUT, S_IN], "D U N S W E");
    assert_eq!(MekBe::new(MekKind::Infuser).sides[R_BOTTOM], S_EXTRA);
    assert_eq!(MekBe::new(MekKind::Cube).sides[R_FRONT], S_OUT);
}

/// Мир-сток для выталкивания: каждая грань берёт всё; журнал (грань, сколько).
#[derive(Default)]
struct Sink {
    log: Vec<(u8, i32)>,
}
impl MekWorld for Sink {
    fn cell(&mut self, _p: Pos) -> Cell {
        AIR as Cell
    }
    fn push_energy(&mut self, _p: Pos, _f: u8, _n: u64) -> u64 {
        0
    }
    fn push_items(&mut self, _p: Pos, f: u8, s: ItemStack) -> i32 {
        self.log.push((f, s.count));
        s.count
    }
}

#[test]
fn eject_period_m89_12_13() {
    // M89-12: включено, выход всегда полон — выталкивания в тиках 0, 11, 22, 33.
    let mut m = MekBe::new(MekKind::Enrich);
    m.eject = true;
    let mut w = Sink::default();
    let mut at = Vec::new();
    for t in 0..40 {
        m.slots[CH_OUT] = ItemStack::of(I_DUST_IRON, 64);
        let before = w.log.len();
        m.eject_items([0, 0, 0], F_NORTH, &mut w);
        if w.log.len() > before {
            at.push(t);
        }
    }
    assert_eq!(at, vec![0, 11, 22, 33]);
    assert!(w.log.iter().all(|&(f, n)| f == F_WEST && n == 64), "лицо N — правая грань W, вся стопка: {:?}", w.log);
    // M89-13: выключили перед тиком 15 — счётчик стоит; включили перед тиком 100 → 107, 118.
    let mut m = MekBe::new(MekKind::Enrich);
    let mut w = Sink::default();
    let mut at = Vec::new();
    for t in 0..120 {
        m.eject = !(15..100).contains(&t);
        m.slots[CH_OUT] = ItemStack::of(I_DUST_IRON, 64);
        let before = w.log.len();
        m.eject_items([0, 0, 0], F_NORTH, &mut w);
        if w.log.len() > before {
            at.push(t);
        }
    }
    assert_eq!(at, vec![0, 11, 107, 118]);
}

#[test]
fn eject_targets_m89_14_20() {
    use crate::world::blockentity::BlockEntity;
    let (mut s, world, _zone) = sim();
    let mut ev = Events::default();
    let y = 100;
    let ch = [0, y, 0];
    let d = world.d.clone();
    let mut w = d.borrow_mut();
    // Камера лицом на юг: правая грань — восток. Справа сундук.
    s.set_world(&mut w, ch, cell_of(ENRICHMENT_CHAMBER, F_SOUTH as u16), &mut ev);
    s.set_world(&mut w, [1, y, 0], CHEST as Cell, &mut ev);
    {
        let m = w.be_mut().mek_mut(ch).unwrap();
        assert_eq!(m.side_at(F_SOUTH, F_EAST), S_OUT);
        m.eject = true;
        m.slots[CH_OUT] = ItemStack::of(I_DUST_IRON, 64);
    }
    // M89-14/15: 64 пыли — в слот 0 сундука за одно срабатывание; слот 5 с 40 пыли первым не доливается.
    if let Some(BlockEntity::Chest(c)) = w.be_mut().get_mut([1, y, 0]) {
        c.slots[5] = ItemStack::of(I_DUST_IRON, 40);
    }
    let mut m = w.be_mut().mek_take(ch).unwrap();
    m.tick(ch, F_SOUTH, &mut *w);
    w.be_mut().mek_put(ch, m);
    let chest = |w: &mut crate::world::WorldData| match w.be_mut().get_mut([1, y, 0]) {
        Some(BlockEntity::Chest(c)) => (c.slots[0], c.slots[5]),
        _ => panic!("нет сундука"),
    };
    assert_eq!(chest(&mut w), (ItemStack::of(I_DUST_IRON, 64), ItemStack::of(I_DUST_IRON, 40)));
    // M89-19: выход камеры → плавильня. Плавильня лицом на север: её грань W (к камере) — право → выход, не принимает;
    // лицом на юг грань W — лево → вход.
    s.set_world(&mut w, [1, y, 0], AIR as Cell, &mut ev);
    s.set_world(&mut w, [1, y, 0], cell_of(ENERGIZED_SMELTER, F_NORTH as u16), &mut ev);
    w.be_mut().mek_mut(ch).unwrap().slots[CH_OUT] = ItemStack::of(I_DUST_IRON, 8);
    let push = |w: &mut crate::world::WorldData| {
        let mut m = w.be_mut().mek_take(ch).unwrap();
        m.eject_cd = 0;
        m.eject_items(ch, F_SOUTH, w);
        let left = m.slots[CH_OUT].n();
        w.be_mut().mek_put(ch, m);
        left
    };
    assert_eq!(push(&mut w), 8, "грань «выход» плавильни не принимает");
    s.set_world(&mut w, [1, y, 0], AIR as Cell, &mut ev);
    s.set_world(&mut w, [1, y, 0], cell_of(ENERGIZED_SMELTER, F_SOUTH as u16), &mut ev);
    assert_eq!(push(&mut w), 0, "грань «вход» — во вход плавильни");
    assert_eq!(w.be().mek([1, y, 0]).unwrap().slots[CH_IN], ItemStack::of(I_DUST_IRON, 8));
    // Не плавится — не принимается.
    w.be_mut().mek_mut(ch).unwrap().slots[CH_OUT] = ItemStack::of(I_STICK, 3);
    assert_eq!(push(&mut w), 3);
    // M89-18: выход камеры вниз, под ней печь — во вход печи.
    w.be_mut().mek_mut(ch).unwrap().sides[R_BOTTOM] = S_OUT;
    s.set_world(&mut w, [0, y - 1, 0], cell_of(FURNACE, F_SOUTH as u16), &mut ev);
    w.be_mut().mek_mut(ch).unwrap().slots[CH_OUT] = ItemStack::of(I_DUST_IRON, 5);
    assert_eq!(push(&mut w), 0);
    assert_eq!(w.be().furnace([0, y - 1, 0]).unwrap().slots[0], ItemStack::of(I_DUST_IRON, 5));
    // M89-20: вход-выход — уходит только выход.
    {
        let m = w.be_mut().mek_mut(ch).unwrap();
        m.sides[R_BOTTOM] = S_IO;
        m.slots[CH_IN] = ItemStack::of(I_RAW_IRON, 10);
        m.slots[CH_OUT] = ItemStack::of(I_DUST_IRON, 4);
    }
    w.be_mut().furnace_mut([0, y - 1, 0]).unwrap().slots[0] = ItemStack::EMPTY;
    assert_eq!(push(&mut w), 0);
    assert_eq!(w.be().mek(ch).unwrap().slots[CH_IN], ItemStack::of(I_RAW_IRON, 10));
}

// ---------------------------------------------------------------- улучшения (MEK-10 §5, §11)

#[test]
fn upgrade_tables_m10_1_4_6() {
    // M10-1: таблицы совпадают с формулами (f64).
    for s in 0..=8i32 {
        assert_eq!(UP_TICKS[s as usize], (200.0 * 10f64.powf(-s as f64 / 8.0)) as u32, "тики s={s}");
    }
    for k in -8..=16i32 {
        assert_eq!(UP_J50[(k + 8) as usize], (50.0 * 10f64.powf(k as f64 / 8.0) - 1e-9).ceil() as u64, "Дж/т k={k}");
    }
    for e in 0..=8i32 {
        assert_eq!(UP_BUF20K[e as usize], (20_000.0 * 10f64.powf(e as f64 / 8.0) + 1e-9) as u64, "буфер e={e}");
    }
    // M10-6: s = e = 4 — 63 тика × 159 Дж/т; e = 8, s = 0 — 5 Дж/т, буфер 200 000.
    let mut m = MekBe::new(MekKind::Enrich);
    m.install(UP_SPEED, 4);
    m.install(UP_ENERGY, 4);
    assert_eq!((m.ticks(), m.per_tick(), m.cap()), (63, 159, 63_245));
    let mut m = MekBe::new(MekKind::Enrich);
    m.install(UP_ENERGY, 8);
    assert_eq!((m.ticks(), m.per_tick(), m.cap()), (200, 5, 200_000));
    // M10-4: e = 4, полный буфер — снять улучшения энергии: буфер 20 000, улучшения — в слот.
    let mut m = MekBe::new(MekKind::Enrich);
    m.install(UP_ENERGY, 4);
    m.energy = m.cap();
    m.press(3); // кнопка «снять улучшения»
    assert_eq!((m.upgrades, m.energy, m.cap()), ([0, 0, 0], 20_000, 20_000));
    assert_eq!(m.slots[CH_UPGRADE], ItemStack::of(I_UPGRADE_ENERGY, 4));
    // Генераторы и куб улучшений не берут.
    assert_eq!(MekBe::new(MekKind::Cube).install(UP_SPEED, 4), 0);
    assert!(!role_accepts(MekKind::Wind, Role::Upgrade, I_UPGRADE_SPEED));
    assert!(role_accepts(MekKind::Infuser, Role::Upgrade, I_UPGRADE_ENERGY));
}

#[test]
fn upgrade_slot_install_m10_2_5() {
    // M10-2: 4 улучшения скорости в слоте перед t1 — t1…t20 счётчик, t21: скорость 4, 63 тика, 500 Дж/т.
    let mut w = Air::default();
    let mut m = MekBe::new(MekKind::Enrich);
    m.slots[CH_UPGRADE] = ItemStack::of(I_UPGRADE_SPEED, 4);
    for t in 1..=21 {
        m.tick([0, 0, 0], F_SOUTH, &mut w);
        if t < 21 {
            assert_eq!((m.install_cd as i32, m.upgrades[UP_SPEED]), (t, 0), "t{t}");
        }
    }
    assert_eq!((m.upgrades[UP_SPEED], m.ticks(), m.per_tick()), (4, 63, 500));
    assert!(m.slots[CH_UPGRADE].is_empty());
    // 12 штук → 8 внутрь, 4 в слоте, отсчёт больше не идёт.
    let mut m = MekBe::new(MekKind::Enrich);
    m.slots[CH_UPGRADE] = ItemStack::of(I_UPGRADE_SPEED, 12);
    for _ in 0..30 {
        m.tick([0, 0, 0], F_SOUTH, &mut w);
    }
    assert_eq!((m.upgrades[UP_SPEED], m.slots[CH_UPGRADE].n(), m.install_cd), (8, 4, 0));
    // M10-5: прогресс 150 при s = 0, поставить 4 скорости → в ближайшем рабочем тике операция заканчивается.
    let mut m = MekBe::new(MekKind::Enrich);
    m.slots[CH_IN] = ItemStack::of(I_RAW_IRON, 9);
    for _ in 0..150 {
        m.energy = 20_000;
        m.tick([0, 0, 0], F_SOUTH, &mut w);
    }
    assert_eq!(m.progress, 150);
    m.install(UP_SPEED, 4);
    m.energy = 20_000;
    m.tick([0, 0, 0], F_SOUTH, &mut w);
    assert_eq!((m.progress, m.slots[CH_OUT]), (0, ItemStack::of(I_DUST_IRON, 4)));
    // «4 улучшения скорости — операция ~3 с»: 63 тика.
    let mut m = MekBe::new(MekKind::Enrich);
    m.install(UP_SPEED, 4);
    m.slots[CH_IN] = ItemStack::of(I_RAW_IRON, 3);
    let mut done = 0;
    for t in 1..=100 {
        m.energy = m.cap();
        m.tick([0, 0, 0], F_SOUTH, &mut w);
        if done == 0 && !m.slots[CH_OUT].is_empty() {
            done = t;
        }
    }
    assert_eq!(done, 63);
}
