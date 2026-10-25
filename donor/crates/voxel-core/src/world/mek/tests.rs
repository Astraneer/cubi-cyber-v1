//! Эталоны Mekanism (MEK-01 §18, MEK-02 §13, MEK-03 §12, MEK-04 §11): cargo test -p voxel-core mek.

use super::*;
use crate::cell::{cell_of, F_EAST, F_NORTH, F_SOUTH, F_WEST};
use crate::registry::ids::*;
use crate::registry::items::*;
use crate::world::grid::{arm_mask, arms_from_mask, arms_of, split_fair, with_arms};

/// Мир-заглушка для генератора: лава вокруг, сток за лицом «берёт всё» (или ничего).
struct Stub {
    lava: Vec<Pos>,
    sink: bool,
    taken: u64,
}

impl MekWorld for Stub {
    fn cell(&mut self, p: Pos) -> Cell {
        if self.lava.contains(&p) {
            LAVA as Cell
        } else {
            AIR as Cell
        }
    }
    fn push_energy(&mut self, _p: Pos, _f: u8, n: u64) -> u64 {
        if self.sink {
            self.taken += n;
            n
        } else {
            0
        }
    }
}

const P: Pos = [0, 64, 0];

fn stub(faces: usize) -> Stub {
    let lava = [F_WEST, F_EAST, F_NORTH, crate::cell::F_DOWN, crate::cell::F_UP, F_SOUTH][..faces].iter().map(|&f| offset(P, f)).collect();
    Stub { lava, sink: true, taken: 0 }
}

fn heat() -> MekBe {
    MekBe::new(MekKind::Heat)
}

#[test]
fn split_fair_table() {
    // N-1: все строки таблицы MEK-02 §4.2 (∞ — больше суммы).
    const INF: u64 = u64::MAX / 4;
    let rows: &[(u64, &[u64], &[u64])] = &[
        (100, &[INF, INF], &[50, 50]),
        (101, &[INF, INF], &[51, 50]),
        (100, &[10, INF, INF], &[10, 45, 45]),
        (100, &[INF, INF, 10], &[45, 45, 10]),
        (10, &[INF; 4], &[3, 3, 2, 2]),
        (2, &[INF; 3], &[1, 1, 0]),
        (5, &[INF; 7], &[1, 1, 1, 1, 1, 0, 0]),
        (100, &[30, 30, 30], &[30, 30, 30]),
        (100, &[60, 20, 60], &[40, 20, 40]),
        (7, &[INF, 1, INF], &[3, 1, 3]),
        (11, &[3, INF, INF], &[3, 4, 4]),
        (53, &[INF, INF], &[27, 26]),
        (53, &[50, 20_000], &[27, 26]),
        (40_000, &[8000; 9], &[4445, 4445, 4445, 4445, 4444, 4444, 4444, 4444, 4444]),
        (100_000, &[8000, 128_000], &[8000, 92_000]),
    ];
    for (total, req, want) in rows {
        assert_eq!(split_fair(*total, req), want.to_vec(), "сумма {total}, просьбы {req:?}");
    }
}

#[test]
fn arms_packing() {
    // N-23: arms_of(with_arms(s, a)) == a для всех 4 096; бит 7 и биты 13–15 не тронуты.
    for a in 0..4096u16 {
        for s in [0u16, 0x80, 0xE000, 0xFFFF] {
            let t = with_arms(s, a);
            assert_eq!(arms_of(t), a);
            assert_eq!(t & 0xE080, s & 0xE080);
        }
    }
    assert_eq!(arm_mask(with_arms(0, arms_from_mask(0b101101))), 0b101101);
}

#[test]
fn active_view_reference() {
    // M01-9: вкл. на нечётных, выкл. на чётных с 1 по 100, затем выкл. → [(1,вкл),(2,выкл),(3,вкл),(160,выкл)].
    let mut a = ActiveView::default();
    let mut ch = Vec::new();
    for t in 1..=300 {
        if let Some(v) = a.countdown() {
            ch.push((t, v));
        }
        let on = t <= 100 && t % 2 == 1;
        if let Some(v) = a.set(on) {
            ch.push((t, v));
        }
    }
    assert_eq!(ch, vec![(1, true), (2, false), (3, true), (160, false)]);
    // M01-10: 1 тик вкл., 59 выкл., по кругу 2 000 тиков — 68 смен.
    let mut a = ActiveView::default();
    let mut n = 0;
    for t in 0..2000 {
        n += a.countdown().is_some() as u32;
        n += a.set(t % 60 == 0).is_some() as u32;
    }
    assert_eq!(n, 68);
}

/// Выработка генератора по тикам (produced) за n тиков.
fn run_heat(g: &mut MekBe, w: &mut Stub, n: usize) -> Vec<u64> {
    (0..n)
        .map(|_| {
            g.tick(P, F_SOUTH, w);
            g.produced
        })
        .collect()
}

#[test]
fn heat_generator_lava_m03_1_3() {
    // M03-1: лава с N граней, 2 000 тиков → 4, 16, 33, 52, 73, 96 Дж/т; T при N = 4 — 537,18 К.
    for (n, want) in [(1, 4), (2, 16), (3, 33), (4, 52), (5, 73), (6, 96)] {
        let mut g = heat();
        let mut w = stub(n);
        let p = run_heat(&mut g, &mut w, 2000);
        assert_eq!(*p.last().unwrap(), want, "граней {n}");
        if n == 4 {
            assert!((g.temp() - 537.18).abs() < 0.01, "T = {}", g.temp());
        }
    }
    // M03-3: 4 грани с холода: t1…t12 и T после t1–t3; t20 — 24, t60 — 48, t100 — 51, t200 — 52.
    let mut g = heat();
    let mut w = stub(4);
    let mut temps = Vec::new();
    let mut prod = Vec::new();
    for t in 1..=200 {
        g.tick(P, F_SOUTH, &mut w);
        prod.push(g.produced);
        if t <= 3 {
            temps.push(g.temp());
        }
    }
    assert_eq!(&prod[..12], &[0, 0, 0, 1, 2, 4, 5, 7, 8, 10, 11, 13]);
    assert_eq!((prod[19], prod[59], prod[99], prod[199]), (24, 48, 51, 52));
    let r4 = |x: f64| (x * 10_000.0).round() / 10_000.0;
    assert_eq!(temps.iter().map(|&t| r4(t)).collect::<Vec<_>>(), vec![312.0, 323.3929, 334.2093]);
    // Генератор «не активен» в лаве без топлива (решение после MEK-03/12).
    assert!(!g.active.shown);
}

#[test]
fn heat_generator_fuel_m03_2_4_5() {
    // M03-2: бак полон, N граней → 112, 136, 162, 188, 214, 241, 268.
    for (n, want) in [(0, 112), (1, 136), (2, 162), (3, 188), (4, 214), (5, 241), (6, 268)] {
        let mut g = heat();
        g.tank = Tank { sub: recipes::SUB_LAVA, amount: HEAT_TANK_MB };
        let mut w = stub(n);
        for _ in 0..2000 {
            g.tank.amount = HEAT_TANK_MB; // «бак полон» всё время
            g.tick(P, F_SOUTH, &mut w);
        }
        assert_eq!(g.produced, want, "граней {n} + топливо");
    }
    // M03-4: 1 уголь, без лавы, с холода: бак после t1 — 70; «активна» t1…t8; выработка t1…t20; всего 262 Дж.
    let mut g = heat();
    g.slots[0] = ItemStack::of(I_COAL, 1);
    let mut w = stub(0);
    let mut prod = Vec::new();
    let mut active = Vec::new();
    for t in 1..=400 {
        g.tick(P, F_SOUTH, &mut w);
        if t == 1 {
            assert_eq!(g.tank.amount, 70);
        }
        prod.push(g.produced);
        active.push(g.active.logic);
    }
    assert_eq!(&prod[..20], &[0, 0, 2, 4, 7, 10, 13, 17, 20, 19, 17, 15, 14, 13, 12, 11, 10, 9, 8, 7]);
    assert_eq!(active.iter().position(|&a| !a), Some(8), "горит t1…t8");
    assert_eq!(prod.iter().sum::<u64>(), 262);
    // M03-5: 64 угля: слот пуст после t64, бак макс. 4 480 (t64), горит t1…t512, всего 55 919 Дж.
    let mut g = heat();
    g.slots[0] = ItemStack::of(I_COAL, 64);
    let mut w = stub(0);
    let (mut sum, mut max_tank, mut empty_at, mut last_burn) = (0u64, 0u32, 0, 0);
    for t in 1..=3000 {
        g.tick(P, F_SOUTH, &mut w);
        sum += g.produced;
        max_tank = max_tank.max(g.tank.amount);
        if empty_at == 0 && g.slots[0].is_empty() {
            empty_at = t;
        }
        if g.active.logic {
            last_burn = t;
        }
        match t {
            20 => assert_eq!(g.produced, 56),
            100 => assert_eq!(g.produced, 111),
            500 => assert_eq!(g.produced, 112),
            520 => assert_eq!(g.produced, 65),
            560 => assert_eq!(g.produced, 1),
            _ => {}
        }
    }
    assert_eq!((empty_at, max_tank, last_burn, sum), (64, 4480, 512, 55_919));
}

#[test]
fn heat_generator_edges_m03_7_10() {
    // M03-7: ведро лавы — «активна» t1…t100, в слоте пустое ведро; бак 23 500 — ведро не берётся.
    let mut g = heat();
    g.slots[0] = ItemStack::of(I_LAVA_BUCKET, 1);
    let mut w = stub(0);
    let mut sum = 0;
    let mut burn = 0;
    for _ in 0..2000 {
        g.tick(P, F_SOUTH, &mut w);
        sum += g.produced;
        burn += g.active.logic as u32;
    }
    assert_eq!((burn, g.slots[0], sum), (100, ItemStack::of(I_BUCKET, 1), 9781));
    let mut g = heat();
    g.slots[0] = ItemStack::of(I_LAVA_BUCKET, 1);
    g.tank = Tank { sub: recipes::SUB_LAVA, amount: 23_500 };
    g.tick(P, F_SOUTH, &mut stub(0));
    assert_eq!(g.slots[0].item, I_LAVA_BUCKET);
    // M03-8: палка — не горит, бак 5; две — горит только t2; доска — горит t1, остаётся 5.
    let mut g = heat();
    g.slots[0] = ItemStack::of(I_STICK, 2);
    let mut w = stub(0);
    g.tick(P, F_SOUTH, &mut w);
    assert_eq!((g.tank.amount, g.active.logic), (5, false));
    g.tick(P, F_SOUTH, &mut w);
    assert_eq!((g.tank.amount, g.active.logic), (0, true));
    let mut g = heat();
    g.slots[0] = ItemStack::of(PLANKS as ItemId, 1);
    g.tick(P, F_SOUTH, &mut stub(0));
    assert_eq!((g.tank.amount, g.active.logic), (5, true));
    // M03-9: буфер полон, бак 1 000, 4 грани, стока нет — топливо не горит, «активна» ни разу.
    let mut g = heat();
    g.energy = 160_000;
    g.tank = Tank { sub: recipes::SUB_LAVA, amount: 1000 };
    let mut w = stub(4);
    w.sink = false;
    for _ in 0..100 {
        g.tick(P, F_SOUTH, &mut w);
        assert!(!g.active.logic);
    }
    assert_eq!((g.tank.amount, g.energy), (1000, 160_000));
    // M03-10: буфер полон, спереди сток без предела — за тик ушло 1 020.
    let mut g = heat();
    g.energy = 160_000;
    let mut w = stub(0);
    g.tick(P, F_SOUTH, &mut w);
    assert_eq!(w.taken, 1020);
}

#[test]
fn energy_io_faces() {
    // Тепловой отдаёт только лицом; инфузор принимает всеми гранями; куб — всеми, кроме лица, лицом отдаёт.
    assert_eq!(energy_io(MekKind::Heat, F_SOUTH, F_SOUTH), (false, true));
    assert_eq!(energy_io(MekKind::Heat, F_SOUTH, F_NORTH), (false, false));
    assert_eq!(energy_io(MekKind::Infuser, F_NORTH, F_SOUTH), (true, false));
    assert_eq!(energy_io(MekKind::Cube, F_EAST, F_EAST), (false, true));
    assert_eq!(energy_io(MekKind::Cube, F_EAST, F_WEST), (true, false));
}

#[test]
fn infuser_steel_path() {
    // MEK-04 §2.2: пустой инфузор с углём в слоте начинает работу в том же тике, когда уголь превратился.
    // 1 железо + 10 углерода → обогащённое железо за 200 тиков × 50 Дж = 10 кДж; второе — сталь.
    let mut m = MekBe::new(MekKind::Infuser);
    m.energy = 20_000;
    m.slots[INF_EXTRA] = ItemStack::of(I_COAL, 2);
    m.slots[INF_IN] = ItemStack::of(I_IRON_INGOT, 1);
    let mut w = stub(0);
    let mut shown = Vec::new();
    for t in 1..=200 {
        if let Some(v) = m.tick(P, F_NORTH, &mut w) {
            shown.push((t, v));
        }
        if t == 1 {
            assert_eq!((m.tank.sub, m.tank.amount, m.progress, m.energy), (recipes::SUB_CARBON, 10, 1, 19_950));
            assert_eq!(m.slots[INF_EXTRA].n(), 1, "уголь — по одному, пока бак не влезет следующий целиком");
        }
    }
    assert_eq!(m.slots[INF_OUT], ItemStack::of(I_ENRICHED_IRON, 1));
    assert!(m.slots[INF_IN].is_empty());
    assert_eq!((m.energy, m.progress), (10_000, 0));
    assert_eq!(shown, vec![(1, true)]);
    // Вход пуст — следующий тик выключает вид сразу (первое выключение — мгновенно).
    assert_eq!(m.tick(P, F_NORTH, &mut w), Some(false));
    // Обогащённое железо назад во вход → стальная пыль; уголь второй уже в баке (10 мБ).
    m.slots[INF_IN] = std::mem::take(&mut m.slots[INF_OUT]);
    for _ in 0..200 {
        m.tick(P, F_NORTH, &mut w);
    }
    assert_eq!(m.slots[INF_OUT], ItemStack::of(I_DUST_STEEL, 1));
    assert_eq!(m.energy, 0);
    // Без энергии — пауза: прогресс держится.
    m.slots[INF_IN] = ItemStack::of(I_IRON_INGOT, 3);
    m.slots[INF_EXTRA] = ItemStack::of(I_COAL, 1);
    m.slots[INF_OUT] = ItemStack::EMPTY;
    m.energy = 100;
    m.tick(P, F_NORTH, &mut w);
    m.tick(P, F_NORTH, &mut w);
    m.tick(P, F_NORTH, &mut w);
    assert_eq!((m.progress, m.last), (2, Step::NoEnergy as u8));
    // Редстоун в слоте энергии: 10 000 Дж, если влезает целиком.
    m.slots[INF_ENERGY] = ItemStack::of(I_REDSTONE, 3);
    m.tick(P, F_NORTH, &mut w);
    assert_eq!((m.slots[INF_ENERGY].n(), m.progress), (2, 3));
    // «Сброс» бака — операция потеряна; уголь в слоте превратится в следующем тике до шага рецепта.
    m.slots[INF_EXTRA] = ItemStack::of(I_COAL, 1);
    m.press(3);
    assert_eq!(m.tank.amount, 0);
    m.tick(P, F_NORTH, &mut w);
    assert_eq!(m.progress, 1, "уголь превратился до шага рецепта, но прогресс начат заново");
}

#[test]
fn infuser_stalls_and_reset() {
    let mut m = MekBe::new(MekKind::Infuser);
    let mut w = stub(0);
    m.energy = 20_000;
    m.tank = Tank { sub: recipes::SUB_CARBON, amount: 100 };
    m.slots[INF_IN] = ItemStack::of(I_IRON_INGOT, 5);
    m.slots[INF_OUT] = ItemStack::of(I_ENRICHED_IRON, 64);
    m.tick(P, F_NORTH, &mut w);
    assert_eq!((m.last, m.progress), (Step::NoRoom as u8, 0));
    m.slots[INF_OUT] = ItemStack::of(I_DUST_STEEL, 1);
    m.tick(P, F_NORTH, &mut w);
    assert_eq!(m.last, Step::WrongOutput as u8);
    m.slots[INF_OUT] = ItemStack::EMPTY;
    for _ in 0..50 {
        m.tick(P, F_NORTH, &mut w);
    }
    assert_eq!(m.progress, 50);
    // Вход другого рецепта нужен с редстоуном — с углеродом в баке рецепта нет: сброс.
    m.slots[INF_IN] = ItemStack::of(I_OSMIUM_INGOT, 1);
    m.tick(P, F_NORTH, &mut w);
    assert_eq!((m.progress, m.last), (0, Step::Reset as u8));
}

#[test]
fn cube_and_numbers() {
    // M12-5: целый smooth_scale: 525, 997, 1 422; t10 3 417; t20 4 608; с t38 — 5 150.
    let mut s = 0u16;
    let mut v = Vec::new();
    for _ in 0..60 {
        s = smooth_scale(s, 2_100_000, 4_000_000);
        v.push(s);
    }
    assert_eq!((v[0], v[1], v[2], v[9], v[19], v[37], v[59]), (525, 997, 1422, 3417, 4608, 5150, 5150));
    // M12-4: куб пуст, 64 пыли в «−» — +10 000 за тик, пыль кончается на t64.
    let mut c = MekBe::new(MekKind::Cube);
    c.slots[0] = ItemStack::of(I_REDSTONE, 64);
    let mut w = stub(0);
    w.sink = false;
    let mut end = 0;
    for t in 1..=100 {
        c.tick(P, F_NORTH, &mut w);
        if end == 0 && c.slots[0].is_empty() {
            end = t;
        }
    }
    assert_eq!((end, c.energy), (64, 640_000));
    // M12-1: куб отдаёт лицом не больше 4 000 за тик.
    let mut w = stub(0);
    c.tick(P, F_NORTH, &mut w);
    assert_eq!(w.taken, 4000);
    // Формат чисел (MEK-08 §11.3).
    assert_eq!([joules(8400), joules(8399), joules(20_000), joules(50), joules(0), joules(1_234_567)], ["8,4 кДж", "8,39 кДж", "20 кДж", "50 Дж", "0 Дж", "1,23 МДж"]);
    assert_eq!(thousands(24_000), "24 000");
    assert_eq!((fill(0, 20_000, 52), fill(1, 20_000, 52), fill(20_000, 20_000, 52)), (0, 1, 52));
}

// ---------------------------------------------------------------- сеть в мире (WorldData + Sim)

use crate::game::sim::Sim;
use crate::game::view::Events;
use crate::world::ticks::Zone;
use crate::world::World;

/// Мир с камнем под y = 63 у (0, 0) и сервер-симуляция; зона — 9 × 9 кусков вокруг (0, 0).
fn sim() -> (Sim, World, Zone) {
    let world = World::new(1);
    let s = Sim::new(&world, 1);
    let mut zone = Zone::default();
    zone.add_square(0, 0, 4);
    (s, world, zone)
}

fn put(s: &mut Sim, world: &World, p: Pos, c: Cell) {
    let mut ev = Events::default();
    let d = world.d.clone();
    let mut w = d.borrow_mut();
    s.set_world(&mut w, p, c, &mut ev);
}

fn tick(s: &mut Sim, zone: &Zone) {
    let mut ev = Events::default();
    s.tick_world(zone.clone(), &mut ev);
}

#[test]
fn grid_line_heat_to_infuser() {
    // Пуля: генератор у лавы (4 грани) → 10 кабелей → инфузор; энергия доходит, кабель разгорается, разрез — встаёт.
    let (mut s, world, zone) = sim();
    let y = 100;
    let g = [0, y, 0];
    for q in [[-1, y, 0], [1, y, 0], [0, y, -1], [0, y - 1, 0]] {
        put(&mut s, &world, q, LAVA as Cell);
    }
    put(&mut s, &world, g, cell_of(HEAT_GENERATOR, F_SOUTH as u16));
    for k in 0..10 {
        put(&mut s, &world, [k, y, 1], UNIVERSAL_CABLE as Cell);
    }
    let inf = [10, y, 1];
    put(&mut s, &world, inf, cell_of(METALLURGIC_INFUSER, F_WEST as u16));
    {
        let mut w = world.d.borrow_mut();
        // Рукава: первый кабель — к генератору (север) и к соседу (восток); последний — к инфузору.
        let a0 = arm_mask(crate::cell::state_of(w.get_cell(0, y, 1)));
        assert_eq!(a0, 1 << F_NORTH | 1 << F_EAST);
        let a9 = arm_mask(crate::cell::state_of(w.get_cell(9, y, 1)));
        assert_eq!(a9, 1 << F_WEST | 1 << F_EAST);
        let m = w.be_mut().mek_mut(inf).unwrap();
        m.slots[INF_EXTRA] = ItemStack::of(I_COAL, 4);
        m.slots[INF_IN] = ItemStack::of(I_IRON_INGOT, 2);
    }
    for _ in 0..600 {
        tick(&mut s, &zone);
    }
    let (glow, nets, cables, gen_e, inf_out) = {
        let w = world.d.borrow();
        let id = w.grid.net_at([0, y, 1]).unwrap();
        let n = w.grid.net(id).unwrap();
        (n.glow, w.grid.net_count(), n.members.len(), w.be().mek(g).unwrap().energy, w.be().mek(inf).unwrap().slots[INF_OUT])
    };
    assert_eq!((nets, cables), (1, 10));
    assert_eq!(glow, 1000, "поток идёт — сеть светится полностью");
    assert!(gen_e < 1000, "генератор отдаёт всё, что выработал: {gen_e}");
    // 600 тиков × ~50 Дж/т ≈ 28 кДж: инфузор сделал хотя бы одну операцию.
    assert!(inf_out.n() >= 1 && inf_out.item == I_ENRICHED_IRON, "{inf_out:?}");
    // Ломаю кабель посередине — две сети, у инфузора поток пропадает.
    put(&mut s, &world, [5, y, 1], AIR as Cell);
    tick(&mut s, &zone);
    let w = world.d.borrow();
    assert_eq!(w.grid.net_count(), 2);
    assert_ne!(w.grid.net_at([0, y, 1]), w.grid.net_at([9, y, 1]));
}

#[test]
fn grid_split_merge_reference() {
    // N-6: сеть из 10, запас 40 000, сломать 5-й → 4 (17 777) и 5 (22 223), номер у большей.
    let (mut s, world, zone) = sim();
    let y = 100;
    for k in 0..10 {
        put(&mut s, &world, [k, y, 0], UNIVERSAL_CABLE as Cell);
    }
    world.d.borrow_mut().commit_grid();
    let id = world.d.borrow().grid.net_at([0, y, 0]).unwrap();
    assert_eq!(world.d.borrow_mut().grid.insert_energy([3, y, 0], 40_000, false), 40_000);
    put(&mut s, &world, [4, y, 0], AIR as Cell);
    world.d.borrow_mut().commit_grid();
    {
        let w = world.d.borrow();
        let a = w.grid.net(w.grid.net_at([0, y, 0]).unwrap()).unwrap();
        let b = w.grid.net(w.grid.net_at([9, y, 0]).unwrap()).unwrap();
        assert_eq!((a.members.len(), a.stored, b.members.len(), b.stored), (4, 17_777, 5, 22_223));
        assert_eq!(w.grid.net_at([9, y, 0]), Some(id), "номер у большей части");
    }
    // N-10: кабель на место — одна сеть из 10, ёмкость 80 000, запас 40 000.
    put(&mut s, &world, [4, y, 0], UNIVERSAL_CABLE as Cell);
    world.d.borrow_mut().commit_grid();
    {
        let w = world.d.borrow();
        let n = w.grid.net(w.grid.net_at([0, y, 0]).unwrap()).unwrap();
        assert_eq!((w.grid.net_count(), n.members.len(), n.cap, n.stored), (1, 10, 80_000, 40_000));
    }
    // N-8: сломать крайний — одна сеть из 9, тот же номер, запас цел.
    put(&mut s, &world, [9, y, 0], AIR as Cell);
    world.d.borrow_mut().commit_grid();
    {
        let w = world.d.borrow();
        let n = w.grid.net(w.grid.net_at([0, y, 0]).unwrap()).unwrap();
        assert_eq!((w.grid.net_count(), n.members.len(), n.cap, n.stored), (1, 9, 72_000, 40_000));
    }
    // Слитая сеть стоит без потока: свечение — не ниже заполнения (40 000 / 72 000 → 555).
    tick(&mut s, &zone);
    let w = world.d.borrow();
    let n = w.grid.net(w.grid.net_at([0, y, 0]).unwrap()).unwrap();
    // Заполнение 40 000 / 72 000 = 555 — свечение не ниже заполнения.
    assert_eq!(n.glow, 555);
}

#[test]
fn grid_glow_n13() {
    // N-13: поток каждый тик, запас перед раздачей 200 из 8 000 → 25 (первый тик), 45, 65 …
    let (mut s, world, zone) = sim();
    let y = 100;
    put(&mut s, &world, [0, y, 0], UNIVERSAL_CABLE as Cell);
    put(&mut s, &world, [1, y, 0], cell_of(METALLURGIC_INFUSER, F_WEST as u16));
    tick(&mut s, &zone); // свести
    let mut glow = Vec::new();
    for _ in 0..4 {
        world.d.borrow_mut().grid.insert_energy([0, y, 0], 200, false);
        tick(&mut s, &zone);
        let w = world.d.borrow();
        glow.push(w.grid.net(w.grid.net_at([0, y, 0]).unwrap()).unwrap().glow);
    }
    assert_eq!(glow, vec![25, 45, 65, 85]);
}

#[test]
fn grid_two_machines_share() {
    // N-3: сеть из 1 и 2 пустых машины, 100 за тик → каждой по 50.
    let (mut s, world, zone) = sim();
    let y = 100;
    put(&mut s, &world, [0, y, 0], UNIVERSAL_CABLE as Cell);
    put(&mut s, &world, [1, y, 0], cell_of(METALLURGIC_INFUSER, F_WEST as u16));
    put(&mut s, &world, [-1, y, 0], cell_of(METALLURGIC_INFUSER, F_EAST as u16));
    tick(&mut s, &zone);
    world.d.borrow_mut().grid.insert_energy([0, y, 0], 100, false);
    tick(&mut s, &zone);
    let w = world.d.borrow();
    assert_eq!((w.be().mek([1, y, 0]).unwrap().energy, w.be().mek([-1, y, 0]).unwrap().energy), (50, 50));
}

#[test]
fn steel_tools_mek07() {
    // MEK-07 §4.3: стальная кирка — камень 6 тиков (0,3 с), булыжник 8, руда 12; железная — 8 / 10 / 15.
    use crate::registry::blocks::bdef;
    use crate::registry::items::{break_delta, can_harvest, I_WOODEN_PICKAXE};
    let ticks = |item: ItemId, b: crate::cell::BlockId| {
        let d = break_delta(item, bdef(b), true, false);
        let (mut p, mut n) = (0f32, 0);
        while p < 1.0 {
            p += d;
            n += 1;
        }
        n
    };
    let (steel, iron) = (I_STEEL_PICKAXE, I_WOODEN_PICKAXE + 10);
    assert_eq!((ticks(steel, STONE), ticks(steel, COBBLE), ticks(steel, OSMIUM_ORE), ticks(steel, GOLD_ORE)), (6, 8, 12, 12));
    assert_eq!((ticks(iron, STONE), ticks(iron, COBBLE), ticks(iron, IRON_ORE)), (8, 10, 15));
    assert_eq!(ticks(steel, STEEL_CASING), 14);
    // Ярус: золото и редстоун — железная кирка и выше; каменная — нет.
    let stone_pick = I_WOODEN_PICKAXE + 5;
    assert!(can_harvest(steel, bdef(GOLD_ORE)) && can_harvest(iron, bdef(REDSTONE_ORE)) && !can_harvest(stone_pick, bdef(GOLD_ORE)));
    assert!(can_harvest(stone_pick, bdef(OSMIUM_ORE)) && can_harvest(stone_pick, bdef(COPPER_ORE)));
    // Прочность 500, урон и скорость атаки — таблицей «тип × материал».
    assert_eq!(idef(steel).max_damage, 500);
    assert_eq!(crate::registry::items::tool_attack(I_STEEL_PICKAXE + 1), Some((11.0, 1.0)));
    assert_eq!(crate::registry::items::armor_toughness(I_STEEL_HELMET + 1), 2.0);
}

#[test]
fn ores_m1_by_height() {
    // Руды M1 есть и лежат на своих высотах (MEK-05 §2.2): осмий — в слое 12…45 и в горах, медь 18…76, золото ≤ 36,
    // редстоун ≤ 30, олово 12…59. 10 × 10 кусков у точки появления.
    use crate::gen::{generate, Gen, PAD};
    let gen = Gen::new(1337);
    let mut by: std::collections::HashMap<crate::cell::BlockId, (u32, i32, i32)> = Default::default();
    for cz in -5..5 {
        for cx in -5..5 {
            let reg = generate(&gen, cx, cz);
            for lz in PAD..PAD + crate::cell::CS {
                for lx in PAD..PAD + crate::cell::CS {
                    for y in 0..crate::cell::H {
                        let b = reg.get(lx, y, lz);
                        if [OSMIUM_ORE, TIN_ORE, COPPER_ORE, GOLD_ORE, REDSTONE_ORE, LAVA].contains(&b) {
                            let e = by.entry(b).or_insert((0, i32::MAX, i32::MIN));
                            e.0 += 1;
                            e.1 = e.1.min(y);
                            e.2 = e.2.max(y);
                        }
                    }
                }
            }
        }
    }
    let n = |b| by.get(&b).copied().unwrap_or((0, 0, 0));
    println!("руды на 100 кусков: осмий {:?}, олово {:?}, медь {:?}, золото {:?}, редстоун {:?}", n(OSMIUM_ORE), n(TIN_ORE), n(COPPER_ORE), n(GOLD_ORE), n(REDSTONE_ORE));
    assert!(n(OSMIUM_ORE).0 > 500 && n(TIN_ORE).0 > 1000 && n(COPPER_ORE).0 > 1000, "{by:?}");
    assert!(n(GOLD_ORE).0 > 100 && n(GOLD_ORE).2 <= 40 && n(REDSTONE_ORE).0 > 300 && n(REDSTONE_ORE).2 <= 34);
    assert!(n(TIN_ORE).1 >= 9 && n(TIN_ORE).2 <= 63 && n(COPPER_ORE).1 >= 15);
    // Подземные лавовые озёра: в среднем 1 кусок из 9, лава 6…42 (источник для теплового генератора).
    let lava = n(LAVA);
    println!("лава: {lava:?}");
    assert!(lava.0 > 200 && lava.1 >= 6 && lava.2 <= 42, "{lava:?}");
}

/// Камера или плавильня с батареей e и входом: тиков до t (1…t) — список (тик, выход) по каждому выходу.
fn run_chamber(kind: MekKind, input: ItemStack, e: u64, t: u32) -> (MekBe, Vec<(u32, i32)>) {
    let mut m = MekBe::new(kind);
    m.energy = e;
    m.slots[CH_IN] = input;
    let mut w = stub(0);
    let mut outs = Vec::new();
    let mut last = 0;
    for i in 1..=t {
        m.tick(P, F_NORTH, &mut w);
        let n = m.slots[CH_OUT].n();
        if n != last {
            outs.push((i, n));
            last = n;
        }
    }
    (m, outs)
}

#[test]
fn m06_1_enrich_raw_iron() {
    // M06-1: 9 сырого железа → 4 / 8 / 12 пыли на t200 / t400 / t600; потрачено 30 000 Дж.
    let (m, outs) = run_chamber(MekKind::Enrich, ItemStack::of(I_RAW_IRON, 9), 20_000, 600);
    // Полный буфер (20 000) — две операции; третья — с подпиткой ниже.
    assert_eq!(outs, vec![(200, 4), (400, 8)]);
    assert_eq!((m.energy, m.last), (0, Step::NoEnergy as u8));
    let mut m = MekBe::new(MekKind::Enrich);
    m.slots[CH_IN] = ItemStack::of(I_RAW_IRON, 9);
    let mut w = stub(0);
    let mut used = 0;
    for t in 1..=600 {
        let add = m.insert_energy(50); // кабель: ровно расход
        used += add;
        m.tick(P, F_NORTH, &mut w);
        if t == 200 || t == 400 || t == 600 {
            assert_eq!(m.slots[CH_OUT], ItemStack::of(I_DUST_IRON, 4 * t as i32 / 200), "t{t}");
        }
    }
    assert_eq!((used, m.energy), (30_000, 0));
    assert!(m.slots[CH_IN].is_empty());
}

#[test]
fn m06_2_raw_block_and_conveniences() {
    // M06-2: 1 блок сырого железа → 12 пыли на t200, 10 000 Дж.
    let (m, outs) = run_chamber(MekKind::Enrich, ItemStack::of(RAW_IRON_BLOCK as ItemId, 1), 20_000, 200);
    assert_eq!((outs, m.energy), (vec![(200, 12)], 10_000));
    // Уголь → обогащённый уголь, гравий → кремень, древесный уголь → обогащённый уголь (MEK-06 §3.1).
    for (inp, out) in [(I_COAL, I_ENRICHED_CARBON), (GRAVEL as ItemId, I_FLINT), (I_CHARCOAL, I_ENRICHED_CARBON), (I_REDSTONE, I_ENRICHED_REDSTONE)] {
        let (m, _) = run_chamber(MekKind::Enrich, ItemStack::of(inp, 1), 20_000, 200);
        assert_eq!(m.slots[CH_OUT], ItemStack::of(out, 1), "{inp}");
    }
    // 2 сырых — рецепт не начинается (вход меньше рецепта): прогресс 0, не «активна».
    let (m, outs) = run_chamber(MekKind::Enrich, ItemStack::of(I_RAW_OSMIUM, 2), 20_000, 50);
    assert_eq!((outs.len(), m.progress, m.last, m.active.shown), (0, 0, Step::Idle as u8, false));
    // Третий добавили — пошло: 4 пыли осмия.
    let mut m = m;
    m.slots[CH_IN].count = 3;
    let mut w = stub(0);
    for _ in 0..200 {
        m.tick(P, F_NORTH, &mut w);
    }
    assert_eq!(m.slots[CH_OUT], ItemStack::of(I_DUST_OSMIUM, 4));
}

#[test]
fn m06_3_smelter() {
    // M06-3: плавильня — 12 пыли железа → 12 слитков на t2400, 120 000 Дж (подача ровно расходом).
    let mut m = MekBe::new(MekKind::Smelter);
    m.slots[CH_IN] = ItemStack::of(I_DUST_IRON, 12);
    let mut w = stub(0);
    let mut used = 0;
    for t in 1..=2400 {
        used += m.insert_energy(50);
        m.tick(P, F_NORTH, &mut w);
        if t % 200 == 0 {
            assert_eq!(m.slots[CH_OUT].n(), t as i32 / 200, "t{t}");
        }
    }
    assert_eq!((m.slots[CH_OUT], used), (ItemStack::of(I_IRON_INGOT, 12), 120_000));
    // Песок → стекло, сырое железо → слиток, сырая медь → слиток, пыль осмия → слиток — каждый на t200 (как печь, без опыта).
    for (inp, out) in [(SAND as ItemId, GLASS as ItemId), (I_RAW_IRON, I_IRON_INGOT), (I_RAW_COPPER, I_COPPER_INGOT), (I_DUST_OSMIUM, I_OSMIUM_INGOT)] {
        let (m, outs) = run_chamber(MekKind::Smelter, ItemStack::of(inp, 1), 20_000, 200);
        assert_eq!((m.slots[CH_OUT], outs), (ItemStack::of(out, 1), vec![(200, 1)]), "{inp}");
    }
}

#[test]
fn m06_5_tables_and_slots() {
    // M06-5: нет двух рецептов камеры на один вход; пыль редстоуна — рецепт есть у камеры; вход слота — по таблице вида.
    for (i, a) in recipes::ENRICH.iter().enumerate() {
        assert!(recipes::ENRICH[i + 1..].iter().all(|b| b.input != a.input), "{a:?}");
        assert!(a.out_n <= idef(a.out).max_stack as i32);
    }
    assert!(role_accepts(MekKind::Enrich, Role::In, I_RAW_IRON));
    assert!(!role_accepts(MekKind::Enrich, Role::In, I_DUST_IRON));
    assert!(role_accepts(MekKind::Smelter, Role::In, I_DUST_IRON));
    assert!(role_accepts(MekKind::Smelter, Role::In, SAND as ItemId));
    assert!(!role_accepts(MekKind::Smelter, Role::In, I_STICK));
    assert!(!role_accepts(MekKind::Infuser, Role::In, I_RAW_IRON));
    assert!(role_accepts(MekKind::Enrich, Role::Energy, I_REDSTONE));
    // Энергия — всеми гранями, отдачи нет.
    assert_eq!(energy_io(MekKind::Enrich, F_NORTH, F_NORTH), (true, false));
    assert_eq!(energy_io(MekKind::Smelter, F_NORTH, F_EAST), (true, false));
}

#[test]
fn chamber_window() {
    // Окно камеры: энергия и прогресс из таблицы, подсказка расхода у работающей, предупреждение «нет энергии».
    let (m, _) = run_chamber(MekKind::Enrich, ItemStack::of(I_RAW_IRON, 3), 20_000, 100);
    let v = m.widget_values();
    assert_eq!(v.len(), 8, "энергия, прогресс, кнопки выталкивания и улучшений (M3)");
    assert_eq!((v[0], v[2]), (fill(15_000, 20_000, 54), fill(100, 200, 26)));
    assert!(m.tip(0).contains("Использует: 50 Дж/т"), "{}", m.tip(0));
    let (m, _) = run_chamber(MekKind::Enrich, ItemStack::of(I_RAW_IRON, 3), 10, 3);
    assert_eq!((m.widget_values()[1] & 1, m.tip(1).as_str()), (1, "Не хватает энергии"));
}
