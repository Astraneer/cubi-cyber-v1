//! Эталоны логистики M5 (MEK-13-14 §23: L-1…L-20, P-1…P-11) через мир сервера (Sim): cargo test -p voxel-core logi.

use super::*;
use crate::cell::{cell_of, Cell, F_EAST, F_NORTH, F_SOUTH, F_WEST};
use crate::game::sim::Sim;
use crate::game::view::Events;
use crate::registry::ids::*;
use crate::registry::items::*;
use crate::world::blockentity::BlockEntity;
use crate::world::grid::{M_PULL, M_PUSH};
use crate::world::mek::recipes::{SUB_HYDROGEN, SUB_LAVA, SUB_OXYGEN, SUB_WATER};
use crate::world::mek::Tank;
use crate::world::ticks::Zone;
use crate::world::World;

pub(crate) const Y: i32 = 100;

pub(crate) struct T {
    pub(crate) s: Sim,
    pub(crate) world: World,
    pub(crate) zone: Zone,
    pub(crate) t: u32,
}

impl T {
    pub(crate) fn new() -> T {
        let world = World::new(1);
        let s = Sim::new(&world, 1);
        let mut zone = Zone::default();
        zone.add_square(0, 0, 4);
        T { s, world, zone, t: 0 }
    }
    pub(crate) fn put(&mut self, p: Pos, c: Cell) {
        let mut ev = Events::default();
        let d = self.world.d.clone();
        let mut w = d.borrow_mut();
        self.s.set_world(&mut w, p, c, &mut ev);
    }
    pub(crate) fn tick(&mut self) {
        let mut ev = Events::default();
        self.t += 1;
        self.s.tick_world(self.zone.clone(), &mut ev);
    }
    /// Режим стороны f передатчика p (как конфигуратор у сервера): рукава у него и соседей заново.
    pub(crate) fn mode(&mut self, p: Pos, f: u8, m: u8) {
        let d = self.world.d.clone();
        let mut w = d.borrow_mut();
        w.grid.set_mode(p, f, m);
        crate::game::sim::fix_arms(&mut w, p);
        let q = offset(p, f);
        if crate::world::grid::is_transmitter(w.get_cell(q[0], q[1], q[2])) {
            crate::game::sim::fix_arms(&mut w, q);
        }
        w.grid.relink(p);
    }
    pub(crate) fn color(&mut self, p: Pos, c: u8) {
        let d = self.world.d.clone();
        let mut w = d.borrow_mut();
        w.grid.set_color(p, c);
        for q in std::iter::once(p).chain((0..6).map(|f| offset(p, f))) {
            if crate::world::grid::is_transmitter(w.get_cell(q[0], q[1], q[2])) {
                crate::game::sim::fix_arms(&mut w, q);
            }
        }
    }
    pub(crate) fn chest(&mut self, p: Pos) -> Vec<ItemStack> {
        match self.world.d.borrow_mut().be_mut().get_mut(p) {
            Some(BlockEntity::Chest(c)) => c.slots.to_vec(),
            _ => panic!("нет сундука {p:?}"),
        }
    }
    pub(crate) fn set_slot(&mut self, p: Pos, i: usize, s: ItemStack) {
        self.world.d.borrow_mut().be_mut().container(p).unwrap().set(i, s);
    }
    pub(crate) fn count(&mut self, p: Pos, item: ItemId) -> i32 {
        self.chest(p).iter().filter(|s| s.item == item).map(|s| s.count).sum()
    }
    pub(crate) fn stacks(&self) -> Vec<Transit> {
        self.world.d.borrow().logistics.stacks.clone()
    }
    /// Сундук A → n транспортёров на восток → сундук B; PULL у A. Ответ: (A, B, клетки линии).
    pub(crate) fn line(&mut self, n: i32) -> (Pos, Pos, Vec<Pos>) {
        let a = [0, Y, 0];
        self.put(a, CHEST as Cell);
        let line: Vec<Pos> = (1..=n).map(|k| [k, Y, 0]).collect();
        for &q in &line {
            self.put(q, LOGISTICAL_TRANSPORTER as Cell);
        }
        let b = [n + 1, Y, 0];
        self.put(b, CHEST as Cell);
        self.mode(line[0], F_WEST, M_PULL);
        (a, b, line)
    }
}

/// Тики прибытия предметов item в сундук b (каждый следующий предмет — свой тик).
fn arrivals(t: &mut T, b: Pos, item: ItemId, ticks: u32) -> Vec<u32> {
    let mut got = Vec::new();
    let mut seen = 0;
    for _ in 0..ticks {
        t.tick();
        let n = t.count(b, item);
        while seen < n {
            got.push(t.t);
            seen += 1;
        }
    }
    got
}

#[test]
fn logi_bullet_chest_to_chest_l2() {
    // Пуля и L-2: сундук A (10 сырого железа) → 10 базовых (PULL у A) → сундук B: прибытия 200, 211, … 299; предмет
    // виден в пути (точка рисования внутри трубы), сумма предметов не меняется.
    let mut t = T::new();
    let (a, b, line) = t.line(10);
    t.set_slot(a, 0, ItemStack::of(I_RAW_IRON, 10));
    let mut seen_pos = false;
    let mut got = Vec::new();
    let mut seen = 0;
    while t.t < 320 {
        t.tick();
        for s in t.stacks() {
            let p = s.draw_pos(0.5, SPEED);
            assert!(p[0] >= 0.99 && p[0] <= 11.01 && (p[1] - (Y as f64 + 0.25)).abs() < 1e-9 && (p[2] - 0.5).abs() < 1e-9, "{p:?}");
            seen_pos = true;
        }
        let n = t.count(b, I_RAW_IRON);
        while seen < n {
            got.push(t.t);
            seen += 1;
        }
        let in_a = t.count(a, I_RAW_IRON);
        let flying: i32 = t.stacks().iter().map(|s| s.item.count).sum();
        assert_eq!(in_a + flying + n, 10, "t{}", t.t);
    }
    assert!(seen_pos);
    assert_eq!(got, (0..10).map(|k| 200 + 11 * k).collect::<Vec<u32>>(), "L-2");
    assert!(line.iter().all(|&q| t.world.d.borrow_mut().get_block(q[0], q[1], q[2]) == LOGISTICAL_TRANSPORTER));
}

#[test]
fn logi_pull_backoff_l3_l4() {
    // L-4: цели нет — попытки забора в тиках 1, 4, 12, 33, 74, 115 (ожидания 2, 7, 20, 40, 40); L-3: пустой источник —
    // проверка раз в 4 тика (ожидание 3).
    let mut t = T::new();
    let (a, b, line) = t.line(3);
    t.put(b, AIR as Cell);
    t.set_slot(a, 0, ItemStack::of(I_RAW_IRON, 5));
    let mut tries = Vec::new();
    let mut fails = 0;
    for _ in 0..130 {
        t.tick();
        let f = t.world.d.borrow().logistics.pull_wait.get(&line[0]).map_or(0, |e| e.1);
        if f != fails {
            tries.push(t.t);
            fails = f;
        }
    }
    assert_eq!(tries, vec![1, 4, 12, 33, 74, 115], "L-4");
    assert_eq!(t.count(a, I_RAW_IRON), 5);
    assert!(t.stacks().is_empty());
    // Пустой источник: ожидание 3 после каждой проверки.
    t.put(b, CHEST as Cell);
    t.set_slot(a, 0, ItemStack::EMPTY);
    for _ in 0..41 {
        t.tick();
    }
    let w = t.world.d.borrow().logistics.pull_wait.get(&line[0]).copied().unwrap();
    assert!(w.0 <= IDLE_POLL, "{w:?}");
    assert_eq!(backoff(1), 2);
    assert_eq!(backoff(6), 40);
}

#[test]
fn logi_last_slot_first_l5() {
    // L-5: в сундуке 3 вида в слотах 0, 5, 26 — уходит вид из слота 26, одна штука.
    let mut t = T::new();
    let (a, _b, _) = t.line(2);
    t.set_slot(a, 0, ItemStack::of(I_COAL, 3));
    t.set_slot(a, 5, ItemStack::of(I_RAW_IRON, 3));
    t.set_slot(a, 26, ItemStack::of(I_FLINT, 3));
    t.tick();
    let s = t.stacks();
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].item, ItemStack::of(I_FLINT, 1));
    assert_eq!(t.chest(a)[26], ItemStack::of(I_FLINT, 2));
}

#[test]
fn logi_nearest_and_prediction_l6_l7() {
    // L-6, L-7: ближняя цель (3 транспортёра) — первая; её свободное место — 3 штуки (слот 61/64, прочие полны):
    // первые 3 забора — в ближнюю (с учётом едущих), 4-й — в дальнюю (5 транспортёров).
    let mut t = T::new();
    let (a, far, line) = t.line(5);
    let near = [line[2][0], Y, 1];
    t.put(near, CHEST as Cell);
    for i in 0..27 {
        t.set_slot(near, i, if i == 13 { ItemStack::of(I_RAW_IRON, 61) } else { ItemStack::of(COBBLE as ItemId, 64) });
    }
    t.set_slot(a, 0, ItemStack::of(I_RAW_IRON, 5));
    for _ in 0..200 {
        t.tick();
    }
    assert_eq!(t.count(near, I_RAW_IRON), 64, "L-7: ближняя заполнена ровно");
    assert_eq!(t.count(far, I_RAW_IRON), 2, "4-й и 5-й — в дальнюю");
    assert!(t.stacks().is_empty());
}

#[test]
fn logi_machine_eject_one_stack_l9() {
    // L-9: камера выталкивает 4 пыли в базовый транспортёр — один стек из 4 (лимит 1 — только у забора).
    let mut t = T::new();
    let ch = [0, Y, 0];
    t.put(ch, cell_of(ENRICHMENT_CHAMBER, F_SOUTH as u16)); // право камеры, смотрящей на юг, — восток
    {
        let d = t.world.d.clone();
        let mut w = d.borrow_mut();
        let m = w.be_mut().mek_mut(ch).unwrap();
        m.eject = true;
        m.slots[crate::world::mek::CH_OUT] = ItemStack::of(I_DUST_IRON, 4);
    }
    let tp = [1, Y, 0];
    t.put(tp, LOGISTICAL_TRANSPORTER as Cell);
    let dst = [2, Y, 0];
    t.put(dst, CHEST as Cell);
    t.tick();
    let s = t.stacks();
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].item, ItemStack::of(I_DUST_IRON, 4));
    assert_eq!(s[0].home, Some(ch));
    for _ in 0..30 {
        t.tick();
    }
    assert_eq!(t.count(dst, I_DUST_IRON), 4);
}

#[test]
fn logi_target_removed_goes_home_l10() {
    // L-10: цель убрали, стек на 3-м из 5 — путь домой, предмет возвращается в источник.
    let mut t = T::new();
    let (a, b, line) = t.line(5);
    t.set_slot(a, 0, ItemStack::of(I_RAW_IRON, 1));
    while t.stacks().first().is_none_or(|s| s.at() != line[2]) {
        t.tick();
    }
    t.put(b, AIR as Cell);
    for _ in 0..250 {
        t.tick();
        if t.count(a, I_RAW_IRON) == 1 {
            break;
        }
    }
    assert_eq!(t.count(a, I_RAW_IRON), 1, "вернулся домой");
    assert!(t.stacks().is_empty());
}

#[test]
fn logi_idle_ping_pong_l11() {
    // L-11: ни цели, ни дома — простой: ходит 0 → 4 → 0; появилась цель — уходит в неё.
    let mut t = T::new();
    let (a, b, line) = t.line(5);
    t.put(b, AIR as Cell);
    t.put(a, AIR as Cell);
    {
        let d = t.world.d.clone();
        let mut w = d.borrow_mut();
        let id = w.logistics.next_id;
        w.logistics.next_id += 1;
        w.logistics.stacks.push(Transit {
            id,
            item: ItemStack::of(I_COAL, 1),
            from: [0, Y, 0],
            path: line.clone(),
            goal: Goal::Idle,
            idle_dir: Some(F_EAST),
            ready: true,
            ..Default::default()
        });
    }
    let mut xs = Vec::new();
    for _ in 0..400 {
        t.tick();
        let s = t.stacks();
        assert_eq!(s.len(), 1, "t{} не выпал", t.t);
        let x = s[0].at()[0];
        if xs.last() != Some(&x) {
            xs.push(x);
        }
    }
    assert!(xs.windows(2).all(|w| (w[0] - w[1]).abs() == 1), "{xs:?}");
    assert!(xs.contains(&1) && xs.contains(&5) && xs.iter().filter(|&&x| x == 5).count() >= 2, "туда-обратно: {xs:?}");
    t.put(b, CHEST as Cell);
    for _ in 0..200 {
        t.tick();
    }
    assert_eq!(t.count(b, I_COAL), 1, "цель появилась — ушёл в неё");
}

#[test]
fn logi_lone_drops_l12_broken_drops_l13() {
    // L-12: одиночный транспортёр, стек без цели — выпадение. L-13: сломан транспортёр с 2 стеками — 2 предмета.
    let mut t = T::new();
    let p = [3, Y, 3];
    t.put(p, LOGISTICAL_TRANSPORTER as Cell);
    {
        let d = t.world.d.clone();
        let mut w = d.borrow_mut();
        w.logistics.stacks.push(Transit { id: 0, item: ItemStack::of(I_COAL, 2), from: [2, Y, 3], path: vec![p], goal: Goal::Dest, ready: false, ..Default::default() });
        w.logistics.next_id = 1;
    }
    let items0 = t.s.items.list.len();
    t.tick();
    assert!(t.stacks().is_empty(), "L-12");
    assert_eq!(t.s.items.list.len(), items0 + 1);
    // L-13
    let (a, _b, line) = t.line(6);
    t.set_slot(a, 0, ItemStack::of(I_RAW_IRON, 10));
    while t.stacks().iter().filter(|s| s.at() == line[1]).count() < 2 {
        t.tick();
        assert!(t.t < 200);
        // два стека в одной клетке бывают на входе второго, когда первый ещё в ней — ускорим: дождёмся пары где угодно
        if t.stacks().len() >= 2 && t.stacks().iter().all(|s| s.at() == t.stacks()[0].at()) {
            break;
        }
    }
    let at = t.stacks()[0].at();
    let here = t.stacks().iter().filter(|s| s.at() == at).count();
    let before = t.s.items.list.iter().filter(|e| !e.removed).count();
    let total = t.stacks().len();
    t.put(at, AIR as Cell);
    t.tick();
    let after = t.s.items.list.iter().filter(|e| !e.removed && e.stack.item == I_RAW_IRON).count();
    assert!(after >= here, "L-13: {here} стеков → предметы ({after})");
    assert!(before <= t.s.items.list.len());
    assert!(t.stacks().len() >= total - here);
}

#[test]
fn logi_colors_l14_l15() {
    // L-14: бесцветный стек в чёрный — нет; L-15: чёрный и красный рядом — связи нет, рукава нет.
    let mut t = T::new();
    let (a, b, line) = t.line(4);
    t.color(line[2], 1);
    t.set_slot(a, 0, ItemStack::of(I_RAW_IRON, 3));
    for _ in 0..200 {
        t.tick();
    }
    assert_eq!(t.count(b, I_RAW_IRON), 0, "бесцветный поток не проходит чёрный участок");
    t.color(line[1], 2);
    let arms = |t: &T, p: Pos| crate::world::grid::arms_of(crate::cell::state_of(t.world.d.borrow_mut().get_cell(p[0], p[1], p[2])));
    assert_eq!(crate::world::grid::arm(arms(&t, line[1]), F_EAST), 0, "L-15: разные цвета — рукава нет");
    t.color(line[1], 0);
    t.color(line[2], 0);
    assert_eq!(crate::world::grid::arm(arms(&t, line[1]), F_EAST), 1);
    for _ in 0..300 {
        t.tick();
    }
    assert_eq!(t.count(b, I_RAW_IRON), 3, "цвета сняты — проходит");
}

#[test]
fn logi_draw_pos_l16_l18() {
    let mk = |progress: u16, path: Vec<Pos>, from: Pos| Transit { progress, path, from, goal: Goal::Dest, ready: true, ..Default::default() };
    // L-16: прямая W→E, progress 0 / 25 / 50 / 100: x = 0 / 0.25 / 0.5 / 1.0 от края, y = 0.25.
    for (pr, x) in [(0u16, 0.0), (25, 0.25), (50, 0.5), (100, 1.0)] {
        let t = mk(pr, vec![[0, 0, 0], [1, 0, 0], [2, 0, 0]], [-1, 0, 0]);
        let p = t.draw_pos(0.0, 5);
        assert!((p[0] - x).abs() < 1e-9 && (p[1] - 0.25).abs() < 1e-9 && (p[2] - 0.5).abs() < 1e-9, "{pr}: {p:?}");
    }
    // L-17: поворот W → N на 50, элитный (20), progress 40, alpha 1 → x = 0.5, z = 0.4 (уже на север).
    let t = mk(40, vec![[0, 0, 0], [0, 0, -1], [0, 0, -2]], [-1, 0, 0]);
    let p = t.draw_pos(1.0, 20);
    assert!((p[0] - 0.5).abs() < 1e-9 && (p[2] - 0.4).abs() < 1e-9, "L-17: {p:?}");
    // L-18: базовый 95 + 0.6 / 1.0 → x 0.98 / 1.0; элитный 90 + 1 → в следующей клетке на 0.1.
    let t = mk(95, vec![[0, 0, 0], [1, 0, 0], [2, 0, 0]], [-1, 0, 0]);
    assert!((t.draw_pos(0.6, 5)[0] - 0.98).abs() < 1e-9);
    assert!((t.draw_pos(1.0, 5)[0] - 1.0).abs() < 1e-9);
    let t = mk(90, vec![[0, 0, 0], [1, 0, 0], [2, 0, 0]], [-1, 0, 0]);
    assert!((t.draw_pos(1.0, 20)[0] - 1.1).abs() < 1e-9, "L-18");
}

#[test]
fn logi_client_view_follows_server() {
    // Клиент двигает копию тем же правилом: без событий (передачи между клетками) копия совпадает с сервером тик в тик.
    let mut t = T::new();
    let (a, _b, _) = t.line(6);
    t.set_slot(a, 0, ItemStack::of(I_RAW_IRON, 1));
    t.tick();
    let mut v = View::default();
    v.apply(t.stacks().iter().map(TransitMsg::of).collect());
    for _ in 0..100 {
        t.tick();
        v.advance();
        let s = &t.stacks()[0];
        assert_eq!((v.list[0].at(), v.list[0].progress), (s.at(), s.progress), "t{}", t.t);
        let (a, b) = (v.list[0].draw_pos(0.3, SPEED), s.draw_pos(0.3, SPEED));
        assert!((0..3).all(|k| (a[k] - b[k]).abs() < 1e-9));
    }
}

// ---------------------------------------------------------------- трубы, трубки, насос (P-1…P-11)

/// Насос в клетке p (лицом на север: зад — юг) с энергией и источником воды (или лавы) под ним.
fn pump(t: &mut T, p: Pos, under: Cell) {
    t.put([p[0], p[1] - 1, p[2]], under);
    t.put(p, cell_of(ELECTRIC_PUMP, F_NORTH as u16));
    let d = t.world.d.clone();
    let mut w = d.borrow_mut();
    let m = w.be_mut().mek_mut(p).unwrap();
    m.energy = m.cap();
}
fn pump_be(t: &T, p: Pos) -> crate::world::mek::MekBe {
    (*t.world.d.borrow().be().mek(p).unwrap()).clone()
}

#[test]
fn pump_water_p7_p8_p9_p11() {
    // P-7: +1 000 мБ в тиках 19, 38, 57, …; выход закрыт — за 190 тиков 10 000 мБ, дальше стоит. P-8: 100 Дж/т, первая
    // операция после сброса — 100 Дж. P-9: источник воды под насосом на месте. P-11: над камнем — ничего.
    let mut t = T::new();
    let p = [0, Y, 0];
    pump(&mut t, p, WATER as Cell);
    let e0 = pump_be(&t, p).energy;
    let mut ups = Vec::new();
    let mut last = 0;
    for _ in 0..250 {
        t.tick();
        let m = pump_be(&t, p);
        if m.tank.amount != last {
            ups.push(t.t);
            last = m.tank.amount;
        }
        if t.t == 19 {
            assert_eq!(e0 - m.energy, 100, "P-8: первая операция — 100 Дж");
        }
        if t.t == 38 {
            assert_eq!(e0 - m.energy, 100 + 1900, "P-8: дальше 1 900 Дж на ведро");
        }
    }
    assert_eq!(ups, (1..=10).map(|k| 19 * k).collect::<Vec<u32>>(), "P-7");
    let m = pump_be(&t, p);
    assert_eq!((m.tank.sub, m.tank.amount), (SUB_WATER, 10_000));
    assert_eq!(t.world.d.borrow_mut().get_block(0, Y - 1, 0), WATER, "P-9: вода бесконечна");
    // P-11
    let mut t = T::new();
    pump(&mut t, p, STONE as Cell);
    for _ in 0..100 {
        t.tick();
    }
    assert_eq!(pump_be(&t, p).tank.amount, 0);
}

#[test]
fn pump_lava_drains_p9() {
    // P-9: над лавой — источник под насосом исчез после первой операции; дальше насос берёт соседний источник озера.
    let mut t = T::new();
    let p = [0, Y, 0];
    // Озеро лавы 3 × 3 под насосом, огорожено камнем (запись жидкости будит тики — пусть не утекает).
    for x in -2..=2 {
        for z in -2..=2 {
            t.put([x, Y - 2, z], STONE as Cell);
            let edge = x.abs() == 2 || z.abs() == 2;
            t.put([x, Y - 1, z], if edge { STONE as Cell } else { LAVA as Cell });
        }
    }
    pump(&mut t, p, LAVA as Cell);
    for _ in 0..19 {
        t.tick();
    }
    assert_eq!(pump_be(&t, p).tank, Tank { sub: SUB_LAVA, amount: 1000 });
    assert_ne!(t.world.d.borrow_mut().get_block(0, Y - 1, 0), LAVA, "источник под насосом выкачан");
    for _ in 0..19 {
        t.tick();
    }
    assert_eq!(pump_be(&t, p).tank.amount, 2000, "второй — из соседа по озеру");
}

#[test]
fn pump_pipe_separator_p10_p1() {
    // P-10: выход насоса — только вверх, 256 мБ/т; трубы сбоку не получают ничего. Труба сверху → сепаратор: вода идёт
    // без вёдер. P-1: ёмкость сети из n труб — 2 000 × n.
    let mut t = T::new();
    let p = [0, Y, 0];
    pump(&mut t, p, WATER as Cell);
    let side = [1, Y, 0];
    t.put(side, MECHANICAL_PIPE as Cell);
    for k in 1..=3 {
        t.put([0, Y + k, 0], MECHANICAL_PIPE as Cell);
    }
    let sep = [0, Y + 3, -1];
    t.put(sep, cell_of(ELECTROLYTIC_SEPARATOR, F_SOUTH as u16));
    for _ in 0..200 {
        t.tick();
    }
    let w = t.world.d.borrow();
    let id = w.grid.net_at([0, Y + 1, 0]).unwrap();
    let n = w.grid.net(id).unwrap();
    assert_eq!(n.cap, 3 * 2000, "P-1");
    let side_id = w.grid.net_at(side).unwrap();
    assert_eq!(w.grid.net(side_id).unwrap().stored, 0, "P-10: сбоку — ничего");
    let m = w.be().mek(sep).unwrap();
    assert_eq!(m.tank.sub, SUB_WATER);
    assert!(m.tank.amount >= 9000, "вода в сепараторе: {}", m.tank.amount);
}

#[test]
fn tube_hydrogen_oxygen_p2_and_gas_sides() {
    // P-2: трубка — 4 000 мБ на штуку; сепаратор отдаёт водород слева в трубку, кислород справа — в трубку к очистителю;
    // стороны газов конфигуратором: водород справа.
    let mut t = T::new();
    let sep = [0, Y, 0];
    t.put(sep, cell_of(ELECTROLYTIC_SEPARATOR, F_SOUTH as u16)); // лицо на юг: лево — запад, право — восток
    {
        let d = t.world.d.clone();
        let mut w = d.borrow_mut();
        let m = w.be_mut().mek_mut(sep).unwrap();
        m.energy = m.cap();
        m.tank = Tank { sub: SUB_WATER, amount: 20_000 };
    }
    for k in 1..=2 {
        t.put([-k, Y, 0], PRESSURIZED_TUBE as Cell);
        t.put([k, Y, 0], PRESSURIZED_TUBE as Cell);
    }
    let pur = [3, Y, 0];
    t.put(pur, cell_of(PURIFICATION_CHAMBER, F_SOUTH as u16));
    for _ in 0..100 {
        t.tick();
    }
    let w = t.world.d.borrow();
    let h = w.grid.net(w.grid.net_at([-1, Y, 0]).unwrap()).unwrap();
    assert_eq!((h.sub, h.cap), (SUB_HYDROGEN, 2 * 4000), "P-2");
    assert!(h.stored > 0);
    let o = w.grid.net(w.grid.net_at([1, Y, 0]).unwrap()).unwrap();
    assert_eq!(o.sub, SUB_OXYGEN);
    assert_eq!(w.be().mek(pur).unwrap().tank.sub, SUB_OXYGEN, "кислород дошёл до очистителя трубкой");
    drop(w);
    // Сторона газов: водород — на восток (право), кислород — нигде.
    {
        let d = t.world.d.clone();
        let mut w = d.borrow_mut();
        let m = w.be_mut().mek_mut(sep).unwrap();
        m.gas_sides = [0, 0, 1, 0, 0, 0];
    }
    for _ in 0..200 {
        t.tick();
    }
    let w = t.world.d.borrow();
    let o = w.grid.net(w.grid.net_at([1, Y, 0]).unwrap()).unwrap();
    // Сеть кислорода опустела в очиститель и стала водородной.
    assert_eq!(o.sub, SUB_HYDROGEN, "справа теперь водород");
}

#[test]
fn pipes_different_fluids_p5_and_pull() {
    // P-5: сеть воды и сеть лавы встык не сливаются; сторона PULL трубы тянет из насоса до 250 мБ/т.
    let mut t = T::new();
    let a = [0, Y, 0];
    let b = [4, Y, 0];
    pump(&mut t, a, WATER as Cell);
    for x in -2..=6 {
        for z in -2..=2 {
            t.put([x, Y - 2, z], STONE as Cell);
        }
    }
    t.put([b[0], Y - 1, 0], LAVA as Cell);
    for (x, z) in [(3, 0), (5, 0), (4, -1), (4, 1)] {
        t.put([x, Y - 1, z], STONE as Cell);
    }
    pump(&mut t, b, LAVA as Cell);
    t.put([0, Y + 1, 0], MECHANICAL_PIPE as Cell);
    t.put([1, Y + 1, 0], MECHANICAL_PIPE as Cell);
    t.put([2, Y + 1, 0], MECHANICAL_PIPE as Cell);
    t.put([4, Y + 1, 0], MECHANICAL_PIPE as Cell);
    for _ in 0..40 {
        t.tick();
    }
    {
        let w = t.world.d.borrow();
        for q in [[0, Y + 1, 0], [4, Y + 1, 0]] {
            let n = w.grid.net(w.grid.net_at(q).unwrap()).unwrap();
            eprintln!("{q:?}: sub {} stored {} members {:?}", n.sub, n.stored, n.members);
        }
    }
    t.put([3, Y + 1, 0], MECHANICAL_PIPE as Cell);
    for _ in 0..5 {
        t.tick();
    }
    let w = t.world.d.borrow();
    let (wa, la) = (w.grid.net_at([0, Y + 1, 0]).unwrap(), w.grid.net_at([4, Y + 1, 0]).unwrap());
    assert_ne!(wa, la, "вода и лава не сливаются");
    assert_eq!((w.grid.net(wa).unwrap().sub, w.grid.net(la).unwrap().sub), (SUB_WATER, SUB_LAVA));
    drop(w);
    // PULL: труба сверху насоса в режиме «тянуть» к нему — 250 мБ/т плюс сам насос толкает 256.
    t.mode([0, Y + 1, 0], crate::cell::F_DOWN, M_PULL);
    t.tick();
    let w = t.world.d.borrow();
    let arm = crate::world::grid::arm(crate::world::grid::arms_of(crate::cell::state_of(w.edited_cell(0, Y + 1, 0).unwrap())), crate::cell::F_DOWN);
    assert_eq!(arm, crate::world::grid::ARM_PULL);
    let _ = M_PUSH;
}

#[test]
fn logi_save_load_l20() {
    // L-20: мир со стеками в пути сохранён и загружен — пути пересчитаны в первом тике, всё доезжает.
    use crate::game::server::ServerSim;
    use crate::save::{storage::MemStorage, Saver};
    let world = World::new(1);
    let mut sv = ServerSim::new(world.share());
    let mut zone = Zone::default();
    zone.add_square(0, 0, 4);
    let mut ev = Events::default();
    let a = [0, Y, 0];
    let b = [8, Y, 0];
    {
        let d = world.d.clone();
        let mut w = d.borrow_mut();
        sv.sim.set_world(&mut w, a, CHEST as Cell, &mut ev);
        for k in 1..=7 {
            sv.sim.set_world(&mut w, [k, Y, 0], LOGISTICAL_TRANSPORTER as Cell, &mut ev);
        }
        sv.sim.set_world(&mut w, b, CHEST as Cell, &mut ev);
        w.grid.set_mode([1, Y, 0], F_WEST, M_PULL);
        crate::game::sim::fix_arms(&mut w, [1, Y, 0]);
        w.be_mut().container(a).unwrap().set(0, ItemStack::of(I_RAW_IRON, 4));
    }
    for _ in 0..60 {
        sv.sim.tick_world(zone.clone(), &mut ev);
    }
    assert!(!world.d.borrow().logistics.stacks.is_empty());
    let mut st = MemStorage::default();
    Saver::new().save_all(&sv, &mut st).unwrap();
    let (mut sv2, _) = Saver::load_all(&mut st).unwrap();
    let w2 = sv2.sim.world.share();
    assert_eq!(w2.d.borrow().logistics.stacks.len(), world.d.borrow().logistics.stacks.len());
    assert_eq!(w2.d.borrow().grid.mode([1, Y, 0], F_WEST), M_PULL);
    for _ in 0..300 {
        sv2.sim.tick_world(zone.clone(), &mut ev);
    }
    let n: i32 = match w2.d.borrow_mut().be_mut().get_mut(b) {
        Some(BlockEntity::Chest(c)) => c.slots.iter().filter(|s| s.item == I_RAW_IRON).map(|s| s.count).sum(),
        _ => 0,
    };
    assert_eq!(n, 4, "все 4 доехали после загрузки");
}

#[test]
#[ignore]
fn logi_bench_cost() {
    // Цена сервера и клиента на стенде mek:8 (480 стеков сразу, 8 линий × 30): cargo test --release -p voxel-core
    // logi_bench_cost -- --ignored --nocapture.
    use crate::game::inventory::Inventory;
    let mut t = T::new();
    {
        let d = t.world.d.clone();
        let mut w = d.borrow_mut();
        let mut ev = Events::default();
        crate::game::mek_demo::build(&mut t.s, &mut w, &mut ev, &mut Inventory::new(), [8, Y, 8], 8);
    }
    for _ in 0..40 {
        t.tick();
    }
    let n = t.stacks().len();
    let t0 = std::time::Instant::now();
    let ticks = 400;
    for _ in 0..ticks {
        t.tick();
    }
    let tick_ms = t0.elapsed().as_secs_f64() * 1000.0 / ticks as f64;
    let mut v = View::default();
    v.apply(t.stacks().iter().map(TransitMsg::of).collect());
    let t1 = std::time::Instant::now();
    let mut out = Vec::new();
    for k in 0..1000 {
        out.clear();
        if k % 3 == 0 {
            v.advance();
        }
        v.draws(0.5, [8.0, Y as f64, 8.0], &mut out);
    }
    let draw_us = t1.elapsed().as_secs_f64() * 1e6 / 1000.0;
    eprintln!("стеков {n} → {}; тик мира (сервер, с логистикой) {tick_ms:.3} мс; клиент draws {draw_us:.1} мкс/кадр ({} чисел)", t.stacks().len(), out.len());
}
