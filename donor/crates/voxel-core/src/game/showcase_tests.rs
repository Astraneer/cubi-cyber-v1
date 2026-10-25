//! Витрина Mekanism и ночь на Перекрёстке (G13) — только для cargo test: машины работают на сервере без игроков, вход
//! бесконечный, вещей игрокам не дают (окно только смотреть), враждебные мобы в зоне не появляются.

use super::*;
use crate::abi::{ScreenKind, A_PICKUP, A_QUICK_MOVE};
use crate::cell::block_of;
use crate::game::interact::Hit;
use crate::game::showcase::machines;
use crate::gen::spawn::{site_on, Hall, R_ZONE};
use crate::net::Server;
use crate::registry::items::{I_IRON_INGOT, I_RAW_IRON, I_STEEL_INGOT};
use crate::world::blockentity::BlockEntity;
use crate::world::mek::CH_IN;

fn chest_items(w: &WorldData, p: [i32; 3], item: ItemId) -> i32 {
    match w.be().get(p) {
        Some(BlockEntity::Chest(c)) => c.slots.iter().filter(|s| s.item == item).map(|s| s.n()).sum(),
        _ => -1,
    }
}

#[test]
fn showcase_runs_alone_and_gives_nothing_away() {
    // Сервер без игроков: три линии сами доводят сырьё до сундуков (ветер — железо, сталь — сталь, удвоение — железо);
    // предметов на земле нет; вход не кончается.
    site_on();
    let mut sv = Server::new(1337, false);
    let list = machines(&sv.sim.sim.world.d.borrow());
    assert_eq!(list.len(), 4 + 5 + 7);
    {
        let mut w = sv.sim.sim.world.d.borrow_mut();
        for &(_, b, p) in &list {
            assert_eq!(w.get_block(p[0], p[1], p[2]), b, "{b} в {p:?}");
        }
    }
    let chests: Vec<(Hall, [i32; 3], ItemId)> = list
        .iter()
        .filter(|m| m.1 == CHEST)
        .map(|&(h, _, p)| (h, p, if h == Hall::Steel { I_STEEL_INGOT } else { I_IRON_INGOT }))
        .collect();
    let mut first = [0usize; 3];
    for t in 1..=3600 {
        sv.tick();
        let w = sv.sim.sim.world.d.borrow();
        for (k, &(_, p, item)) in chests.iter().enumerate() {
            if first[k] == 0 && chest_items(&w, p, item) > 0 {
                first[k] = t;
            }
        }
        if first.iter().all(|&f| f > 0) && t > 1200 {
            break;
        }
    }
    eprintln!("витрина: первые слитки на тиках {:?}", chests.iter().zip(first).map(|(c, t)| (c.0, t)).collect::<Vec<_>>());
    assert!(first.iter().all(|&f| f > 0), "слитки в сундуках всех линий (тик первого): {first:?}");
    assert_eq!(sv.sim.item_count(), 0, "ничего не выпало на землю");
    let w = sv.sim.sim.world.d.borrow();
    let ch = list.iter().find(|m| m.0 == Hall::Wind && m.1 == ENRICHMENT_CHAMBER).unwrap().2;
    assert_eq!(w.be().mek(ch).unwrap().slots[CH_IN].item, I_RAW_IRON, "вход пополняется");
}

#[test]
fn showcase_window_is_view_only() {
    // Игрок у камеры витрины: окно открывается, но клик и Shift-клик по входу ничего не двигают ни у клиента, ни на
    // сервере; конфигуратор в руке не меняет машину.
    site_on();
    let w = World::new(1337);
    let mut g = Game::new(&w);
    let ch = machines(&g.sim().world.d.borrow()).into_iter().find(|m| m.0 == Hall::Wind && m.1 == ENRICHMENT_CHAMBER).unwrap().2;
    let p = crate::gen::spawn::PAVILIONS.iter().find(|p| p.hall == Hall::Wind).unwrap();
    let (fx, fz) = crate::gen::spawn::face_vec(p.face);
    let stand = [ch[0] as f64 + 0.5 + 2.0 * fx as f64, ch[1] as f64, ch[2] as f64 + 0.5 + 2.0 * fz as f64];
    g.teleport(stand[0], stand[1], stand[2]);
    for _ in 0..3 {
        g.tick();
    }
    assert_eq!(block_of(w.get_cell(ch[0], ch[1], ch[2])), ENRICHMENT_CHAMBER, "машина у клиента");
    for (_, b, p) in machines(&g.sim().world.d.borrow()) {
        assert_eq!(block_of(w.get_cell(p[0], p[1], p[2])), b, "у клиента {b} в {p:?}");
    }
    let point = [ch[0] as f64 + 0.5 + 0.5 * fx as f64, ch[1] as f64 + 0.5, ch[2] as f64 + 0.5 + 0.5 * fz as f64];
    let face = p.face;
    g.interact(0, Hit { pos: ch, face, point, inside: false });
    g.tick();
    g.tick();
    assert_eq!(g.screen_kind(), ScreenKind::Machine as i32, "окно открылось");
    let input = |g: &Game| g.sim().world.d.borrow().be().mek(ch).unwrap().slots[CH_IN];
    let before = input(&g);
    assert!(!before.is_empty(), "во входе сырьё");
    g.screen_click(CH_IN as i32, 0, A_PICKUP);
    g.tick();
    g.screen_click(CH_IN as i32, 0, A_QUICK_MOVE);
    for _ in 0..3 {
        g.tick();
    }
    assert!(g.me().inv.main.iter().all(|s| s.is_empty()), "в инвентарь ничего не пришло");
    assert!(g.client.sim.ctl.screen.as_ref().is_some_and(|m| m.cursor.is_empty()), "курсор пуст");
    assert!(input(&g).n() >= before.n() - 1, "вход не взяли (машина могла съесть одну операцию)");
    assert_eq!(g.server.sim.stats.win_mismatch, 0, "предсказание совпало с сервером");
}

#[test]
fn no_hostile_spawn_in_zone_at_night() {
    // Ночь, игрок на площади: зомби появляются вокруг (в 24…128 блоках от игрока), но ни один — в зоне r ≤ 32.
    site_on();
    let w = World::new(1337);
    let mut g = Game::new(&w);
    g.spawn(0.5, 13.5);
    g.server.sim.sim.mobs.herds = false;
    g.set_time_of_day(0.0);
    g.set_day_cycle(false);
    let (sx, sz) = (g.sim().world.d.borrow().gen().site.x as f64 + 0.5, g.sim().world.d.borrow().gen().site.z as f64 + 0.5);
    let mut seen = std::collections::HashSet::new();
    let mut spawned = 0;
    for _ in 0..1200 {
        g.tick();
        for m in g.sim().mobs.list.iter().filter(|m| m.def().hostile) {
            if seen.insert(m.id) {
                spawned += 1;
                let r = (m.pos[0] - sx).hypot(m.pos[2] - sz);
                assert!(r > R_ZONE as f64 + 0.5, "враждебный моб появился в зоне: r {r:.1}");
            }
        }
    }
    assert!(spawned > 0, "ночью вокруг зомби всё же появляются (проверка не пустая)");
}


#[test]
fn raise_restores_zone_over_saved_world() {
    // Старт сервера поверх сохранённого мира: в зоне лишняя правка (земля на полу) и снятая машина — зона снова как у
    // генератора, машина на месте, ничего не рассыпалось; правка за зоной цела.
    site_on();
    let mut sv = Server::new(1337, false);
    let (y0, ch) = {
        let w = sv.sim.sim.world.d.borrow();
        (w.gen().site.y0, machines(&w).into_iter().find(|m| m.0 == Hall::Wind && m.1 == ENRICHMENT_CHAMBER).unwrap().2)
    };
    let floor_before = sv.sim.sim.world.d.borrow_mut().get_cell(0, y0, 12);
    {
        let mut w = sv.sim.sim.world.d.borrow_mut();
        w.set_cell(0, y0 + 1, 12, DIRT as crate::cell::Cell);
        w.set_cell(ch[0], ch[1], ch[2], AIR as crate::cell::Cell);
        w.set_cell(40, 90, 0, DIRT as crate::cell::Cell);
        drop(w.take_removed());
    }
    sv.sim.raise_crossroads();
    let mut w = sv.sim.sim.world.d.borrow_mut();
    assert_eq!(w.get_block(0, y0 + 1, 12), AIR, "лишняя правка в зоне снята");
    assert_eq!(w.get_cell(0, y0, 12), floor_before);
    assert_eq!(w.get_block(ch[0], ch[1], ch[2]), ENRICHMENT_CHAMBER, "машина снова на месте");
    assert_eq!(w.get_block(40, 90, 0), DIRT, "за зоной правка цела");
    drop(w);
    assert_eq!(sv.sim.item_count(), 0);
}
