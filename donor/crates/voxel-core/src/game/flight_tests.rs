//! Полёт выживания (G16): F включает полёт с физикой Minecraft (PlayerEntity.travel при abilities.flying) — по сети с
//! задержкой сервер принимает каждый ход без поправок и телепортов, отладки сервера не нужно; сел на землю — полёт
//! выключен; выключил в воздухе — урон от падения считается с высоты выключения; возрождение — без полёта.

use super::net_tests::{put_on, LagNet};
use super::player::{Keys, Player};
use crate::abi::{K_FWD, K_JUMP, K_LEFT, K_SNEAK, K_SPRINT, P_FLIGHT};
use crate::cell::Cell;
use crate::game::physics::Solids;
use crate::registry::ids::STONE;
use crate::registry::shapes::Aabb;

/// Пол — плита под y = 0, больше ничего.
struct Floor;
impl Solids for Floor {
    fn shapes_at(&mut self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
        if y == -1 {
            out.push(Aabb { lo: [x as f64, -1.0, z as f64], hi: [x as f64 + 1.0, 0.0, z as f64 + 1.0] });
        }
    }
    fn slip(&mut self, _: i32, _: i32, _: i32) -> f32 {
        0.6
    }
    fn chokes(&mut self, _: i32, _: i32, _: i32) -> bool {
        false
    }
}

fn keys(fwd: bool, jump: bool, sneak: bool, sprint: bool) -> Keys {
    Keys { fwd, jump, sneak, sprint, ..Keys::default() }
}

/// Числа полёта Minecraft 1.21.1: вперёд ~10.9 б/с, с бегом ~21.8, вверх 7.5 б/с; F на земле — висит, не падает;
/// Shift до земли — сел, полёт выключен, урона нет.
#[test]
fn flight_numbers_like_minecraft() {
    let mut p = Player::new([0.5, 0.0, 0.5]);
    p.tick(&mut Floor, Keys::default(), false);
    assert!(p.on_ground);
    p.set_flight(true);
    for _ in 0..5 {
        p.tick(&mut Floor, Keys::default(), false);
    }
    assert!(p.flying && !p.on_ground, "F на земле — висит");
    assert_eq!(p.pos[1], 0.0, "и не падает");
    // Вверх: (v + 0.15) × 0.6 → 0.225 после хода, ход 0.375 б/т.
    for _ in 0..40 {
        p.tick(&mut Floor, keys(false, true, false, false), false);
    }
    let dy = p.pos[1] - p.prev[1];
    assert!((dy - 0.375).abs() < 1e-6, "подъём {dy}");
    // Вперёд: ход (v + 0.049) → v·0.91: 0.049 / 0.09 = 0.5444 б/т.
    for _ in 0..80 {
        p.tick(&mut Floor, keys(true, false, false, false), false);
    }
    let h = |p: &Player| ((p.pos[0] - p.prev[0]).powi(2) + (p.pos[2] - p.prev[2]).powi(2)).sqrt();
    assert!((h(&p) - 0.5444).abs() < 1e-3, "вперёд {}", h(&p));
    assert!((p.pos[1] - p.prev[1]).abs() < 1e-9, "высота держится без гравитации");
    // Бег: ускорение 0.1 — вдвое.
    for _ in 0..80 {
        p.tick(&mut Floor, keys(true, false, false, true), false);
    }
    assert!(p.sprinting && (h(&p) - 1.0889).abs() < 2e-3, "бег {}", h(&p));
    // Вниз до земли: сел — полёт выключен, приземление без падения.
    let mut n = 0;
    while p.flying && n < 200 {
        p.tick(&mut Floor, keys(false, false, true, false), false);
        n += 1;
    }
    assert!(!p.flying && p.on_ground && p.pos[1] == 0.0, "сел: {:?}", p.pos);
    assert!(p.landed.is_none_or(|(_, f)| f == 0.0), "урона нет: {:?}", p.landed);
}

fn idx(net: &mut LagNet, k: usize) -> usize {
    let id = net.peer(k).id();
    net.sv.sim.idx(id).expect("игрок на сервере")
}

/// По сети 75 ± 20 мс, сервер без отладки: взлёт, полёт с бегом, боком, в стену, спуск и посадка — ни одной поправки и
/// ни одного телепорта; полёт у сервера тот же, что у клиента, креатива нет.
#[test]
fn flight_over_lag_without_rollbacks() {
    let mut net = LagNet::new(&["A"], 75, 20);
    // Стена поперёк площадки на z = 12, x −8…7, высотой 6 — в неё упирается полёт.
    for x in -8..8 {
        for y in 101..107 {
            net.sv.sim.world_edit([x, y, 12], STONE as Cell);
        }
    }
    net.sv.sim.sim.world.d.borrow_mut().take_log();
    net.until_ready();
    put_on(&mut net, 0, 0.5, -6.5);
    net.sv.sim.debug = false; // полёт выживания — не отладка
    let i = idx(&mut net, 0);
    net.peer(0).set_look(0.0, 0.0);
    net.peer(0).press(P_FLIGHT);
    net.ticks(1, |_, _| {});
    assert!(net.peer(0).client.sim.ctl.player.flying, "клиент летит сразу");
    let mut top = 0.0f64;
    net.ticks(120, |n, k| {
        let keys = match k {
            0..=14 => K_JUMP,               // вверх ~5.6 блока
            15..=34 => K_FWD,               // вперёд к стене (z 12): упор
            35..=49 => K_FWD | K_SPRINT,    // бег в стену
            50..=64 => K_LEFT,              // вбок вдоль стены
            65..=79 => K_JUMP | K_FWD,      // над стеной
            _ => K_SNEAK,                   // вниз до земли
        };
        n.peer(0).set_look(if k < 50 { 0.0 } else { 180.0 }, 0.0);
        n.peer(0).set_keys(keys);
        top = top.max(n.peer(0).pos()[1]);
    });
    net.peer(0).set_keys(0);
    net.ticks(6, |_, _| {});
    let st = net.sv.status();
    assert_eq!((st.corrections, st.teleports), (0, 0), "сервер принял каждый ход полёта");
    assert_eq!(net.peer(0).stats()[1], 0, "клиент телепортов не получал");
    let s = net.sv.sim.players[i].player.pos;
    let (fl, gr, nc) = { let p = &net.sv.sim.players[i].player; (p.flying, p.on_ground, p.noclip) };
    assert!(top > 106.0, "летал выше стены: {top}");
    assert!(!fl && gr && !nc, "сел — полёт выключен и у сервера");
    assert!(!net.peer(0).client.sim.ctl.player.flying, "и у клиента");
    assert_eq!(net.sv.sim.players[i].hp.hp, 20.0, "посадка из полёта без урона");
    let c = net.peer(0).pos();
    assert!((s[0] - c[0]).abs() < 1e-9 && (s[2] - c[2]).abs() < 1e-9, "там же: {s:?} {c:?}");
}

/// Выключил полёт на высоте ~10 над полом: урон — как от обычного падения с этой высоты (ceil(fd − 3), спуск тика
/// приземления не в счёт — тело с той же высоты без скорости над тем же полом); пока летал и висел —
/// урона нет. Умер в полёте — возродился без полёта, без расхождений.
#[test]
fn fall_damage_after_flight_off() {
    let mut net = LagNet::new(&["A"], 75, 20);
    net.until_ready();
    put_on(&mut net, 0, 0.5, 0.5);
    net.sv.sim.debug = false;
    let i = idx(&mut net, 0);
    net.sv.sim.players[i].hp.spawn_protect = 0;
    net.peer(0).press(P_FLIGHT);
    net.ticks(60, |n, _| {
        let high = n.peer(0).pos()[1] >= 110.6;
        n.peer(0).set_keys(if high { 0 } else { K_JUMP });
    });
    let y_off = net.peer(0).pos()[1];
    assert!(net.peer(0).client.sim.ctl.player.flying && (110.6..111.6).contains(&y_off), "висит на {y_off}");
    assert_eq!(net.sv.sim.players[i].hp.hp, 20.0);
    net.peer(0).press(P_FLIGHT);
    net.ticks(40, |_, _| {});
    let mut drop = Player::new([0.5, y_off - 101.0, 0.5]);
    drop.vel = [0.0; 3];
    drop.on_ground = false;
    while !drop.on_ground {
        drop.tick(&mut Floor, Keys::default(), false);
    }
    let fd = drop.landed.expect("упало").1;
    let want = 20.0 - crate::game::health::fall_damage_k(fd, 1.0) as f32;
    assert_eq!(net.sv.sim.players[i].hp.hp, want, "урон как от падения с {:.2} (fallDistance {fd})", y_off - 101.0);
    assert!(want <= 13.0, "{want}");
    let st = net.sv.status();
    assert_eq!((st.corrections, st.teleports), (0, 0));
    // Смерть в полёте → возрождение без полёта.
    net.peer(0).press(P_FLIGHT);
    net.ticks(10, |n, _| n.peer(0).set_keys(K_JUMP));
    net.peer(0).set_keys(0);
    assert!(net.sv.sim.players[i].player.flying);
    net.sv.sim.hurt(i, crate::game::health::DamageKind::Generic, 100.0);
    net.ticks(30, |_, _| {});
    net.peer(0).client.sim.respawn();
    net.ticks(15, |_, _| {});
    assert!(net.sv.sim.players[i].hp.alive(), "возродился");
    assert!(!net.sv.sim.players[i].player.flying && !net.peer(0).client.sim.ctl.player.flying, "без полёта");
    net.ticks(10, |n, _| n.peer(0).set_keys(K_FWD));
    // Одна поправка на кадре возрождения бывает и без полёта (прежнее поведение Teleport возрождения) — телепортов нет.
    let st = net.sv.status();
    assert!(st.corrections <= 1 && st.teleports == 0, "после возрождения ходы сходятся: {:?}", (st.corrections, st.teleports));
}
