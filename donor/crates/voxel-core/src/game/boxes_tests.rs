//! G8 «сундуки и печи по сети» (MP-5, design.md §3.7 / §4.4): окна блок-сущностей у двоих игроков через транспорт с
//! задержкой 75 ± 20 мс (тот же `LagNet`, что в net_tests): двое у одного сундука перекладывают наперегонки — ни
//! потерь, ни дублей; печь — огонь и стрелка у обоих; крышка — всем; ломание сундука с открытым окном у другого.

use std::collections::BTreeMap;

use super::net_tests::{put_on, LagNet};
use super::*;
use crate::abi::*;
use crate::cell::{cell_of, state_of, Cell, State, F_NORTH};
use crate::game::inventory::OFFHAND;
use crate::registry::blocks::ST_LIT;
use crate::registry::ids::{FURNACE, METALLURGIC_INFUSER};
use crate::registry::items::{I_COAL, I_RAW_IRON, I_STICK, I_WOODEN_PICKAXE};
use crate::world::blockentity::{FURNACE_FUEL, FURNACE_IN, FURNACE_OUT};
use crate::world::stack::Container;

/// Сундук стоит напротив A (с юга от него) и B (с севера).
const CHEST_AT: [i32; 3] = [0, 101, 2];

/// Двое по обе стороны блока [0, 101, 2]: A в (0.5, 0.5) смотрит на юг, B в (0.5, 4.5) — на север. У игроков —
/// вещи (`give` — команда отладки, сервер с --dev). Содержимое блок-сущности заполняет тест.
fn box_net(cell: Cell, delay: u64, jitter: i64) -> LagNet {
    let mut net = LagNet::new(&["A", "B"], delay, jitter);
    net.until_ready();
    put_on(&mut net, 0, 0.5, 0.5);
    put_on(&mut net, 1, 0.5, 4.5);
    net.sv.sim.world_edit(CHEST_AT, cell);
    for (k, (a, b, c)) in [(0usize, (DIRT as ItemId, 200, STONE as ItemId)), (1, (STONE as ItemId, 150, I_COAL))] {
        net.peer(k).give(a, b);
        net.peer(k).give(c, 33);
    }
    net.peer(0).set_look(0.0, 35.0);
    net.peer(1).set_look(180.0, 35.0);
    net.ticks(12, |_, _| {});
    net
}

/// Сундук с несколькими стеками у сервера.
fn chest_net(delay: u64, jitter: i64) -> LagNet {
    let net = box_net(cell_of(CHEST, F_NORTH as State), delay, jitter);
    {
        let d = net.sv.sim.sim.world.d.clone();
        let mut w = d.borrow_mut();
        let c = w.be_mut().chest_mut(CHEST_AT).expect("сундук у сервера");
        c.slots[0] = ItemStack::of(DIRT as ItemId, 40);
        c.slots[1] = ItemStack::of(STONE as ItemId, 64);
        c.slots[5] = ItemStack::of(I_STICK, 17);
        c.slots[13] = ItemStack { item: I_WOODEN_PICKAXE, count: 1, damage: 12 };
    }
    net
}

/// Оба открывают блок ПКМ и ждут `WinOpen`.
fn open_both(net: &mut LagNet) {
    for k in 0..2 {
        assert_eq!(net.peer(k).target(), Some(CHEST_AT), "игрок {k} смотрит на блок");
        net.peer(k).press(P_USE);
        net.ticks(8, |_, _| {});
        assert!(net.peer(k).client.sim.ctl.screen.is_some(), "окно открылось у игрока {k}");
    }
}

/// Игрок сервера, которому принадлежит клиент k (порядок входа при разбросе задержки не обязан совпадать).
fn server_of(net: &LagNet, k: usize) -> &super::server::ServerPlayer {
    let id = net.client(k).me;
    net.sv.sim.players.iter().find(|p| p.id == id).expect("игрок на сервере")
}

fn add(m: &mut BTreeMap<u32, i64>, s: ItemStack) {
    if !s.is_empty() {
        *m.entry(s.id() as u32).or_default() += s.n() as i64;
    }
}

/// Все предметы сервера: сундук, инвентари, курсоры и сетки открытых окон, предметы на земле.
fn tally(net: &LagNet) -> BTreeMap<u32, i64> {
    let mut m = BTreeMap::new();
    let sv = &net.sv.sim;
    if let Some(c) = sv.sim.world.d.borrow().be().chest(CHEST_AT) {
        c.slots.iter().for_each(|&s| add(&mut m, s));
    }
    for p in &sv.players {
        (0..=OFFHAND).for_each(|i| add(&mut m, p.inv.get(i)));
        if let Some(w) = &p.screen {
            add(&mut m, w.cursor);
            w.grid.cells.iter().for_each(|&s| add(&mut m, s));
        }
    }
    for e in sv.sim.items.list.iter().filter(|e| !e.removed) {
        add(&mut m, e.stack);
    }
    m
}

/// Один случайный жест над окном сундука (63 слота: 0…26 сундук, 27…62 рюкзак и хотбар): клик, Shift, число, двойной
/// клик, Q, клик за окном, дв. клики ЛКМ-тяга и ПКМ-тяга (как их шлёт screen.js).
fn random_gesture(rng: &mut GameRng, sim: &mut super::client::ClientSim, drops: bool) -> usize {
    let slot = rng.below(63);
    let mut r = rng.below(100);
    if !drops && (84..=89).contains(&r) {
        r = 0; // без выбросов: предмет на земле через 5 минут (6000 тиков) исчезает по правилам игры
    }
    match r {
        0..=54 => sim.screen_click(slot, rng.below(2), A_PICKUP),
        55..=71 => sim.screen_click(slot, 0, A_QUICK_MOVE),
        72..=77 => sim.screen_click(slot, rng.below(9), A_SWAP),
        78..=83 => sim.screen_click(slot, 0, A_PICKUP_ALL),
        84..=86 => sim.screen_click(slot, rng.below(2), A_THROW),
        87..=89 => sim.screen_click(SLOT_OUTSIDE, rng.below(2), A_PICKUP),
        90..=92 => {
            let s = sim.ctl.inv.get(rng.below(41) as usize);
            sim.screen_quick_move_all(slot, s.id(), s.damage);
        }
        _ => {
            let mode = rng.below(2);
            sim.screen_click(SLOT_OUTSIDE, mode << 2, A_QUICK_CRAFT);
            for _ in 0..rng.range(1, 4) {
                sim.screen_click(rng.below(63), (mode << 2) | 1, A_QUICK_CRAFT);
            }
            sim.screen_click(SLOT_OUTSIDE, (mode << 2) | 2, A_QUICK_CRAFT);
            return 5;
        }
    }
    1
}

/// Инвентарь клиента k совпадает с серверным.
fn assert_synced_inv(net: &LagNet, k: usize) {
    let sv = server_of(net, k);
    for i in 0..=OFFHAND {
        assert_eq!(net.client(k).ctl.inv.get(i), sv.inv.get(i), "инвентарь клиента {k}, ячейка {i}");
    }
}

/// Инвентарь, курсор и сундук клиента k совпадают с серверными.
fn assert_synced(net: &LagNet, k: usize, what: &str) {
    let sv = server_of(net, k);
    let me = net.client(k);
    for i in 0..=OFFHAND {
        assert_eq!(me.ctl.inv.get(i), sv.inv.get(i), "{what}: инвентарь клиента {k}, ячейка {i}");
    }
    let (Some(cm), Some(sm)) = (&me.ctl.screen, &sv.screen) else { panic!("{what}: окно закрыто (клиент {k})") };
    assert_eq!(cm.cursor, sm.cursor, "{what}: курсор клиента {k}");
    let w = me.world.d.borrow();
    let theirs = net.sv.sim.sim.world.d.borrow();
    let (a, b) = (w.be().chest(CHEST_AT).expect("сундук у клиента"), theirs.be().chest(CHEST_AT).unwrap());
    assert_eq!(a.slots, b.slots, "{what}: сундук у клиента {k}");
}

/// Пуля: A кладёт стек в сундук — B видит его в своём окне.
#[test]
fn g8_bullet_second_client_sees_chest_change() {
    let mut net = chest_net(75, 20);
    open_both(&mut net);
    net.peer(0).client.sim.screen_click(0, 0, A_PICKUP); // A берёт 40 земли из слота 0 сундука
    net.ticks(10, |_, _| {});
    let b = net.peer(1).client.sim.world.d.borrow().be().chest(CHEST_AT).unwrap().slots[0];
    assert!(b.is_empty(), "B видит, что стек взяли: {b:?}");
    assert_eq!(net.sv.sim.sim.world.d.borrow().be().chest(CHEST_AT).unwrap().slots[0], ItemStack::EMPTY);
    assert_synced(&net, 0, "пуля");
    assert_synced(&net, 1, "пуля");
}

/// Готово-когда: 1000 случайных кликов с каждой стороны при 75 ± 20 мс — суммы по предметам не меняются ни на штуку
/// (сервер в любой момент), клиенты в конце совпадают с сервером.
fn race(delay: u64, jitter: i64, clicks: usize, seed: u64) -> (u32, u32, usize) {
    race_by(delay, jitter, clicks, seed, [true, true])
}

/// Итог: (полных копий окна, полных копий инвентаря, кликов).
fn race_by(delay: u64, jitter: i64, clicks: usize, seed: u64, active: [bool; 2]) -> (u32, u32, usize) {
    let mut net = chest_net(delay, jitter);
    open_both(&mut net);
    let before = tally(&net);
    assert!(before.len() >= 5, "в игре разные предметы: {before:?}");
    let mut rng = GameRng(seed);
    let mut sent = [0usize; 2];
    let mut guard = 0;
    while (0..2).any(|k| active[k] && sent[k] < clicks) {
        net.ticks(1, |n, _| {
            for k in 0..2 {
                for _ in 0..rng.below(3) {
                    if active[k] && sent[k] < clicks && n.peer(k).client.sim.ctl.screen.is_some() {
                        sent[k] += random_gesture(&mut rng, &mut n.peer(k).client.sim, clicks <= 2000);
                    }
                }
            }
        });
        if guard % 10 == 0 {
            assert_eq!(tally(&net), before, "сервер: суммы разошлись на тике {guard}");
        }
        guard += 1;
        // Если сервер закрыл окно (например, предмет выпал и далеко) — открыть заново.
        for k in 0..2 {
            if net.peer(k).client.sim.ctl.screen.is_none() {
                net.peer(k).press(P_USE);
            }
        }
        assert!(guard < 3000 + clicks, "не набралось кликов: {sent:?}");
    }
    net.ticks(60, |_, _| {}); // тишина: всё дошло
    let st = &net.sv.sim.stats;
    eprintln!("race {delay}/{jitter}: кликов {sent:?}, тиков {guard}, win_mismatch {}, inv_mismatch {}, вниз {} КБ", st.win_mismatch, st.inv_mismatch, net.peers_down() / 1024);
    assert_eq!(tally(&net), before, "сервер: итог");
    for k in 0..2 {
        assert_synced(&net, k, "после гонки");
    }
    (st.win_mismatch, st.inv_mismatch, sent[0] + sent[1])
}

#[test]
fn g8_chest_race_two_clients_no_loss_no_dupe() {
    let (win, inv, clicks) = race(75, 20, 1000, 1);
    // Полная копия — только при настоящем конфликте двоих; без ревизий она затирала свежие предсказания и тянула
    // за собой следующие (было 1353 копий окна и 542 инвентаря на 2000 кликов).
    assert!(win as usize * 100 < clicks * 55 && inv < 100, "полных копий: окно {win}, инвентарь {inv} на {clicks} кликов");
}

#[test]
fn g8_chest_race_other_seeds_and_lags() {
    race(0, 0, 300, 2);
    race(120, 40, 300, 3);
}

/// Один жмёт, второй смотрит: без конфликтов предсказание сходится почти всегда.
#[test]
fn g8_single_clicker_rarely_mismatches() {
    let (win, inv, _) = race_by(75, 20, 500, 5, [true, false]);
    assert_eq!((win, inv), (0, 0), "один жмёт — полных копий нет");
}

/// Крышка и число смотрящих — у всех, кто видит кусок; закрыл один — крышка стоит открытой, закрыл последний — опускается.
#[test]
fn g8_chest_lid_and_viewers_for_everyone() {
    let mut net = chest_net(75, 20);
    let viewers = |net: &LagNet, k: usize| net.client(k).world.d.borrow().be().chest(CHEST_AT).map(|c| (c.viewers, c.lid.open));
    assert_eq!((viewers(&net, 0), viewers(&net, 1)), (Some((0, false)), Some((0, false))));
    net.peer(0).press(P_USE);
    net.ticks(8, |_, _| {});
    assert_eq!((viewers(&net, 0), viewers(&net, 1)), (Some((1, true)), Some((1, true))), "первый открыл — крышка вверх у обоих");
    net.peer(1).press(P_USE);
    net.ticks(8, |_, _| {});
    assert_eq!((viewers(&net, 0), viewers(&net, 1)), (Some((2, true)), Some((2, true))), "двое смотрят");
    net.peer(0).client.sim.close_screen();
    net.ticks(8, |_, _| {});
    assert_eq!((viewers(&net, 0), viewers(&net, 1)), (Some((1, true)), Some((1, true))), "закрыл один — крышка открыта");
    net.peer(1).client.sim.close_screen();
    net.ticks(8, |_, _| {});
    assert_eq!((viewers(&net, 0), viewers(&net, 1)), (Some((0, false)), Some((0, false))), "закрыл последний — вниз");
    net.ticks(20, |_, _| {});
    for k in 0..2 {
        let p = net.client(k).world.d.borrow().be().chest(CHEST_AT).unwrap().lid.progress;
        assert_eq!(p, 0.0, "крышка у игрока {k} опустилась");
    }
}

/// Тяга стека по 9 слотам при 75 мс — ни одной полной пересылки окна (предсказание совпало с сервером).
#[test]
fn g8_drag_over_nine_slots_without_full_copy() {
    let mut net = chest_net(75, 20);
    open_both(&mut net);
    net.peer(0).client.sim.screen_click(0, 0, A_PICKUP); // 40 земли на курсор
    net.ticks(10, |_, _| {});
    let before = (net.sv.sim.stats.win_mismatch, net.sv.sim.stats.inv_mismatch);
    // ЛКМ-тяга по пустым слотам 2, 3, 4, 6…11 (9 слотов): поровну в каждый, остаток на курсоре.
    let sim = &mut net.peer(0).client.sim;
    sim.screen_click(SLOT_OUTSIDE, 0, A_QUICK_CRAFT);
    for s in [2, 3, 4, 6, 7, 8, 9, 10, 11] {
        sim.screen_click(s, 1, A_QUICK_CRAFT);
    }
    sim.screen_click(SLOT_OUTSIDE, 2, A_QUICK_CRAFT);
    net.ticks(12, |_, _| {});
    let after = (net.sv.sim.stats.win_mismatch, net.sv.sim.stats.inv_mismatch);
    assert_eq!(before, after, "ни одной полной пересылки");
    assert_synced(&net, 0, "тяга");
    assert_synced(&net, 1, "тяга");
    let b = net.client(1).world.d.borrow().be().chest(CHEST_AT).unwrap().slots;
    assert_eq!((b[2].n(), b[11].n()), (4, 4), "B видит разложенное: {b:?}");
}

/// Сундук сломан, пока у B открыто окно и в руке (на курсоре) стек: окно закрывается, стек возвращается в рюкзак,
/// содержимое сундука выпадает — ничего не потеряно.
#[test]
fn g8_chest_broken_under_open_window() {
    let mut net = chest_net(75, 20);
    // A открывает и закрывает (чтобы бить мог), B держит окно со стеком на курсоре.
    open_both(&mut net);
    net.peer(1).client.sim.screen_click(1, 0, A_PICKUP); // 64 камня на курсор B
    net.peer(0).client.sim.close_screen();
    net.ticks(10, |_, _| {});
    let mut before = tally(&net);
    before.remove(&(CHEST as u32));
    assert!(net.client(1).ctl.screen.as_ref().is_some_and(|m| !m.cursor.is_empty()), "курсор B не пуст");
    // A ломает сундук рукой (в хотбаре пусто).
    net.peer(0).press(P_HOTBAR + 8); // пустая рука
    let mut done = None;
    net.ticks(200, |n, k| {
        n.peer(0).set_keys(if done.is_none() { K_ATTACK } else { 0 });
        if k == 0 {
            n.peer(0).click();
        }
        if done.is_none() && n.peer(0).block(0, 101, 2) == AIR {
            done = Some(k);
        }
    });
    assert!(done.is_some(), "A сломал сундук");
    net.ticks(10, |_, _| {});
    assert!(net.client(1).ctl.screen.is_none(), "окно у B закрылось");
    assert!(net.sv.sim.players.iter().all(|p| p.screen.is_none()), "и у сервера");
    let mut after = tally(&net);
    after.remove(&(CHEST as u32));
    assert_eq!(after, before, "ничего не потеряно и не умножено (сундук разобран на предметы)");
    for k in 0..2 {
        assert_synced_inv(&net, k);
    }
}

/// Печь: огонёк и стрелка идут у обоих (WinData каждому), результат виден второму, когда первый его забрал.
#[test]
fn g8_furnace_flame_arrow_and_output_for_both() {
    let mut net = box_net(cell_of(FURNACE, F_NORTH as State), 75, 20);
    {
        let d = net.sv.sim.sim.world.d.clone();
        let mut w = d.borrow_mut();
        let f = w.be_mut().furnace_mut(CHEST_AT).expect("печь у сервера");
        f.set(FURNACE_IN, ItemStack::of(I_RAW_IRON, 3)); // через Container: время рецепта
        f.set(FURNACE_FUEL, ItemStack::of(I_COAL, 1));
    }
    open_both(&mut net);
    net.ticks(30, |_, _| {});
    let want = {
        let d = net.sv.sim.sim.world.d.borrow();
        let f = d.be().furnace(CHEST_AT).unwrap();
        vec![f.flame_px(), f.arrow_px()]
    };
    assert!(want[0] > 0 && want[1] > 0, "печь работает: {want:?}");
    for k in 0..2 {
        let v = net.client(k).furnace_view();
        assert!(v.len() == 2 && (v[0] - want[0]).abs() <= 1 && (v[1] - want[1]).abs() <= 1, "у игрока {k}: {v:?}, у сервера {want:?}");
        assert_eq!(state_of(net.client(k).world.get_cell(0, 101, 2)) & ST_LIT, ST_LIT, "горит — у игрока {k}");
    }
    // Плавка идёт дальше: обе стрелки движутся (число меняется между отсчётами).
    let mut moved = [false; 2];
    let mut last = [net.client(0).furnace_view(), net.client(1).furnace_view()];
    net.ticks(60, |n, _| {
        for k in 0..2 {
            let v = n.client(k).furnace_view();
            moved[k] |= v != last[k];
            last[k] = v;
        }
    });
    assert_eq!(moved, [true, true], "стрелки движутся у обоих");
    // Первый слиток готов (200 тиков на штуку): A забирает выход (Shift) — у B слот выхода пустеет.
    net.ticks(160, |_, _| {});
    let out = net.client(1).world.d.borrow().be().furnace(CHEST_AT).unwrap().slots[FURNACE_OUT];
    assert!(!out.is_empty(), "слиток виден и B: {out:?}");
    net.peer(0).client.sim.screen_click(2, 0, A_QUICK_MOVE);
    net.ticks(12, |_, _| {});
    let out = net.client(1).world.d.borrow().be().furnace(CHEST_AT).unwrap().slots[FURNACE_OUT];
    assert!(out.is_empty(), "A забрал — у B пусто: {out:?}");
    assert_synced_inv(&net, 0);
}

/// Окно машины Mekanism (виджеты): числа и «сброс» видят оба.
#[test]
fn g8_machine_window_for_both() {
    let mut net = box_net(cell_of(METALLURGIC_INFUSER, F_NORTH as State), 75, 20);
    net.sv.sim.sim.world.d.borrow_mut().be_mut().mek_mut(CHEST_AT).expect("машина у сервера").tank.amount = 600;
    open_both(&mut net);
    net.ticks(6, |_, _| {});
    for k in 0..2 {
        let tip = net.client(k).screen_tip(0);
        assert!(tip.contains("600"), "бак у игрока {k}: {tip}");
    }
    net.peer(1).client.sim.screen_button(3); // «сброс»
    net.ticks(12, |_, _| {});
    assert!(net.sv.sim.sim.world.d.borrow().be().mek(CHEST_AT).unwrap().tank.amount <= 10, "сервер: бак пуст");
    for k in 0..2 {
        let tip = net.client(k).screen_tip(0);
        assert!(!tip.contains("600"), "у игрока {k} бак пуст: {tip}");
    }
}

/// Длинный прогон из mp/plan.md MP-5 (10 000 кликов с каждой стороны): `cargo test -- --ignored g8_`.
#[test]
#[ignore]
fn g8_chest_race_10000() {
    race(75, 20, 10_000, 7);
}
