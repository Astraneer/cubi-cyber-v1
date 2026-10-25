//! Перекрёсток (G1, mp/spawn.md) — только для cargo test: место и постройка генератора, точки появления, защита на
//! сервере и предсказание отказа у клиента (сервер и два клиента через net::Server / net::Peer, как у бота).

use crate::abi::*;
use crate::gen::spawn::{site_on, DECK, PARKOUR_N, R_ZONE, ROOF};
use crate::gen::{generate, Gen, Region};
use crate::net::{ConnId, Peer, Server};
use crate::proto::Act;
use crate::registry::ids::*;
use crate::registry::items::ItemId;

#[test]
fn site_place_and_build() {
    site_on();
    let g = Gen::new(1337);
    let s = &g.site;
    // Эталон места для сида 1337: у самой (0, 0), пол на 69; крыша Маяка ниже 120.
    assert_eq!((s.x, s.z, s.y0), (0, 0, 69));
    assert!(s.y0 + ROOF + 1 <= 120);
    // Тот же сид — то же место (детерминизм поиска).
    let g2 = Gen::new(1337);
    assert_eq!((g2.site.x, g2.site.z, g2.site.y0), (s.x, s.z, s.y0));
    // Рельеф: ровно до 34, дальше — к естественному.
    for (x, z) in [(0, 0), (20, -25), (-33, 5), (0, 34)] {
        assert_eq!(g.column(x, z).h, s.y0, "({x}, {z})");
    }
    // Постройка: пол Маяка, угол — бревно, смотровая, роза ветров, вода бассейна, сады; деревьев генератора нет.
    let r = generate(&g, 0, 0);
    let at = |r: &Region, x: i32, y: i32, z: i32| r.get(x - r.x0, y, z - r.z0);
    assert_eq!(at(&r, 0, s.y0, 0), STONE);
    assert_eq!(at(&r, 3, s.y0 + 5, 3), SPRUCE_LOG);
    assert_eq!(at(&r, 0, s.y0 + DECK, 0), SPRUCE_PLANKS);
    assert_eq!(at(&r, 5, s.y0, 0), PLANKS, "мостик на восток");
    assert_eq!(at(&r, 5, s.y0, 5), WATER, "бассейн");
    let n = generate(&g, 0, -2);
    assert_eq!(at(&n, 0, s.y0, -17), IRON_BLOCK, "острие севера");
    // Ни одного бревна и руды над полом в садах, кроме деревьев площади (их стволы — в известных клетках).
    for (cx, cz) in [(-2, -2), (-2, 1), (1, 1), (1, -2), (-1, 0), (0, 1)] {
        let r = generate(&g, cx, cz);
        for lz in 1..17 {
            for lx in 1..17 {
                let (x, z) = (r.x0 + lx, r.z0 + lz);
                let d2 = x * x + z * z;
                if d2 > 31 * 31 {
                    continue;
                }
                assert_ne!(r.get(lx, s.y0, lz), COAL_ORE);
                assert_ne!(r.get(lx, s.y0, lz), IRON_ORE);
                assert!(r.get(lx, s.y0 + 1, lz) != CACTUS);
            }
        }
    }
    // Шов: клетки рамки соседних областей совпадают (площадь кладётся одинаково в каждую область).
    for (a, b) in [((0, 0), (1, 0)), ((0, 0), (0, 1)), ((-1, -1), (0, -1)), ((1, 1), (1, 0))] {
        let (ra, rb) = (generate(&g, a.0, a.1), generate(&g, b.0, b.1));
        for z in ra.z0.max(rb.z0)..(ra.z0 + 18).min(rb.z0 + 18) {
            for x in ra.x0.max(rb.x0)..(ra.x0 + 18).min(rb.x0 + 18) {
                for y in s.y0 - 2..s.y0 + ROOF + 2 {
                    assert_eq!(ra.get(x - ra.x0, y, z - ra.z0), rb.get(x - rb.x0, y, z - rb.z0), "шов {a:?}/{b:?} в ({x}, {y}, {z})");
                }
            }
        }
    }
    // Дальние плитки видят Маяк столбцом до крыши, площадь — камнем.
    assert_eq!(s.lod(0, 0, 4).map(|t| t.0), Some(s.y0 + ROOF));
    assert_eq!(s.lod(12, 5, 1).map(|t| (t.0, t.1)), Some((s.y0, L_STONE)));
    assert_eq!(s.lod(200, 0, 1), None);
    // Защита — цилиндр R_ZONE.
    assert!(s.protects(0, 0) && s.protects(R_ZONE, 0) && !s.protects(R_ZONE + 1, 0) && !s.protects(23, 23));
}

#[test]
fn parkour_is_jumpable() {
    site_on();
    let g = Gen::new(1337);
    let p = g.site.parkour();
    assert_eq!(p.len(), PARKOUR_N as usize);
    for (i, w) in p.windows(2).enumerate() {
        let ((ax, ak, az), (bx, bk, bz)) = (w[0], w[1]);
        let d = (((ax - bx).pow(2) + (az - bz).pow(2)) as f32).sqrt();
        assert!(bk == ak + 1, "подъём 1");
        assert!((1.0..=3.2).contains(&d), "площадки {i}→{}: {d}", i + 1);
    }
    for &(dx, k, dz) in p {
        assert!(dx.abs().max(dz.abs()) >= 4, "не в Маяке: ({dx}, {dz})");
        assert!(dx * dx + dz * dz <= 7 * 7 + 7, "над бассейном: ({dx}, {dz})");
        if k <= 2 {
            assert!(dx.abs() > 1 && dz.abs() > 1, "низкая площадка над мостиком: ({dx}, {dz})");
        }
    }
    // Последняя — вровень со смотровой.
    assert_eq!(p.last().unwrap().1, DECK);
}

/// Сервер и клиенты без задержки: кадры — байтами (как у бота).
struct Net {
    sv: Server,
    peers: Vec<(Peer, ConnId)>,
}

impl Net {
    fn new(nicks: &[&str]) -> Net {
        let mut sv = Server::new(1337, true);
        let peers = nicks.iter().map(|n| (Peer::new(1337, n), sv.open_conn())).collect();
        let mut net = Net { sv, peers };
        for _ in 0..200 {
            if net.peers.iter().all(|p| p.0.ready()) {
                return net;
            }
            net.tick();
        }
        panic!("не вошли");
    }
    fn tick(&mut self) {
        for (p, c) in &mut self.peers {
            if p.ready() {
                p.tick();
            }
            for b in p.take() {
                self.sv.frame(*c, &b);
            }
        }
        self.sv.tick();
        let (out, _) = self.sv.take();
        for (c, b) in out {
            if let Some((p, _)) = self.peers.iter_mut().find(|p| p.1 == c) {
                p.recv(&b).unwrap();
            }
        }
    }
    fn ticks(&mut self, n: usize, mut f: impl FnMut(&mut Net, usize)) {
        for k in 0..n {
            f(self, k);
            self.tick();
        }
    }
    fn peer(&mut self, k: usize) -> &mut Peer {
        &mut self.peers[k].0
    }
}

#[test]
fn spawn_on_crossroads_and_protection() {
    site_on();
    let mut net = Net::new(&["A", "B"]);
    net.ticks(2, |_, _| {});
    let y0 = Gen::new(1337).site.y0;
    // Оба — на юго-западной части площади, не друг в друге; взгляд на север-северо-запад: Маяк справа, витрина слева.
    let (a, b) = (net.peer(0).pos(), net.peer(1).pos());
    for p in [a, b] {
        let r = (p[0] * p[0] + p[2] * p[2]).sqrt();
        assert!((9.5..=16.0).contains(&r) && p[2] > 0.0 && p[0] < 0.0 && p[1] == (y0 + 1) as f64, "точка появления {p:?}");
    }
    assert!((a[0] - b[0]).hypot(a[2] - b[2]) >= 1.5, "не друг в друге: {a:?} {b:?}");
    let yaw = net.peer(0).client.sim.ctl.player.yaw as f64;
    assert!((150.0..=200.0).contains(&yaw), "взгляд на север-северо-запад: {yaw}");
    // Маяк (0, 0) и середина галереи витрины (−19, −3) — в поле зрения: не дальше 55° от взгляда.
    let (lx, lz) = (-yaw.to_radians().sin(), yaw.to_radians().cos());
    for (tx, tz) in [(0.5, 0.5), (-18.5, -2.5)] {
        let (dx, dz) = (tx - a[0], tz - a[2]);
        let c = (lx * dx + lz * dz) / dx.hypot(dz);
        assert!(c > 55f64.to_radians().cos(), "({tx}, {tz}) в кадре: cos {c:.2}");
    }
    assert_eq!(net.peer(0).client.sim.view.overlay_msg, MSG_CROSSROADS_ENTER, "строка «Перекрёсток…» при появлении");

    // A бьёт пол площади под собой: трещин нет, блок на месте, над хотбаром — сообщение; серверу ломание не шлётся.
    net.peer(0).set_look(180.0, 90.0);
    net.ticks(2, |_, _| {});
    let floor = net.peer(0).target().expect("пол под ногами");
    assert_eq!(floor[1], y0);
    let before = net.peer(0).block(floor[0], floor[1], floor[2]);
    net.ticks(60, |n, k| {
        if k == 0 {
            n.peer(0).click();
        }
        n.peer(0).set_keys(K_ATTACK);
    });
    net.peer(0).set_keys(0);
    net.ticks(3, |_, _| {});
    assert_eq!(net.peer(0).block(floor[0], floor[1], floor[2]), before);
    assert_eq!(net.peer(1).block(floor[0], floor[1], floor[2]), before);
    assert_eq!(net.peer(0).client.sim.ctl.mine.stage, -1, "трещин нет");
    assert_eq!(net.peer(0).client.sim.view.overlay_msg, MSG_CROSSROADS);
    assert_eq!(net.sv.status().rejected, 0, "клиент предсказал отказ — серверу нечего отклонять");

    // Ставление: блок не появляется ни на миг, стек цел.
    net.peer(0).give(DIRT as ItemId, 3);
    net.ticks(3, |_, _| {});
    let up = [floor[0], floor[1] + 1, floor[2]];
    let mut seen = false;
    net.ticks(8, |n, k| {
        if k == 0 {
            n.peer(0).press(P_USE);
        }
        seen |= n.peer(0).block(up[0], up[1], up[2]) == DIRT;
    });
    assert!(!seen, "блок не мигнул");
    assert_eq!(net.peer(0).count(DIRT as ItemId), 3);
    assert_eq!(net.peer(0).stats()[0], 0, "без откатов");
    // Q: вещь остаётся у игрока.
    net.ticks(4, |n, k| {
        if k == 0 {
            n.peer(0).press(P_DROP);
        }
    });
    assert_eq!((net.peer(0).count(DIRT as ItemId), net.peer(0).items()), (3, 0));

    // Изменённый клиент шлёт ломание сам (бот-«читер»): сервер отказывает, блок на месте у всех.
    let seq = net.peer(0).client.sim.seq + 1;
    net.peer(0).client.sim.seq = seq;
    net.peer(0).client.sim.act(Act::BreakStart { pos: floor, face: 1, seq });
    net.peer(0).client.sim.act(Act::BreakDone { pos: floor, seq });
    net.ticks(4, |_, _| {});
    assert!(net.sv.status().rejected >= 2, "отказ сервера");
    assert_eq!(net.sv.sim.sim.world.d.borrow_mut().get_block(floor[0], floor[1], floor[2]), before);
    assert_eq!(net.peer(1).block(floor[0], floor[1], floor[2]), before);

    // За границей зоны ломается как обычно.
    net.peer(0).spawn(0.5, 45.5);
    net.ticks(12, |_, _| {});
    assert_eq!(net.peer(0).client.sim.view.overlay_msg, MSG_CROSSROADS_LEAVE, "«Вы покидаете Перекрёсток»");
    net.peer(0).set_look(180.0, 90.0);
    net.ticks(2, |_, _| {});
    let out = net.peer(0).target().expect("земля за зоной");
    assert!(out[0] * out[0] + out[2] * out[2] > R_ZONE * R_ZONE);
    let mut broke = false;
    net.ticks(120, |n, k| {
        if k == 0 {
            n.peer(0).click();
        }
        n.peer(0).set_keys(K_ATTACK);
        broke |= n.sv.sim.sim.world.d.borrow_mut().get_block(out[0], out[1], out[2]) == AIR;
    });
    assert!(broke, "за кольцом блок ломается");
}

/// Врата странствий: постоял в проёме 3 с — перенос на сушу за 400–2000 блоков, лицом к Маяку; сразу второй раз — нет
/// (перезарядка).
#[test]
fn wandering_gate() {
    site_on();
    let mut net = Net::new(&["A"]);
    net.ticks(2, |_, _| {});
    let g = Gen::new(1337);
    let b = g.site.gate_box();
    let inside = [b[0] as f64 + 2.5, b[1] as f64, b[2] as f64 + 0.5];
    net.peer(0).teleport(inside);
    net.ticks(3, |_, _| {});
    assert_eq!(net.peer(0).client.sim.view.overlay_msg, MSG_WANDER, "строка о вратах");
    net.ticks(50, |_, _| {});
    assert!(net.peer(0).pos()[2] < b[5] as f64, "ещё в проёме: 3 с не прошли");
    net.ticks(20, |_, _| {});
    let p = net.peer(0).pos();
    let r = (p[0] * p[0] + p[2] * p[2]).sqrt();
    assert!((360.0..=2050.0).contains(&r), "перенос за 400–2000: {p:?}, r {r}");
    assert!(p[1] > crate::cell::SEA as f64 + 1.0, "на суше: {p:?}");
    // Взгляд — на площадь (look = (−sin yaw, cos yaw)).
    let yaw = net.peer(0).client.sim.ctl.player.yaw.to_radians() as f64;
    let (lx, lz) = (-yaw.sin(), yaw.cos());
    assert!((lx * -p[0] + lz * -p[2]) / r > 0.99, "лицом к Маяку: yaw {}", yaw.to_degrees());
    assert_eq!(net.sv.status().teleports, 0, "без поправок движения");
    // Сразу обратно в проём — перезарядка 60 с: не переносит.
    net.peer(0).teleport(inside);
    net.ticks(80, |_, _| {});
    assert!((net.peer(0).pos()[0] - inside[0]).abs() < 1.0, "перезарядка");
}

/// Кнопки неба (G15): ПКМ по кнопке ночи меняет небо только нажавшему и только пока он в зоне; мир, другой игрок и
/// сервер этого не видят; кнопка дня возвращает день; вышел из зоны — небо мира. Доски стенда и витрины — в JSON.
#[test]
fn sky_buttons_change_only_own_sky() {
    use crate::gen::spawn::{boards_json, recipe_cells, SKY_DAY, SKY_NIGHT, SKY_X, SKY_Z};
    site_on();
    let mut net = Net::new(&["A", "B"]);
    net.ticks(2, |_, _| {});
    let s = Gen::new(1337).site;
    let (bx, by, bz) = (s.x + SKY_X, s.y0 + 2, s.z + SKY_Z);
    assert_eq!(net.peer(0).block(bx, by, bz), WHITE_WOOL, "кнопка дня");
    assert_eq!(net.peer(0).block(bx + 2, by, bz), BLACK_WOOL, "кнопка ночи");
    let sky = |net: &mut Net, k: usize| net.peer(k).client.sim.time_of_day(0.0);
    let world = |net: &mut Net| net.sv.sim.sim.day;
    let near = |a: f64, b: f64| ((a - b + 0.5).rem_euclid(1.0) - 0.5).abs() < 0.01;
    // A — перед кнопкой ночи, лицом на север; B — рядом.
    net.peer(0).teleport([(bx + 2) as f64 + 0.5, (s.y0 + 1) as f64, (bz + 2) as f64 + 0.5]);
    net.peer(1).teleport([(bx + 4) as f64 + 0.5, (s.y0 + 1) as f64, (bz + 4) as f64 + 0.5]);
    net.peer(0).set_look(180.0, 5.0);
    net.ticks(3, |_, _| {});
    assert_eq!(net.peer(0).target(), Some([bx + 2, by, bz]), "прицел на кнопке ночи");
    let base = sky(&mut net, 1);
    assert!(near(sky(&mut net, 0), base), "до нажатия небо одно");
    assert!(!near(base, SKY_NIGHT), "в мире не полночь: {base}");
    let edits = net.sv.status().rejected;
    net.ticks(1, |n, _| n.peer(0).press(P_USE));
    assert_eq!(net.peer(0).client.sim.view.overlay_msg, MSG_SKY_NIGHT);
    // Переход плавный: через два тика небо сдвинулось, но не скачком (на alpha 0 — прошлый тик).
    net.ticks(2, |_, _| {});
    let a1 = sky(&mut net, 0);
    assert!(!near(a1, SKY_NIGHT) && !near(a1, base), "плавно: {a1}");
    net.ticks(50, |_, _| {});
    assert!(near(sky(&mut net, 0), SKY_NIGHT), "у A ночь: {}", sky(&mut net, 0));
    let b = sky(&mut net, 1);
    assert!(near(b, crate::game::sim::time_of_day(world(&mut net), true, 0.0)), "у B время мира: {b}");
    assert!(!near(b, SKY_NIGHT), "у B не ночь");
    // Мир не тронут: кнопки на месте у всех, отказов и откатов нет.
    assert_eq!(net.peer(1).block(bx + 2, by, bz), BLACK_WOOL);
    assert_eq!(net.sv.sim.sim.world.d.borrow_mut().get_block(bx + 2, by, bz), BLACK_WOOL);
    assert_eq!(net.sv.status().rejected, edits);
    assert_eq!(net.peer(0).stats()[0], 0, "без откатов");
    // Кнопка дня.
    net.peer(0).teleport([bx as f64 + 0.5, (s.y0 + 1) as f64, (bz + 2) as f64 + 0.5]);
    net.ticks(3, |_, _| {});
    assert_eq!(net.peer(0).target(), Some([bx, by, bz]), "прицел на кнопке дня");
    net.ticks(1, |n, _| n.peer(0).press(P_USE));
    net.ticks(50, |_, _| {});
    assert!(near(sky(&mut net, 0), SKY_DAY), "у A день: {}", sky(&mut net, 0));
    // Вышел из зоны — небо мира (плавно).
    net.peer(0).spawn(0.5, 45.5);
    net.ticks(60, |_, _| {});
    let w = crate::game::sim::time_of_day(world(&mut net), true, 0.0);
    assert!(near(sky(&mut net, 0), w) && near(sky(&mut net, 1), w), "за зоной — время мира");
    assert_eq!(net.peer(0).client.sim.view.sky_pick, None);

    // Доски: витрина и стенд в JSON, у каждой доски крафта — рецепты из книги (сетка не пустая).
    let j = boards_json(&s);
    assert!(j.starts_with("[{") && j.ends_with("}]"));
    assert_eq!(j.matches("\"kind\":\"show\"").count(), 3);
    assert_eq!(j.matches("\"kind\":\"craft\"").count(), 3);
    assert_eq!(j.matches("\"kind\":\"sky\"").count(), 2);
    assert_eq!(j.matches("\"grid\"").count(), 12, "по 4 рецепта на доску крафта");
    for name in ["wind_generator", "basic_universal_cable", "steel_casing", "crusher"] {
        let (g, out, n) = recipe_cells(name).unwrap();
        assert!(g.iter().filter(|&&i| i != 0).count() >= 3 && out != 0 && n >= 1, "{name}");
    }
}
