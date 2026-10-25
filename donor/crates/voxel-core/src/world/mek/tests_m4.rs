//! Эталоны Mekanism M4 «Удвоение» (G12): дробилка (MEK-06 M06-4), очиститель (MEK-11 M11-6/7/9), сепаратор (M11-1…5,
//! MEK-10 M10-7), улучшения газа (M10-8), замыкание таблиц ×2 (ORE-3) и линия ×2 (ORE-1, ORE-2; MEK-04-06-10-11 §11).
//! cargo test -p voxel-core m4

use super::*;
use crate::cell::{cell_of, F_EAST, F_SOUTH, F_WEST};
use crate::game::sim::Sim;
use crate::game::view::Events;
use crate::registry::ids::*;
use crate::registry::items::*;
use crate::world::blockentity::BlockEntity;
use crate::world::ticks::Zone;
use crate::world::World;

/// Мир-заглушка: воздух; сток газа на заданной грани берёт всё (журнал: грань, вещество, мБ).
#[derive(Default)]
struct Air {
    gas_faces: Vec<u8>,
    gas: Vec<(u8, u8, u32)>,
}

impl MekWorld for Air {
    fn cell(&mut self, _p: Pos) -> Cell {
        AIR as Cell
    }
    fn push_energy(&mut self, _p: Pos, _f: u8, _n: u64) -> u64 {
        0
    }
    fn push_gas(&mut self, _p: Pos, f: u8, sub: u8, n: u32) -> u32 {
        if self.gas_faces.contains(&f) {
            self.gas.push((f, sub, n));
            n
        } else {
            0
        }
    }
}

const P: Pos = [0, 0, 0];

fn tick(m: &mut MekBe, w: &mut Air) {
    m.tick(P, F_SOUTH, w);
}

#[test]
fn crusher_recipes_m06_4() {
    // M06-4: камень → булыжник, булыжник → гравий, гравий → песок, песчаник → 2 песка, слиток железа → пыль — на t200.
    for (input, out, n) in [
        (STONE as ItemId, COBBLE as ItemId, 1),
        (COBBLE as ItemId, GRAVEL as ItemId, 1),
        (GRAVEL as ItemId, SAND as ItemId, 1),
        (SANDSTONE as ItemId, SAND as ItemId, 2),
        (I_IRON_INGOT, I_DUST_IRON, 1),
        (I_STEEL_INGOT, I_DUST_STEEL, 1),
        (I_CLUMP_IRON, I_DIRTY_DUST_IRON, 1),
    ] {
        let mut m = MekBe::new(MekKind::Crusher);
        m.energy = m.cap();
        m.slots[CH_IN] = ItemStack::of(input, 1);
        let mut w = Air::default();
        for t in 1..=200 {
            tick(&mut m, &mut w);
            if t < 200 {
                assert!(m.slots[CH_OUT].is_empty(), "{input}: t{t}");
            }
        }
        assert_eq!(m.slots[CH_OUT], ItemStack::of(out, n), "{input}");
        assert_eq!(m.energy, 20_000 - 10_000 + 0, "10 кДж на операцию");
    }
    assert!(role_accepts(MekKind::Crusher, Role::In, I_CLUMP_IRON + 3));
    assert!(!role_accepts(MekKind::Crusher, Role::In, I_RAW_IRON));
    assert_eq!(mek_def(MekKind::Crusher).title, "Дробилка");
}

#[test]
fn purifier_even_m11_6_7_9() {
    // M11-6: ровно 1 мБ/т: бак 210, 1 сырое железо, E 80 000 → t200: 2 комка, бак 10, E 40 000.
    let mut m = MekBe::new(MekKind::Purifier);
    m.energy = 80_000;
    m.tank = Tank { sub: SUB_OXYGEN, amount: 210 };
    m.slots[PUR_IN] = ItemStack::of(I_RAW_IRON, 1);
    let mut w = Air::default();
    for _ in 0..200 {
        tick(&mut m, &mut w);
    }
    assert_eq!((m.slots[PUR_OUT], m.tank.amount, m.energy), (ItemStack::of(I_CLUMP_IRON, 2), 10, 40_000));
    // M11-7: бак 150, без подпитки — t150: P 150, бак 0; дальше пауза, P держится; 5 кремней перед t301 → по одному
    // t301…t305 → выход на t350, бак 0.
    let mut m = MekBe::new(MekKind::Purifier);
    m.energy = 80_000;
    m.tank = Tank { sub: SUB_OXYGEN, amount: 150 };
    m.slots[PUR_IN] = ItemStack::of(I_RAW_IRON, 1);
    for t in 1..=300 {
        m.energy = 80_000;
        tick(&mut m, &mut w);
        if t == 150 {
            assert_eq!((m.progress, m.tank.amount), (150, 0));
        }
    }
    assert_eq!((m.progress, m.last), (150, Step::NoGas as u8), "пауза, прогресс держится");
    assert_eq!(m.tip(2), "Нет кислорода — пауза");
    m.slots[PUR_EXTRA] = ItemStack::of(I_FLINT, 5);
    for t in 301..=350 {
        m.energy = 80_000;
        tick(&mut m, &mut w);
        if t < 350 {
            assert!(m.slots[PUR_OUT].is_empty(), "t{t}");
        }
        if t == 305 {
            assert!(m.slots[PUR_EXTRA].is_empty());
        }
    }
    assert_eq!((m.slots[PUR_OUT], m.tank.amount), (ItemStack::of(I_CLUMP_IRON, 2), 0));
    // M11-9: блок сырого железа — 18 комков на t200, 400 мБ кислорода (2 мБ/т).
    let mut m = MekBe::new(MekKind::Purifier);
    m.slots[PUR_IN] = ItemStack::of(RAW_IRON_BLOCK as ItemId, 1);
    let mut used = 0;
    for _ in 0..200 {
        m.energy = 80_000;
        m.tank = Tank { sub: SUB_OXYGEN, amount: 210 };
        tick(&mut m, &mut w);
        used += 210 - m.tank.amount;
    }
    assert_eq!((m.slots[PUR_OUT], used), (ItemStack::of(I_CLUMP_IRON, 18), 400));
    // Бак принимает только кислород; кремень — только если 10 мБ влезут целиком.
    let mut m = MekBe::new(MekKind::Purifier);
    assert_eq!(m.insert_gas(SUB_HYDROGEN, 10, false), 0);
    m.tank = Tank { sub: SUB_OXYGEN, amount: 205 };
    m.slots[PUR_EXTRA] = ItemStack::of(I_FLINT, 1);
    tick(&mut m, &mut w);
    assert_eq!((m.tank.amount, m.slots[PUR_EXTRA].n()), (205, 1), "кремень ждёт");
    assert!(role_accepts(MekKind::Purifier, Role::Extra, I_FLINT) && !role_accepts(MekKind::Purifier, Role::Extra, I_COAL));
    assert!(!role_accepts(MekKind::Infuser, Role::Extra, I_FLINT));
}

#[test]
fn purifier_upgrades_m10_8() {
    // M10-8: s = 8, g = 0, ровно: 20 тиков, 20 000 Дж/т, 400 кДж и 2 000 мБ кислорода на операцию (100 мБ/т).
    let mut m = MekBe::new(MekKind::Purifier);
    assert_eq!(m.install(UP_SPEED, 8), 8);
    assert_eq!((m.ticks(), m.per_tick(), m.cap()), (20, 20_000, 80_000));
    m.slots[PUR_IN] = ItemStack::of(I_RAW_IRON, 1);
    let mut used = 0;
    let mut w = Air::default();
    for _ in 0..20 {
        m.energy = m.cap();
        m.tank = Tank { sub: SUB_OXYGEN, amount: 210 };
        tick(&mut m, &mut w);
        used += 210 - m.tank.amount;
    }
    assert_eq!((m.slots[PUR_OUT].n(), used), (2, 2_000));
    // Улучшения газа возвращают расход: s = 8, g = 8 → 200 мБ на операцию; s = 3 → 474 (5 мБ/т, в конце 6).
    assert_eq!(m.install(UP_GAS, 8), 8);
    assert_eq!(GAS_OP[(8 - 8 + 8) as usize], 200);
    let mut m = MekBe::new(MekKind::Purifier);
    m.install(UP_SPEED, 3);
    m.slots[PUR_IN] = ItemStack::of(I_RAW_IRON, 1);
    let mut per = Vec::new();
    for _ in 0..84 {
        m.energy = m.cap();
        m.tank = Tank { sub: SUB_OXYGEN, amount: 210 };
        tick(&mut m, &mut w);
        per.push(210 - m.tank.amount);
    }
    assert_eq!((per.iter().sum::<u32>(), per[0], per[83]), (474, 5, 6));
    // Буфер и Дж/т очистителя по таблицам базы 200 / 80 000 (MEK-10 §5.2).
    let mut m = MekBe::new(MekKind::Purifier);
    m.install(UP_SPEED, 1);
    assert_eq!(m.per_tick(), 356);
    m.install(UP_ENERGY, 1);
    assert_eq!((m.per_tick(), m.cap()), (267, 106_681));
    // Газ — только очиститель.
    assert_eq!(MekBe::new(MekKind::Enrich).install(UP_GAS, 1), 0);
    assert!(role_accepts(MekKind::Purifier, Role::Upgrade, I_UPGRADE_GAS));
    assert!(!role_accepts(MekKind::Crusher, Role::Upgrade, I_UPGRADE_GAS));
}

fn separator(water: u32, energy: u64) -> MekBe {
    let mut m = MekBe::new(MekKind::Separator);
    m.tank = Tank { sub: recipes::SUB_WATER, amount: water };
    m.energy = energy;
    m
}

#[test]
fn separator_m11_1_2_3() {
    // M11-1: E 160 000, вода 2 000, без притока, оба «простой» → t400: вода 1 200, H₂ 800, O₂ 400, E 0; t401: 0 операций.
    let mut m = separator(2_000, 160_000);
    let mut w = Air::default();
    for _ in 0..400 {
        tick(&mut m, &mut w);
    }
    assert_eq!((m.tank.amount, m.gas[0].amount, m.gas[1].amount, m.energy), (1_200, 800, 400, 0));
    tick(&mut m, &mut w);
    assert_eq!((m.last, m.gas[0].amount), (Step::NoEnergy as u8, 800));
    // M11-2: с притоком ≥ 400 Дж/т — t1000: вода 0, H₂ 2 000, O₂ 1 000; потрачено 400 000 Дж.
    let mut m = separator(2_000, 0);
    let mut spent = 0;
    for _ in 0..1000 {
        m.energy += 400;
        let e = m.energy;
        tick(&mut m, &mut w);
        spent += e - m.energy;
    }
    assert_eq!((m.tank.amount, m.gas[0].amount, m.gas[1].amount, spent), (0, 2_000, 1_000, 400_000));
    tick(&mut m, &mut w);
    assert_eq!(m.last, Step::NoWater as u8);
    // M11-3: вода ≥ 2 400, кислород забирают каждый тик (сток справа), водород «простой» → t1200: H₂ 2 400; t1201 стоит;
    // окно подсказывает, что водороду некуда деться.
    let mut m = separator(24_000, 0);
    let mut w = Air { gas_faces: vec![dir_of(F_SOUTH, sides::R_RIGHT)], ..Default::default() };
    for _ in 0..1200 {
        m.energy = 160_000;
        tick(&mut m, &mut w);
    }
    assert_eq!((m.gas[0].amount, m.gas[1].amount), (2_400, 1));
    assert!(w.gas.iter().all(|&(f, s, _)| f == F_EAST && s == SUB_OXYGEN), "кислород — вправо (лицо на юг → восток)");
    m.energy = 160_000;
    tick(&mut m, &mut w);
    assert_eq!((m.last, m.active.logic), (Step::H2Full as u8, false), "встал: водород некуда деть");
    let tank_h2 = 1; // второй бак окна
    let i = m.def().widgets.iter().enumerate().filter(|(_, w)| w.kind == crate::abi::Widget::Tank).nth(tank_h2).unwrap().0;
    assert!(m.tip(i).contains("Бак водорода полон"), "{}", m.tip(i));
    assert_eq!(m.widget_values()[i * 2 + 1] & 1, 1, "предупреждение на шкале водорода");
    let status = m.def().widgets.len() - 2;
    assert_eq!(m.tip(status), "Стоит: водород некуда деть!");
    // Кнопка режима водорода → «сброс излишка» → «сброс»: водород уходит, сепаратор снова работает.
    m.press(4);
    assert_eq!(m.gas_mode[0], GM_EXCESS);
    m.press(4);
    assert_eq!((m.gas_mode[0], m.tip(5)), (GM_DUMP, "H2: сброс".to_string()));
    // Сброс — после шага рецепта (MEK-11 §4.2.2): в этом тике ещё стоит, бак −8; со следующего работает.
    m.energy = 160_000;
    tick(&mut m, &mut w);
    assert_eq!((m.last, m.gas[0].amount), (Step::H2Full as u8, 2_392));
    tick(&mut m, &mut w);
    assert_eq!((m.last, m.gas[0].amount), (Step::Worked as u8, 2_392 + 2 - 8));
}

#[test]
fn separator_modes_bucket_speed_m11_4_5_m10_7() {
    let mut w = Air { gas_faces: vec![F_EAST], ..Default::default() };
    // M11-4: H₂ «сброс» — после каждого тика 0; «сброс излишка» — растёт до 2 040 (t1020) и держится.
    let mut m = separator(24_000, 0);
    m.gas_mode[0] = GM_DUMP;
    for _ in 0..50 {
        m.energy = 160_000;
        tick(&mut m, &mut w);
        assert_eq!(m.gas[0].amount, 0);
    }
    let mut m = separator(24_000, 0);
    m.gas_mode[0] = GM_EXCESS;
    for t in 1..=1100 {
        m.energy = 160_000;
        tick(&mut m, &mut w);
        if t == 1020 || t == 1100 {
            assert_eq!(m.gas[0].amount, 2_040, "t{t}");
        }
    }
    // Оба «сброс излишка» и оба ≥ 2 040 → 0 операций.
    let mut m = separator(24_000, 160_000);
    m.gas_mode = [GM_EXCESS; 2];
    m.gas = [Tank { sub: SUB_HYDROGEN, amount: 2_040 }, Tank { sub: SUB_OXYGEN, amount: 2_040 }];
    tick(&mut m, &mut Air::default());
    assert_eq!((m.energy, m.gas[0].amount), (160_000, 2_040));
    // Бак в «сбросе» не выталкивается.
    let mut m = separator(0, 0);
    m.gas[1] = Tank { sub: SUB_OXYGEN, amount: 100 };
    m.gas_mode[1] = GM_DUMP;
    let mut sink = Air { gas_faces: vec![F_EAST], ..Default::default() };
    tick(&mut m, &mut sink);
    assert!(sink.gas.is_empty());
    assert_eq!(m.gas[1].amount, 92);
    // M11-5: бак 23 500, ведро — остаётся полным; бак 23 000 → +1 000, в слоте пустое ведро.
    let mut m = separator(23_500, 0);
    m.slots[SEP_FLUID] = ItemStack::of(I_WATER_BUCKET, 1);
    tick(&mut m, &mut Air::default());
    assert_eq!((m.tank.amount, m.slots[SEP_FLUID].item), (23_500, I_WATER_BUCKET));
    m.tank.amount = 23_000;
    tick(&mut m, &mut Air::default());
    assert_eq!((m.tank.amount, m.slots[SEP_FLUID]), (24_000, ItemStack::of(I_BUCKET, 1)));
    assert!(role_accepts(MekKind::Separator, Role::Fluid, I_WATER_BUCKET) && !role_accepts(MekKind::Separator, Role::Fluid, I_BUCKET));
    // M10-7: s = 3, вода 24 000, E 160 000 → 8 операций за тик: вода −16, H₂ +16, O₂ +8, E −3 200; при E 1 000 — 2.
    let mut m = separator(24_000, 160_000);
    m.install(UP_SPEED, 3);
    tick(&mut m, &mut Air::default());
    assert_eq!((m.tank.amount, m.gas[0].amount, m.gas[1].amount, m.energy), (24_000 - 16, 16, 8, 160_000 - 3_200));
    assert!(m.tip(3).contains("Использует: 3,2 кДж/т"), "{}", m.tip(3));
    m.energy = 1_000;
    tick(&mut m, &mut Air::default());
    assert_eq!(m.energy, 200);
    // Буфер сепаратора с улучшениями энергии — таблица базы 160 000; скорость энергию на операцию не меняет.
    let mut m = separator(0, 0);
    m.install(UP_ENERGY, 1);
    assert_eq!(m.cap(), 213_363);
}

#[test]
fn separator_feeds_purifier_through_right_face() {
    // Сепаратор лицом на юг, справа (восток) — очиститель: кислород сам идёт в бак очистителя (до 210), водород копится.
    let world = World::new(1);
    let mut s = Sim::new(&world, 1);
    let mut zone = Zone::default();
    zone.add_square(0, 0, 4);
    let mut ev = Events::default();
    let (sep, pur) = ([2, 100, 2], [3, 100, 2]);
    {
        let d = world.d.clone();
        let mut w = d.borrow_mut();
        s.set_world(&mut w, sep, cell_of(ELECTROLYTIC_SEPARATOR, F_SOUTH as u16), &mut ev);
        s.set_world(&mut w, pur, cell_of(PURIFICATION_CHAMBER, F_SOUTH as u16), &mut ev);
        let m = w.be_mut().mek_mut(sep).unwrap();
        m.tank = Tank { sub: recipes::SUB_WATER, amount: 10_000 };
        m.energy = 160_000;
    }
    for _ in 0..300 {
        s.tick_world(zone.clone(), &mut ev);
    }
    let w = world.d.borrow();
    let (a, b) = (w.be().mek(sep).unwrap(), w.be().mek(pur).unwrap());
    assert_eq!((b.tank.sub, b.tank.amount), (SUB_OXYGEN, PURIFY_TANK), "бак очистителя полон кислородом");
    assert_eq!((a.gas[0].amount, a.gas[1].amount), (600, 300 - 210), "водород копится, кислород — остаток");
    // Очиститель слева от сепаратора (запад) кислород не получает.
    drop(w);
    let world2 = World::new(1);
    let mut s2 = Sim::new(&world2, 1);
    {
        let d = world2.d.clone();
        let mut w = d.borrow_mut();
        s2.set_world(&mut w, sep, cell_of(ELECTROLYTIC_SEPARATOR, F_SOUTH as u16), &mut ev);
        s2.set_world(&mut w, [1, 100, 2], cell_of(PURIFICATION_CHAMBER, F_WEST as u16), &mut ev);
        let m = w.be_mut().mek_mut(sep).unwrap();
        m.tank = Tank { sub: recipes::SUB_WATER, amount: 10_000 };
        m.energy = 160_000;
    }
    for _ in 0..20 {
        s2.tick_world(zone.clone(), &mut ev);
    }
    assert_eq!(world2.d.borrow().be().mek([1, 100, 2]).unwrap().tank.amount, 0, "водород очиститель не берёт");
}

#[test]
fn doubling_tables_close_ore_3() {
    // ORE-3: для каждого металла сырьё → 2 комка → 2 грязные пыли → 2 пыли → 2 слитка находится поиском по таблицам.
    for (k, &(raw, dust, ingot)) in recipes::METALS.iter().enumerate() {
        let p = recipes::PURIFY[recipes::find_purify(raw).unwrap() as usize];
        assert_eq!((p.out, p.out_n, p.rate), (I_CLUMP_IRON + k as ItemId, 2, 1));
        let c = recipes::CRUSH[recipes::find_crush(p.out).unwrap() as usize];
        assert_eq!((c.out, c.out_n), (I_DIRTY_DUST_IRON + k as ItemId, 1));
        let e = recipes::ENRICH[recipes::find_enrich(c.out).unwrap() as usize];
        assert_eq!((e.out, e.out_n), (dust, 1));
        assert_eq!(crate::world::blockentity::smelt_result(dust), Some(ingot), "плавка пыли {k}");
    }
    // Нет двух рецептов одного входа в таблице.
    for t in [&recipes::ENRICH[..], &recipes::CRUSH[..]] {
        for (i, a) in t.iter().enumerate() {
            assert!(t[i + 1..].iter().all(|b| b.input != a.input), "{}", a.input);
        }
    }
    assert!(recipes::find_purify(I_DUST_IRON).is_none());
    // Продвинутая схема — инфузией из базовой (60 редстоуна).
    assert!(recipes::INFUSE.iter().any(|r| r.input == I_BASIC_CIRCUIT && r.amount == 60 && r.out == I_ADVANCED_CIRCUIT));
}

/// Линия §4.3.4 площадки «удвоение» через мир (тот же build, что у сцены): ответ — тики, когда в сундуке появлялся
/// каждый следующий слиток.
fn line(raw: i32, prefill: bool, ticks: u32) -> Vec<u32> {
    use crate::game::inventory::Inventory;
    use crate::game::mek_demo::{build, double_layout};
    let world = World::new(1);
    let mut s = Sim::new(&world, 1);
    let mut zone = Zone::default();
    zone.add_square(0, 0, 4);
    let mut ev = Events::default();
    let p = [8, 100, 8];
    let (sep, pur, cru, ch, sm, chest, _) = double_layout(p);
    {
        let d = world.d.clone();
        let mut w = d.borrow_mut();
        build(&mut s, &mut w, &mut ev, &mut Inventory::new(), p, 6);
        w.be_mut().mek_mut(pur).unwrap().slots[PUR_IN] = ItemStack::of(I_RAW_IRON, raw);
        if prefill {
            // «Энергии и кислорода достаточно» (§4.3.4): буферы полны, бак очистителя полон.
            for q in [sep, pur, cru, ch, sm] {
                let m = w.be_mut().mek_mut(q).unwrap();
                m.energy = m.cap();
            }
            w.be_mut().mek_mut(pur).unwrap().tank = Tank { sub: SUB_OXYGEN, amount: PURIFY_TANK };
        }
    }
    let mut got = Vec::new();
    let mut seen = 0;
    for t in 1..=ticks {
        s.tick_world(zone.clone(), &mut ev);
        let n = match world.d.borrow_mut().be_mut().get_mut(chest) {
            Some(BlockEntity::Chest(c)) => c.slots.iter().filter(|x| x.item == I_IRON_INGOT).map(|x| x.count).sum::<i32>(),
            _ => panic!("нет сундука"),
        };
        while seen < n {
            got.push(t);
            seen += 1;
        }
    }
    got
}

#[test]
fn line_one_raw_ore_1() {
    // ORE-1: 1 сырое железо → первый слиток в сундуке t837, второй t1035 (энергии и кислорода достаточно).
    let got = line(1, true, 1100);
    assert_eq!(got, vec![837, 1035]);
}

#[test]
fn line_ten_raw_ore_2() {
    // ORE-2 («Готово, когда» G12): 10 сырого железа → 20 слитков к t4632, без рук. И с пустыми буферами (как в сцене:
    // энергия — от куба по кабелю, кислород — от сепаратора) — тоже 20, чуть позже.
    let got = line(10, true, 4700);
    assert_eq!((got.len(), got.last().copied()), (20, Some(4632)));
    let cold = line(10, false, 5000);
    assert_eq!(cold.len(), 20, "{cold:?}");
    assert!(cold[19] < 4700, "{}", cold[19]);
}

#[test]
fn m4_crafting_crusher_returns_buckets() {
    // Крафт M4 (стандартная книга): дробилка — 2 ведра лавы, вёдра остаются в сетке пустыми; сепаратор, очиститель,
    // ядро, продвинутая схема и улучшение газа находятся.
    use crate::game::crafting::{Grid, RecipeBook};
    let b = RecipeBook::standard();
    let grid = |rows: [[ItemId; 3]; 3]| {
        let mut g = Grid::new(3, 3);
        for (y, r) in rows.iter().enumerate() {
            for (x, &i) in r.iter().enumerate() {
                if i != 0 {
                    g.set(x, y, ItemStack::of(i, 1));
                }
            }
        }
        g
    };
    let (r, c, l, s) = (I_REDSTONE, I_BASIC_CIRCUIT, I_LAVA_BUCKET, STEEL_CASING as ItemId);
    let mut g = grid([[r, c, r], [l, s, l], [r, c, r]]);
    assert_eq!(b.result(&g), ItemStack::of(CRUSHER as ItemId, 1));
    b.consume(&mut g, &mut |_| {});
    assert_eq!((*g.at(0, 1), *g.at(2, 1)), (ItemStack::of(I_BUCKET, 1), ItemStack::of(I_BUCKET, 1)), "вёдра вернулись");
    let (a, o, i, gd) = (I_ALLOY_INFUSED, I_DUST_OSMIUM, I_DUST_IRON, I_DUST_GOLD);
    assert_eq!(b.result(&grid([[a, o, a], [i, a, gd], [a, o, a]])), ItemStack::of(I_ELECTROLYTIC_CORE, 1));
    let (ir, core) = (I_IRON_INGOT, I_ELECTROLYTIC_CORE);
    assert_eq!(b.result(&grid([[ir, r, ir], [a, core, a], [ir, r, ir]])), ItemStack::of(ELECTROLYTIC_SEPARATOR as ItemId, 1));
    assert_eq!(b.result(&grid([[a, c, a], [0, 0, 0], [0, 0, 0]])), ItemStack::of(I_ADVANCED_CIRCUIT, 1));
    let (adv, os, en) = (I_ADVANCED_CIRCUIT, I_OSMIUM_INGOT, ENRICHMENT_CHAMBER as ItemId);
    assert_eq!(b.result(&grid([[a, adv, a], [os, en, os], [a, adv, a]])), ItemStack::of(PURIFICATION_CHAMBER as ItemId, 1));
    let gl = GLASS as ItemId;
    assert_eq!(b.result(&grid([[0, gl, 0], [a, i, a], [0, gl, 0]])), ItemStack::of(I_UPGRADE_GAS, 1));
}
