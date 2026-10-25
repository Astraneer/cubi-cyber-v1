//! Проверки среза 4 «выживание» (только для cargo test): урон падения по таблице ASP-12 §16 в живой петле клиент ↔ сервер,
//! вода и лестница спасают, кактус, утопление, лава и горение, смерть с выпадением вещей, возрождение, регенерация, броня.
//! Сцена: каменный пол 21×21 на y = 100 высоко над рельефом, игрок — на нём; урон считает сервер, клиент получает
//! Msg::Vitals (сериализация та же, что по сети).

use super::*;
use crate::abi::*;
use crate::game::health::DamageKind;
use crate::game::inventory::OFFHAND;
use crate::registry::ids::*;
use crate::registry::items::*;

/// Пол 21×21 на y = 100; у игрока снята неуязвимость входа.
fn scene() -> (World, Game) {
    scene_with(|_| {})
}
fn scene_with(f: impl FnOnce(&World)) -> (World, Game) {
    let w = World::new(1337);
    for x in -10..=10 {
        for z in -10..=10 {
            w.set_block(x, 100, z, STONE);
        }
    }
    f(&w);
    let mut g = Game::new(&w);
    g.teleport(0.5, 101.0, 0.5);
    g.body(|p| {
        p.on_ground = true;
        p.vel = [0.0, -0.0784, 0.0];
    });
    g.set_look(0.0, 0.0);
    g.tick();
    g.on_server(|sv, i| sv.players[i].hp.spawn_protect = 0);
    (w, g)
}
fn hp(g: &mut Game) -> f32 {
    g.on_server(|sv, i| sv.players[i].hp.hp)
}
fn alive(g: &mut Game) -> bool {
    g.on_server(|sv, i| sv.players[i].hp.alive())
}
/// Сход с края высотой h: в воздухе с vy = −0.0784 (как после шага с края), пол — на y = 100.
fn step_off(g: &mut Game, h: i32) {
    g.teleport(0.5, 101.0 + h as f64, 0.5);
    g.body(|p| {
        p.on_ground = false;
        p.vel = [0.0, -0.0784, 0.0];
        p.fall = 0.0;
    });
}
fn run(g: &mut Game, n: usize) {
    for _ in 0..n {
        g.tick();
    }
}

#[test]
fn fall_damage_table() {
    // ASP-12 §3.1/§16: сход с края — 3 → 0, 4 → 1, 6 → 3, 10 → 7, 22 → 19, 23 → 19 (жив с ½ ♥), 24 → 21 (смерть).
    for (h, dmg) in [(2, 0), (3, 0), (4, 1), (5, 2), (6, 3), (10, 7), (22, 19), (23, 19)] {
        let (_w, mut g) = scene();
        step_off(&mut g, h);
        run(&mut g, 60);
        assert_eq!(hp(&mut g), 20.0 - dmg as f32, "сход с {h}");
        assert!(g.client.sim.vit.hp == 20.0 - dmg as f32, "клиент знает: {} при {h}", g.client.sim.vit.hp);
    }
    let (_w, mut g) = scene();
    step_off(&mut g, 24);
    run(&mut g, 60);
    assert!(!alive(&mut g), "сход с 24 — смерть");
    assert!(g.client.sim.vit.dead);
}

#[test]
fn fall_into_water_no_damage() {
    // Вода в 1 блок спасает с любой высоты (ASP-12 §12 п. 3).
    let (_w, mut g) = scene_with(|w| {
        for x in -3..=3 {
            for z in -3..=3 {
                w.set_block(x, 101, z, WATER);
            }
        }
    });
    step_off(&mut g, 23);
    run(&mut g, 80);
    assert_eq!(hp(&mut g), 20.0, "вода в 1 блок");
}

#[test]
fn armor_does_not_help_against_fall() {
    let (_w, mut g) = scene();
    for k in 0..4 {
        g.set_slot(36 + k, ItemStack::of(I_LEATHER_HELMET + 4 + k as ItemId, 1)); // железный комплект
    }
    step_off(&mut g, 10);
    run(&mut g, 60);
    assert_eq!(hp(&mut g), 13.0);
    let worn: Vec<u16> = g.on_server(|sv, i| (36..40).map(|k| sv.players[i].inv.get(k).damage).collect());
    assert_eq!(worn, vec![0, 0, 0, 0], "падение броню не тратит");
}

#[test]
fn window_between_hits() {
    // Окно: 6 → 14; через 5 тиков 4 — отказ, 9 — добор 3 (11); через 10 после первого — полный.
    let (_w, mut g) = scene();
    g.debug_hurt(6.0);
    g.tick();
    assert_eq!(hp(&mut g), 14.0);
    run(&mut g, 4);
    g.debug_hurt(4.0);
    g.tick();
    assert_eq!(hp(&mut g), 14.0, "4 не больше прошлого 6");
    g.debug_hurt(9.0);
    g.tick();
    assert_eq!(hp(&mut g), 11.0, "добор 9 − 6");
    run(&mut g, 5);
    g.debug_hurt(4.0);
    g.tick();
    assert_eq!(hp(&mut g), 7.0, "вне окна — полный");
}

#[test]
fn cactus_stings_every_ten_ticks() {
    // Кактус рядом: упор в бок (коллизия 14/16) — коробка заходит в ячейку на 1/16, урон раз в 10 тиков.
    let (_w, mut g) = scene_with(|w| {
        w.set_block(1, 101, 0, CACTUS);
    });
    g.set_keys(K_LEFT); // лицом на юг (+Z): влево — на восток (+X)
    let mut seen = Vec::new();
    let mut last = 20.0;
    for t in 0..60 {
        g.tick();
        let h = hp(&mut g);
        if h < last {
            seen.push(t);
            last = h;
        }
    }
    assert!(seen.len() >= 5, "укол за укол: {seen:?}");
    assert_eq!(seen.windows(2).map(|w| w[1] - w[0]).collect::<Vec<_>>(), vec![10; seen.len() - 1], "раз в 10 тиков: {seen:?}");
    assert_eq!(hp(&mut g), 20.0 - seen.len() as f32);
}

#[test]
fn cactus_armor_reduces_and_wears() {
    let (_w, mut g) = scene_with(|w| {
        w.set_block(1, 101, 0, CACTUS);
    });
    for k in 0..4 {
        g.set_slot(36 + k, ItemStack::of(I_LEATHER_HELMET + 4 + k as ItemId, 1)); // железный комплект
    }
    g.set_keys(K_LEFT);
    run(&mut g, 45);
    // Железо 15 очков: 0.42 за укол, каждая часть −1 прочности за укол.
    let worn: Vec<u16> = g.on_server(|sv, i| (36..40).map(|k| sv.players[i].inv.get(k).damage).collect());
    assert!(worn.iter().all(|&d| d >= 4), "износ {worn:?}");
    let h = hp(&mut g);
    assert!(h > 17.5 && h < 19.0, "сердца убывают втрое медленнее: {h}");
}

#[test]
fn drowning_timeline() {
    // Вода глубиной 3 над полом, игрок на дне: голова под водой, воздух 300 → 0 за 300 тиков, первый урон на 320-м.
    let (_w, mut g) = scene_with(|w| {
        for x in -3..=3 {
            for z in -3..=3 {
                for y in 101..=103 {
                    w.set_block(x, y, z, WATER);
                }
            }
        }
    });
    g.teleport(0.5, 101.0, 0.5);
    g.body(|p| {
        p.vel = [0.0, 0.0, 0.0];
        p.fall = 0.0;
    });
    let air_at = |g: &mut Game| g.on_server(|sv, i| sv.players[i].hp.air);
    run(&mut g, 100);
    let a = air_at(&mut g);
    assert!(a > 190 && a <= 200, "−1 за тик под водой: {a}");
    assert_eq!(hp(&mut g), 20.0);
    while air_at(&mut g) > 1 {
        g.tick();
    }
    assert_eq!(hp(&mut g), 20.0);
    run(&mut g, 21);
    assert_eq!(hp(&mut g), 18.0, "первый урон утопления — 2");
    run(&mut g, 20);
    assert_eq!(hp(&mut g), 16.0, "дальше каждую секунду");
    assert!(g.client.sim.vit.air <= 0, "клиент видит воздух");
}

#[test]
fn lava_burns_to_death() {
    let (_w, mut g) = scene_with(|w| {
        for x in -3..=3 {
            for z in -3..=3 {
                for y in 101..=102 {
                    w.set_block(x, y, z, LAVA);
                }
            }
        }
    });
    g.teleport(0.5, 101.0, 0.5);
    g.body(|p| p.vel = [0.0, 0.0, 0.0]);
    run(&mut g, 12);
    let h = hp(&mut g);
    assert!(h <= 16.0, "лава бьёт сразу: {h}");
    run(&mut g, 80);
    assert!(!alive(&mut g), "лава убивает");
    assert_eq!(g.on_server(|sv, i| sv.players[i].death_kind), Some(DamageKind::Lava));
}

#[test]
fn water_extinguishes() {
    let (_w, mut g) = scene_with(|w| {
        for x in -3..=3 {
            for z in -3..=3 {
                w.set_block(x, 101, z, WATER);
            }
        }
    });
    g.on_server(|sv, i| sv.players[i].hp.ignite(200));
    run(&mut g, 2);
    assert_eq!(g.on_server(|sv, i| sv.players[i].hp.fire), 0);
}

#[test]
fn death_drops_everything_and_respawn_works() {
    let (_w, mut g) = scene();
    g.kit();
    g.set_slot(36, ItemStack::of(I_LEATHER_HELMET, 1));
    g.set_slot(OFFHAND, ItemStack::of(TORCH as ItemId, 3));
    g.tick();
    let before: i32 = (0..41).map(|k| g.me().inv.get(k).n()).sum();
    assert!(before > 10);
    g.debug_hurt(100.0);
    g.tick();
    assert!(!alive(&mut g));
    run(&mut g, 2);
    assert!(g.client.sim.vit.dead, "клиент знает о смерти");
    let inv_total: i32 = (0..41).map(|k| g.me().inv.get(k).n()).sum();
    assert_eq!(inv_total, 0, "всё выпало, включая броню и вторую руку");
    let dropped: i32 = g.sim().items.list.iter().map(|e| e.stack.n()).sum();
    assert_eq!(dropped, before, "на земле всё, что было");
    // Кнопка возрождения — с 20-го тика смерти.
    g.respawn();
    run(&mut g, 2);
    assert!(!alive(&mut g), "раньше 20 тиков кнопка не работает");
    // Мёртвый не подбирает свои вещи (задержка подбора 40 тиков прошла): инвентарь пуст.
    run(&mut g, 50);
    let inv_total: i32 = (0..41).map(|k| g.me().inv.get(k).n()).sum();
    assert_eq!(inv_total, 0, "мёртвый ничего не подбирает");
    let spawn = g.on_server(|sv, _| sv.spawn_point().0);
    g.respawn();
    run(&mut g, 3);
    assert!(alive(&mut g), "возродился");
    assert_eq!(hp(&mut g), 20.0);
    assert!(!g.client.sim.vit.dead);
    let p = g.me().player.pos;
    assert!((p[0] - spawn[0]).abs() < 1.0 && (p[2] - spawn[2]).abs() < 1.0, "на точке появления {p:?} ≠ {spawn:?}");
    let inv_total: i32 = (0..41).map(|k| g.me().inv.get(k).n()).sum();
    assert_eq!(inv_total, 0, "инвентарь пуст");
    let cp = g.ctl().player.pos;
    assert!((cp[0] - p[0]).abs() < 0.5 && (cp[2] - p[2]).abs() < 0.5, "клиент переставлен");
    // 60 тиков неуязвимости.
    g.debug_hurt(5.0);
    run(&mut g, 3);
    assert_eq!(hp(&mut g), 20.0, "неуязвимость после возрождения");
}

#[test]
fn regen_after_pause() {
    let (_w, mut g) = scene();
    g.debug_hurt(10.0);
    g.tick();
    assert_eq!(hp(&mut g), 10.0);
    run(&mut g, 130);
    assert_eq!(hp(&mut g), 10.0, "5 с без лечения");
    run(&mut g, 15);
    assert_eq!(hp(&mut g), 11.0, "первое +½ ♥ на 140-м тике");
    run(&mut g, 40);
    assert_eq!(hp(&mut g), 12.0);
}

#[test]
fn void_hurts_through_protection() {
    let (_w, mut g) = scene();
    g.on_server(|sv, i| sv.players[i].hp.spawn_protect = 60);
    g.teleport(0.5, -70.0, 0.5);
    g.body(|p| p.vel = [0.0, 0.0, 0.0]);
    run(&mut g, 3);
    assert!(hp(&mut g) < 20.0, "пустота пробивает неуязвимость");
}

#[test]
fn creative_flight_is_invulnerable() {
    let (_w, mut g) = scene();
    g.set_flying(true);
    g.tick();
    g.debug_hurt(5.0);
    run(&mut g, 3);
    assert_eq!(hp(&mut g), 20.0);
}

#[test]
fn dead_player_cannot_act() {
    let (w, mut g) = scene_with(|w| {
        w.set_block(0, 101, 2, DIRT);
    });
    g.debug_hurt(100.0);
    run(&mut g, 3);
    g.set_look(0.0, 30.0);
    g.set_keys(K_LOCKED | K_ATTACK);
    g.press(P_ATTACK);
    run(&mut g, 40);
    assert_eq!(w.get_block(0, 101, 2), DIRT, "мёртвый не ломает");
}

/// Глубокая вода 9×9×27 над полом (до верха мира); игрок посередине, в толще воды, без скорости.
fn deep_water() -> (World, Game) {
    let (w, mut g) = scene_with(|w| {
        for x in -4..=4 {
            for z in -4..=4 {
                for y in 101..=127 {
                    w.set_block(x, y, z, WATER);
                }
            }
        }
    });
    g.teleport(0.5, 106.0, 0.5);
    g.body(|p| {
        p.on_ground = false;
        p.vel = [0.0, 0.0, 0.0];
        p.fall = 0.0;
    });
    (w, g)
}
/// Смещение по Y на каждом из n тиков (клиентское тело).
fn dy_ticks(g: &mut Game, n: usize) -> Vec<f64> {
    (0..n)
        .map(|_| {
            let y0 = g.ctl().player.pos[1];
            g.tick();
            g.ctl().player.pos[1] - y0
        })
        .collect()
}

#[test]
fn water_sink_swim_up_down_asp03() {
    // ASP-03 §13.1: тону без ввода 0, −0.005, −0.009, −0.0122…; Space: +0.04, 0.067, 0.0886, 0.10588…; Shift: −0.04, −0.077…
    let near = |a: &[f64], b: &[f64]| {
        for (x, y) in a.iter().zip(b) {
            assert!((x - y).abs() < 2e-6, "{a:?} ≠ {b:?}");
        }
    };
    let (_w, mut g) = deep_water();
    near(&dy_ticks(&mut g, 5), &[0.0, -0.005, -0.009, -0.0122, -0.01476]);
    g.assert_in_sync();
    let (_w, mut g) = deep_water();
    g.set_keys(K_JUMP);
    near(&dy_ticks(&mut g, 5), &[0.04, 0.067, 0.0886, 0.10588, 0.119704]);
    let (_w, mut g) = deep_water();
    g.set_keys(K_SNEAK);
    near(&dy_ticks(&mut g, 4), &[-0.04, -0.077, -0.1066, -0.13028]);
    // Пределы: тону −0.025 б/т, Space +0.175.
    let (_w, mut g) = deep_water();
    let d = dy_ticks(&mut g, 60);
    assert!((d[59] + 0.025).abs() < 1e-3, "предел тону {}", d[59]);
    let (_w, mut g) = deep_water();
    g.set_keys(K_JUMP);
    let d = dy_ticks(&mut g, 60);
    assert!((d[59] - 0.175).abs() < 2e-3, "предел всплытия {}", d[59]);
}

#[test]
fn water_walk_speed_asp03() {
    // W в воде: 0.0196, 0.03528, 0.047824…, предел 0.098 б/т (1.96 б/с); бег в воде не включается.
    let (_w, mut g) = deep_water();
    g.set_look(0.0, 0.0);
    g.set_keys(K_FWD);
    let mut dz = Vec::new();
    for _ in 0..30 {
        let z0 = g.ctl().player.pos[2];
        g.tick();
        dz.push(g.ctl().player.pos[2] - z0);
    }
    for (a, b) in dz.iter().zip([0.0196, 0.03528, 0.047824]) {
        assert!((a - b).abs() < 1e-5, "{dz:?}");
    }
    assert!((dz[29] - 0.098).abs() < 1e-3, "предел {}", dz[29]);
    g.set_keys(K_FWD | K_SPRINT);
    run(&mut g, 5);
    assert!(!g.ctl().player.sprinting, "бег в воде не держится");
}

#[test]
fn run_down_stairs_no_fall_damage() {
    // Урок cubi-core: бег вниз по лестнице не копит урон — каждая ступень (приземление) обнуляет падение.
    let (_w, mut g) = scene_with(|w| {
        for k in 0..30 {
            // Ступени вдоль +Z: пол k-й ступени на y = 140 − k − … выше пола арены; шаг 2 блока вперёд, 1 вниз.
            for z in (2 * k)..(2 * k + 2) {
                for x in -1..=1 {
                    for y in 100..(100 + (31 - k)) {
                        w.set_block(x, y, z, STONE);
                    }
                }
            }
        }
    });
    g.teleport(0.5, 131.0, 0.5);
    g.body(|p| {
        p.on_ground = true;
        p.vel = [0.0, -0.0784, 0.0];
        p.fall = 0.0;
    });
    g.set_look(0.0, 0.0);
    g.set_keys(K_FWD | K_SPRINT);
    run(&mut g, 80);
    let z = g.ctl().player.pos[2];
    assert!(z > 20.0, "пробежал {z}");
    assert_eq!(hp(&mut g), 20.0, "бег вниз по ступеням не бьёт");
}

#[test]
fn eyes_in_wall_suffocate_and_leaves_do_not() {
    // Песок на голову: стоя в яме 1×1 глубиной 2, голова в блоке — ½ ♥ раз в 10 тиков; листва не душит.
    let (_w, mut g) = scene_with(|w| {
        w.set_block(0, 101, 0, AIR);
    });
    g.put([0, 102, 0], SAND as Cell);
    g.put([0, 101, 0], SAND as Cell);
    g.body(|p| p.vel = [0.0, 0.0, 0.0]);
    // Игрок внутри песка (глаза в блоке): урон стены.
    run(&mut g, 25);
    assert!(hp(&mut g) < 20.0, "удушье в блоке: {}", hp(&mut g));
    let (_w, mut g) = scene_with(|_| {});
    g.put([0, 102, 0], LEAVES as Cell);
    g.put([0, 101, 0], LEAVES as Cell);
    g.body(|p| p.vel = [0.0, 0.0, 0.0]);
    run(&mut g, 25);
    assert_eq!(hp(&mut g), 20.0, "листва не душит");
}

/// Яблоки в руке (ячейка 0), здоровье hp_now.
fn with_apples(g: &mut Game, n: i32, hp_now: f32) {
    g.set_slot(0, ItemStack::of(I_APPLE, n));
    g.select(0);
    g.on_server(|sv, i| {
        sv.players[i].hp.hp = hp_now;
        sv.players[i].hp.last_hurt = sv.players[i].hp.now; // регенерация не мешает числам
    });
    g.tick();
}
fn apples(g: &mut Game) -> i32 {
    g.me().inv.main[0].n()
}

#[test]
fn eating_takes_32_ticks_and_heals() {
    // ASP-12 §9: нажатие в тике N, съедено на N+31 (32-й тик), яблоко при HP 12 → 16; стек −1.
    let (_w, mut g) = scene();
    with_apples(&mut g, 3, 12.0);
    g.set_keys(K_LOCKED | K_USE);
    run(&mut g, 31);
    assert_eq!((hp(&mut g), apples(&mut g)), (12.0, 3), "за 31 тик ещё не доел");
    assert!(g.client.sim.eating.is_some());
    g.tick();
    assert_eq!((hp(&mut g), apples(&mut g)), (16.0, 2), "на 32-м тике +2 ♥ и стек −1");
    g.assert_in_sync();
}

#[test]
fn eating_stops_when_released() {
    let (_w, mut g) = scene();
    with_apples(&mut g, 3, 12.0);
    g.set_keys(K_LOCKED | K_USE);
    run(&mut g, 12);
    g.set_keys(K_LOCKED);
    run(&mut g, 40);
    assert_eq!((hp(&mut g), apples(&mut g)), (12.0, 3), "отпустил — яблоко цело");
    assert!(g.client.sim.eating.is_none());
    assert!(g.on_server(|sv, i| sv.players[i].eating.is_none()));
}

#[test]
fn full_health_cannot_eat() {
    let (_w, mut g) = scene();
    with_apples(&mut g, 3, 20.0);
    g.set_keys(K_LOCKED | K_USE);
    run(&mut g, 80);
    assert_eq!((hp(&mut g), apples(&mut g)), (20.0, 3), "при полных сердцах ПКМ с едой — ничего");
    assert!(g.client.sim.eating.is_none());
}

#[test]
fn eating_slows_walking_and_stays_in_sync() {
    // Пока ест, ввод ×0.2: клиент и сервер идут одинаково (ни поправок, ни телепортов).
    let (_w, mut g) = scene();
    with_apples(&mut g, 3, 10.0);
    g.set_keys(K_LOCKED | K_USE | K_FWD);
    let z0 = g.ctl().player.pos[2];
    run(&mut g, 25);
    let dz = g.ctl().player.pos[2] - z0;
    assert!(dz < 2.0 && dz > 0.3, "шаг ×0.2: {dz}");
    run(&mut g, 40);
    g.assert_in_sync();
}

#[test]
fn damage_does_not_interrupt_eating() {
    let (_w, mut g) = scene();
    with_apples(&mut g, 3, 15.0);
    g.set_keys(K_LOCKED | K_USE);
    run(&mut g, 10);
    g.debug_hurt(2.0);
    run(&mut g, 30);
    // 15 − 2 + 4 = 17 (регенерация не успела: пауза 5 с).
    assert_eq!((hp(&mut g), apples(&mut g)), (17.0, 2));
}

#[test]
fn food_blocks_attack_and_break_while_eating() {
    let (w, mut g) = scene_with(|w| {
        w.set_block(0, 101, 2, DIRT);
    });
    with_apples(&mut g, 3, 10.0);
    g.set_look(0.0, 30.0);
    g.set_keys(K_LOCKED | K_USE);
    run(&mut g, 3);
    assert!(g.client.sim.eating.is_some());
    g.set_keys(K_LOCKED | K_USE | K_ATTACK);
    g.press(P_ATTACK);
    run(&mut g, 25);
    assert_eq!(w.get_block(0, 101, 2), DIRT, "ест — ломать нельзя");
}

#[test]
fn fluids_keep_client_and_server_in_step() {
    // Вода и лава: физика одна у клиента и сервера — ни поправок теневого тика, ни телепортов (падение в воду, плавание, лава).
    let (_w, mut g) = scene_with(|w| {
        for x in -3..=3 {
            for z in -3..=3 {
                for y in 101..=104 {
                    w.set_block(x, y, z, WATER);
                }
            }
        }
    });
    step_off(&mut g, 15);
    run(&mut g, 40);
    g.set_look(0.0, 0.0);
    g.set_keys(K_FWD | K_JUMP);
    run(&mut g, 30);
    g.set_keys(K_LEFT | K_SNEAK);
    run(&mut g, 30);
    assert_eq!((g.server.sim.stats.corrections, g.server.sim.stats.teleports), (0, 0), "вода");
    assert_eq!(hp(&mut g), 20.0);
    let (_w, mut g) = scene_with(|w| {
        for x in -3..=3 {
            for z in -3..=3 {
                for y in 101..=102 {
                    w.set_block(x, y, z, LAVA);
                }
            }
        }
    });
    step_off(&mut g, 8);
    run(&mut g, 70);
    assert!(!alive(&mut g));
    assert_eq!((g.server.sim.stats.corrections, g.server.sim.stats.teleports), (0, 0), "лава");
}

#[test]
fn no_damage_in_crossroads_zone() {
    // Зона Перекрёстка (r = 32, spawn.md): урона нет, кроме пустоты; вне зоны — есть.
    crate::gen::spawn::site_on();
    let (_w, mut g) = scene();
    let (sx, sz) = g.on_server(|sv, _| {
        let w = sv.sim.world.d.borrow();
        (w.gen().site.x, w.gen().site.z)
    });
    g.on_server(|sv, i| sv.players[i].hp.spawn_protect = 0);
    g.on_server(|sv, i| sv.players[i].player.teleport([sx as f64 + 0.5, 200.0, sz as f64 + 0.5]));
    g.debug_hurt(5.0);
    g.tick();
    assert_eq!(hp(&mut g), 20.0, "в зоне урона нет");
    g.on_server(|sv, i| sv.players[i].player.teleport([sx as f64 + 100.5, 200.0, sz as f64 + 0.5]));
    g.debug_hurt(5.0);
    g.tick();
    assert_eq!(hp(&mut g), 15.0, "вне зоны — есть");
}
