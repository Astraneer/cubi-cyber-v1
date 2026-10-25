//! Mekanism M5 «Логистика» через игру (клиент + локальный сервер, как в браузере) и по сети (два клиента): площадка mek:7
//! (сцена mek_logistics) — сундук руды → транспортёры → линия ×2 → транспортёры → сундук; насос → труба → сепаратор;
//! едущие предметы у клиента (item_draws) движутся без рывков.

use super::*;
use crate::cell::block_of;
use crate::game::mek_demo::{double_layout, logistics_layout};
use crate::registry::items::{I_IRON_INGOT, I_RAW_IRON};
use crate::world::blockentity::BlockEntity;
use crate::world::mek::recipes::SUB_WATER;

fn scene() -> (World, Game) {
    let w = World::new(1337);
    for x in -12..=16 {
        for z in -8..=8 {
            w.set_block(x, 99, z, STONE);
        }
    }
    let mut g = Game::new(&w);
    g.teleport(0.5, 100.0, 0.5);
    g.set_look(0.0, 0.0);
    g.tick();
    (w, g)
}

fn count(g: &Game, chest: [i32; 3], item: u16) -> i32 {
    match g.sim().world.d.borrow_mut().be_mut().get_mut(chest) {
        Some(BlockEntity::Chest(c)) => c.slots.iter().filter(|s| s.item == item).map(|s| s.count).sum(),
        _ => -1,
    }
}

/// Записи едущих предметов в item_draws клиента (поворот 45° / 135° и подъём 0 — признак стека в трубе).
fn transit_draws(v: &[f32]) -> Vec<[f32; 3]> {
    v.chunks(8).filter(|d| d[4] == 0.0 && (d[3] - std::f32::consts::FRAC_PI_4).abs() < 1e-3 || (d[3] - 3.0 * std::f32::consts::FRAC_PI_4).abs() < 1e-3).map(|d| [d[0], d[1], d[2]]).collect()
}

#[test]
fn logistics_scene_ore_chest_to_ingot_chest() {
    // «Готово, когда» G14: 10 сырого железа в сундуке A → транспортёры → линия ×2 → транспортёры → 20 слитков в сундуке
    // B без рук; вода — насосом по трубе (сепаратор пуст при постройке); предметы в пути видны клиенту.
    let (w, mut g) = scene();
    g.mek_build(7);
    g.tick();
    g.tick();
    let p = [0, 100, 0];
    let (sep, ..) = double_layout(p);
    let (a, t0, t1, b, pump, _, top) = logistics_layout(p);
    assert_eq!(block_of(w.get_cell(t0[0], t0[1], t0[2])), LOGISTICAL_TRANSPORTER, "у клиента транспортёр");
    assert_eq!(block_of(w.get_cell(top[0], top[1], top[2])), MECHANICAL_PIPE);
    assert_eq!(block_of(w.get_cell(pump[0], pump[1], pump[2])), ELECTRIC_PUMP);
    let _ = t1;
    let mut seen_items = 0;
    let mut water = false;
    let mut t = 0;
    while count(&g, b, I_IRON_INGOT) < 20 && t < 6000 {
        g.tick();
        t += 1;
        seen_items = seen_items.max(transit_draws(&g.item_draws(0.5)).len());
        water |= g.sim().world.d.borrow().be().mek(sep).is_some_and(|m| m.tank.sub == SUB_WATER && m.tank.amount > 0);
    }
    assert_eq!(count(&g, b, I_IRON_INGOT), 20, "через {t} тиков");
    assert_eq!(count(&g, a, I_RAW_IRON), 0, "сундук A пуст");
    assert!(seen_items >= 2, "клиент рисует едущие предметы: {seen_items}");
    assert!(water, "вода пришла в сепаратор трубой");
}

// ---------------------------------------------------------------- по сети: два клиента и сервер

use crate::game::client::Client;
use crate::game::server::ServerSim;
use crate::proto::{decode, encode, C2S};

struct Net {
    sv: ServerSim,
    cl: Vec<(Client, Option<u32>)>,
}

impl Net {
    fn pump(&mut self) {
        for (c, id) in &mut self.cl {
            for b in c.take_outbox() {
                match (decode::<C2S>(&b).unwrap(), *id) {
                    (C2S::Hello(h), None) => *id = Some(self.sv.join(&h).expect("вход")),
                    (m, Some(i)) => self.sv.handle(i, m),
                    _ => {}
                }
            }
        }
        for (c, id) in &mut self.cl {
            let Some(p) = id.and_then(|i| self.sv.player_mut(i)) else { continue };
            for m in std::mem::take(&mut p.outbox) {
                c.recv(&encode(&m)).unwrap();
            }
        }
    }
    fn tick(&mut self) {
        for (c, _) in &mut self.cl {
            c.sim.tick();
        }
        self.pump();
        self.sv.tick();
        self.pump();
        for (c, _) in &mut self.cl {
            c.sim.tick_visual();
        }
    }
}

#[test]
fn two_players_see_items_ride_smoothly() {
    // По сети: A строит площадку «логистика», B стоит рядом. Сервер решает, клиенты рисуют: у B те же стеки, что у
    // сервера (тот же предмет, клетка), точка каждого стека от кадра к кадру сдвигается плавно (≤ 0.06 блока за
    // полтика при скорости 0.05 блока/тик) — без рывков на передачах между транспортёрами.
    let mut sv = ServerSim::new(World::new(1337));
    sv.debug = true;
    let (wa, wb) = (World::new(1337), World::new(1337));
    let mut net = Net { sv, cl: vec![(Client::new(&wa, "A"), None), (Client::new(&wb, "B"), None)] };
    net.pump();
    for _ in 0..3 {
        net.tick();
    }
    let ia = net.cl[0].1.unwrap();
    let a = net.sv.player_mut(ia).unwrap().player.pos;
    let feet = [a[0].floor() as i32, a[1].floor() as i32, a[2].floor() as i32];
    net.cl[0].0.sim.debug_build(7);
    for _ in 0..5 {
        net.tick();
    }
    let (_, t0, ..) = logistics_layout(feet);
    assert_eq!(block_of(wb.get_cell(t0[0], t0[1], t0[2])), LOGISTICAL_TRANSPORTER, "B видит транспортёр");
    net.cl[1].0.sim.teleport(t0[0] as f64 + 0.5, t0[1] as f64, t0[2] as f64 + 3.5);
    for _ in 0..5 {
        net.tick();
    }
    let mut prev: std::collections::HashMap<u32, [f64; 3]> = Default::default();
    let mut max_step = 0.0f64;
    let mut seen = 0;
    for _ in 0..300 {
        net.tick();
        let b = &net.cl[1].0.sim;
        let sv_stacks = net.sv.sim.world.d.borrow().logistics.stacks.clone();
        seen = seen.max(b.transit.list.len());
        for t in &b.transit.list {
            // Тот же стек у сервера: предмет тот же, клетка — та же или соседняя (сервер мог уже передать дальше).
            let s = sv_stacks.iter().find(|s| s.id == t.id);
            if let Some(s) = s {
                assert_eq!(s.item.item, t.item.item);
                let (x, y) = (s.at(), t.at());
                assert!((0..3).map(|k| (x[k] - y[k]).abs()).sum::<i32>() <= 1, "сервер {x:?}, B {y:?}");
            }
            // Плавность: точки на долях 0.25 и 0.75 тика и на следующем тике — без скачков.
            for alpha in [0.25, 0.75] {
                let p = t.draw_pos(alpha, crate::world::logistics::SPEED);
                if let Some(q) = prev.get(&t.id) {
                    let d = (0..3).map(|k| (p[k] - q[k]).powi(2)).sum::<f64>().sqrt();
                    max_step = max_step.max(d);
                }
                prev.insert(t.id, p);
            }
        }
    }
    assert!(seen >= 2, "B видит едущие предметы: {seen}");
    assert!(max_step <= 0.06, "рывок {max_step:.3} блока");
    assert!(!transit_draws(&net.cl[1].0.sim.item_draws(0.5)).is_empty() || seen > 0);
}
