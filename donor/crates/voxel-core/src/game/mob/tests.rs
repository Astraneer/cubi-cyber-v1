//! Проверки мобов (ASP-16 §11, ASP-17 §12, ASP-18): скорости по таблице, поворот, ступенька, дроп, блуждание по траве,
//! паника, зомби бьёт игрока ночью и горит днём, стада детерминированы, голос в среднем раз в 8 с.

use super::ai::AiCtx;
use super::*;
use crate::abi::K_LOCKED;
use crate::game::health::DamageKind;
use crate::game::physics::Bufs;
use crate::game::server::ServerSim;
use crate::game::Game;
use crate::world::World;

/// Пол из блока b размером (2r+1)² вокруг (0, 0) на y = 100 (высоко над рельефом).
fn floor(w: &World, r: i32, b: u16) {
    for x in -r..=r {
        for z in -r..=r {
            w.set_block(x, 100, z, b);
        }
    }
}

/// Ход моба к далёкой цели по прямой: n тиков MoveControl + тело; ответ — сдвиг за последний тик.
fn walk(kind: MobKind, mult: f32, n: usize) -> f64 {
    let w = World::new(1337);
    for x in -2..=2 {
        for z in -4..=80 {
            w.set_block(x, 100, z, STONE);
        }
    }
    let mut d = w.d.borrow_mut();
    let mut m = Mob::new(1, kind, [0.5, 101.0, 0.5], 0.0, 1);
    let mut bufs = Bufs::default();
    let mut last = 0.0;
    for _ in 0..n {
        m.prev = m.pos;
        m.move_to([0.5, 101.0, 1000.0], mult);
        m.move_tick(&mut d);
        m.jumping = false;
        m.tick_body(&mut *d, &mut bufs);
        last = m.pos[2] - m.prev[2];
    }
    last
}

#[test]
fn mob_walk_speed_table() {
    // ASP-16 §3.2 / §11: установившийся ход за тик = 0.98·(m·s)²/0.454.
    let near = |a: f64, b: f64| (a - b).abs() < 2e-6;
    let cases = [
        (MobKind::Pig, 1.0, 0.134912),
        (MobKind::Pig, 1.25, 0.210800),
        (MobKind::Cow, 1.0, 0.086344),
        (MobKind::Cow, 2.0, 0.345375),
        (MobKind::Chicken, 1.4, 0.264427),
        (MobKind::Sheep, 1.0, 0.114189),
        (MobKind::Sheep, 1.25, 0.178421),
        (MobKind::Zombie, 1.0, 0.114189),
    ];
    for (k, m, want) in cases {
        let got = walk(k, m, 120);
        assert!(near(got, want), "{k:?} ×{m}: {got} ≠ {want}");
    }
    // 90 % хода — к 4-му тику.
    assert!(walk(MobKind::Pig, 1.0, 4) > 0.9 * 0.134912);
}

#[test]
fn mob_turn_180() {
    // Цель позади: yaw поворачивается по 90°/тик — через 2 тика 180.
    let w = World::new(1337);
    floor(&w, 4, STONE);
    let mut d = w.d.borrow_mut();
    let mut m = Mob::new(1, MobKind::Pig, [0.5, 101.0, 0.5], 0.0, 1);
    for _ in 0..2 {
        m.move_to([0.5, 101.0, -50.0], 1.0);
        m.move_tick(&mut d);
    }
    assert!((m.yaw - 180.0).abs() < 1e-3, "yaw {}", m.yaw);
}

#[test]
fn mob_climbs_one_block_by_path() {
    // Стена в 1 блок на z = 3: путь ведёт наверх, свинья подпрыгивает (апекс ≈ 1.2522 над полом) и стоит на блоке.
    let w = World::new(1337);
    floor(&w, 8, STONE);
    for x in -8..=8 {
        for z in 3..=8 {
            w.set_block(x, 101, z, STONE);
        }
    }
    let mut sim = ServerSim::from_world(&w);
    sim.sim.mobs.herds = false;
    sim.sim.mobs.natural = false;
    let id = sim.sim.spawn_mob(MobKind::Pig, [0.5, 101.0, 0.5], false).unwrap();
    let (mut apex, mut on_top) = (0.0f64, None);
    for t in 0..80 {
        let s = &mut sim.sim;
        let d = s.world.d.clone();
        let mut wd = d.borrow_mut();
        let i = s.mobs.index(id).unwrap();
        let mut budget = PATH_BUDGET;
        let (mut out, mut rev, mut ev) = (Vec::new(), Vec::new(), crate::game::view::Events::default());
        let Mobs { list, stats, fx, bufs, .. } = &mut s.mobs;
        let m = &mut list[i];
        if t == 0 {
            let mut c = AiCtx { w: &mut wd, players: &[], time: 0, dark: 0, budget: &mut budget, out: &mut out, revenge: &mut rev, stats, ev: &mut ev, fx };
            assert!(m.nav_to(&mut c, [0, 102, 5], 0, 1.0), "путь наверх");
        }
        m.prev = m.pos;
        m.nav_tick(&mut wd);
        m.move_tick(&mut wd);
        m.jumping = std::mem::take(&mut m.ai.jump_req);
        m.tick_body(&mut *wd, bufs);
        apex = apex.max(m.pos[1] - 101.0);
        if on_top.is_none() && m.on_ground && m.pos[1] >= 102.0 {
            on_top = Some(t);
        }
    }
    assert!((apex - 1.2522).abs() < 0.01, "апекс {apex}");
    assert!(on_top.is_some_and(|t| t < 40), "на блоке: {on_top:?}");
}

#[test]
fn loot_tables() {
    // ASP-17 §6.1, ASP-18 §10: счёт по многим смертям — в пределах таблиц; детёныш — ничего; горевший — жареное.
    let mut rng = GameRng(9);
    let total = |kind: MobKind, rng: &mut GameRng, f: &dyn Fn(&mut Mob)| -> std::collections::BTreeMap<u16, (i32, i32, i32)> {
        let mut t = std::collections::BTreeMap::new();
        for k in 0..2000 {
            let mut m = Mob::new(k, kind, [0.0; 3], 0.0, 1);
            f(&mut m);
            let mut seen = std::collections::BTreeMap::new();
            for s in m.loot(rng, true) {
                *seen.entry(s.item).or_insert(0) += s.count;
            }
            for (item, n) in seen {
                let e = t.entry(item).or_insert((i32::MAX, 0, 0));
                e.0 = e.0.min(n);
                e.1 = e.1.max(n);
                e.2 += n;
            }
        }
        t
    };
    let pig = total(MobKind::Pig, &mut rng, &|_| {});
    assert_eq!(pig.keys().copied().collect::<Vec<_>>(), vec![I_PORKCHOP]);
    let (lo, hi, sum) = pig[&I_PORKCHOP];
    assert!((lo, hi) == (1, 3) && (sum as f64 / 2000.0 - 2.0).abs() < 0.1);
    let cow = total(MobKind::Cow, &mut rng, &|_| {});
    assert_eq!(cow[&I_BEEF].1, 3);
    assert!(cow[&I_LEATHER].1 == 2 && (cow[&I_LEATHER].2 as f64 / 2000.0 - 1.0).abs() < 0.1, "кожа 0–2, в среднем 1");
    let sheep = total(MobKind::Sheep, &mut rng, &|m| m.color = SHEEP_BLACK);
    assert_eq!(sheep[&(BLACK_WOOL as u16)], (1, 1, 2000), "шерсть своего цвета");
    let shorn = total(MobKind::Sheep, &mut rng, &|m| m.sheared = true);
    assert!(!shorn.contains_key(&(WHITE_WOOL as u16)) && shorn[&I_MUTTON].1 == 2);
    let chicken = total(MobKind::Chicken, &mut rng, &|_| {});
    assert_eq!((chicken[&I_CHICKEN], chicken[&I_FEATHER].1), ((1, 1, 2000), 2));
    let burnt = total(MobKind::Pig, &mut rng, &|m| m.life.fire = 100);
    assert!(burnt.contains_key(&I_COOKED_PORKCHOP) && !burnt.contains_key(&I_PORKCHOP));
    let baby = total(MobKind::Cow, &mut rng, &|m| m.baby = true);
    assert!(baby.is_empty(), "детёныш не роняет");
    let zombie = total(MobKind::Zombie, &mut rng, &|_| {});
    assert_eq!(zombie[&I_ROTTEN_FLESH].1, 2);
    let iron = zombie.get(&I_IRON_INGOT).map_or(0, |e| e.2);
    assert!(iron > 0 && iron < 40, "редкий железный слиток: {iron} из 2000");
}

/// Игра (клиент + локальный сервер) на травяном поле 41×41 на y = 100, игрок в центре; без стад и ночного спауна.
fn field(b: u16) -> (World, Game) {
    let w = World::new(1337);
    floor(&w, 20, b);
    let mut g = Game::new(&w);
    g.server.sim.sim.mobs.herds = false;
    g.server.sim.sim.mobs.natural = false;
    g.teleport(0.5, 101.0, 0.5);
    g.set_keys(K_LOCKED);
    g.set_time_of_day(0.5);
    g.tick();
    (w, g)
}

#[test]
fn pig_wanders_on_grass_and_is_seen() {
    // Пуля: свинья бродит по траве, клиент видит её (зеркало) там же, где сервер, с отставанием ~2 тика.
    let (_w, mut g) = field(GRASS);
    g.debug_spawn_mob(MobKind::Pig as u8, 6.5, 101.0, 0.5, 1);
    let start = g.server.sim.sim.mobs.list[0].pos;
    let mut far = 0.0f64;
    for _ in 0..1200 {
        g.tick();
        let m = &g.server.sim.sim.mobs.list[0];
        far = far.max(((m.pos[0] - start[0]).powi(2) + (m.pos[2] - start[2]).powi(2)).sqrt());
        assert!(m.pos[1] >= 100.99, "не проваливается: {:?}", m.pos);
    }
    assert!(far > 2.0, "бродит: ушла на {far}");
    let id = g.server.sim.sim.mobs.list[0].id;
    let d = g.mob_draws(0.0);
    assert_eq!(d.len(), view::DRAW_STRIDE);
    assert_eq!((d[0] as u32, d[1] as u8), (id, MobKind::Pig as u8));
    let sv = g.server.sim.sim.mobs.list[0].pos;
    let gap = ((d[2] as f64 - sv[0]).powi(2) + (d[4] as f64 - sv[2]).powi(2)).sqrt();
    assert!(gap < 1.0, "клиент рисует рядом с сервером: {gap}");
}

#[test]
fn hit_knocks_back_and_panics() {
    // Удар кулаком: окно 10 тиков (второй удар сразу не проходит), отлёт, паника — бег быстрее блуждания.
    let (_w, mut g) = field(GRASS);
    g.debug_spawn_mob(MobKind::Cow as u8, 0.5, 101.0, 2.5, 1);
    g.set_look(0.0, 20.0);
    for _ in 0..30 {
        g.tick();
    }
    let s = &mut g.server.sim;
    let i = s.idx(g.server.pid.unwrap()).unwrap();
    s.players[i].atk_ticks = 100;
    let id = s.sim.mobs.list[0].id;
    let z0 = s.sim.mobs.list[0].pos[2];
    s.attack_mob(i, id);
    assert_eq!(s.sim.mobs.list[0].life.hp, 9.0, "кулак — 1");
    s.players[i].atk_ticks = 100;
    s.attack_mob(i, id);
    assert_eq!(s.sim.mobs.list[0].life.hp, 9.0, "окно: тот же урон не проходит");
    let mut fastest = 0.0f64;
    for _ in 0..40 {
        g.tick();
        let m = &g.server.sim.sim.mobs.list[0];
        fastest = fastest.max(((m.pos[0] - m.prev[0]).powi(2) + (m.pos[2] - m.prev[2]).powi(2)).sqrt());
    }
    let m = &g.server.sim.sim.mobs.list[0];
    assert!(m.pos[2] - z0 > 1.0 || fastest > 0.2, "отлетела или бежит: dz {}, ход {fastest}", m.pos[2] - z0);
    assert!(fastest > 0.15, "паника ×2.0 быстрее блуждания (0.086): {fastest}");
}

#[test]
fn kill_drops_loot_and_body_vanishes() {
    let (_w, mut g) = field(GRASS);
    g.debug_spawn_mob(MobKind::Pig as u8, 0.5, 101.0, 2.5, 1);
    g.tick();
    let s = &mut g.server.sim;
    let mut ev = crate::game::view::Events::default();
    let before = s.sim.items.list.len();
    assert!(s.sim.hurt_mob(0, Blow { kind: DamageKind::Generic, amount: 20.0, by: None }, &mut ev));
    assert!(!s.sim.mobs.list[0].alive());
    let drops = s.sim.items.list.len() - before;
    assert!((1..=3).contains(&drops) || s.sim.items.list.iter().map(|e| e.stack.count).sum::<i32>() >= 1, "добыча сразу");
    for _ in 0..19 {
        g.tick();
    }
    assert_eq!(g.server.sim.sim.mobs.list.len(), 1, "лежит 20 тиков");
    for _ in 0..3 {
        g.tick();
    }
    assert!(g.server.sim.sim.mobs.list.is_empty(), "тело убрано");
    assert!(g.mob_draws(0.0).is_empty(), "у клиента — тоже");
}

#[test]
fn zombie_hunts_and_hits_player_at_night() {
    // Ночь: зомби в 8 блоках видит игрока, идёт и бьёт (3, броня 0) — здоровье падает, отбрасывание сходится с клиентом.
    let (_w, mut g) = field(STONE);
    g.set_time_of_day(0.0);
    g.debug_spawn_mob(MobKind::Zombie as u8, 0.5, 101.0, 8.5, 1);
    let mut hit = None;
    for t in 0..200 {
        g.tick();
        if g.me().hp.hp < 20.0 && hit.is_none() {
            hit = Some(t);
            assert!(g.server.sim.sim.mobs.list[0].target.is_some(), "цель — игрок");
            assert_eq!(g.me().hp.hp, 17.0, "удар 3");
        }
    }
    assert!(hit.is_some_and(|t| t < 120), "зомби дошёл и ударил: {hit:?}");
    assert!(g.me().hp.hp < 17.0 || !g.me().hp.alive(), "бьёт раз в 20 тиков: {}", g.me().hp.hp);
    assert_eq!(g.server.sim.stats.teleports, 0, "отбрасывание без поправок-телепортов");
}

#[test]
fn zombie_burns_in_daylight() {
    // Полдень, открытое небо: зомби загорается (4 %/тик) и сгорает; под водой — нет.
    let (_w, mut g) = field(STONE);
    g.teleport(0.5, 101.0, -15.5);
    g.debug_spawn_mob(MobKind::Zombie as u8, 0.5, 101.0, 15.5, 1);
    let mut lit = false;
    for _ in 0..1200 {
        g.tick();
        if g.server.sim.sim.mobs.list.first().is_some_and(|m| m.life.fire > 0) {
            lit = true;
        }
        if g.server.sim.sim.mobs.list.is_empty() {
            break;
        }
    }
    assert!(lit, "загорелся");
    assert!(g.server.sim.sim.mobs.list.is_empty(), "сгорел за минуту");
}

#[test]
fn herds_are_deterministic_and_on_grass() {
    // Стада одного куска — те же при каждом запуске (ГСЧ от сида и куска); на траве.
    let run = || {
        let w = World::new(1337);
        let mut sim = ServerSim::from_world(&w);
        let d = sim.sim.world.d.clone();
        let mut wd = d.borrow_mut();
        for cx in -6..6 {
            for cz in -6..6 {
                sim.sim.populate_chunk(&mut wd, cx, cz);
            }
        }
        sim.sim.mobs.list.iter().map(|m| (m.kind, m.pos.map(|v| v.to_bits()), m.baby)).collect::<Vec<_>>()
    };
    let (a, b) = (run(), run());
    assert_eq!(a, b);
    assert!(!a.is_empty(), "на 144 кусках есть стада");
    let w = World::new(1337);
    let mut wd = w.d.borrow_mut();
    for (_, p, _) in &a {
        let p = p.map(f64::from_bits);
        assert!(is_grass_block(&mut wd, p[0].floor() as i32, p[1] as i32 - 1, p[2].floor() as i32), "на траве: {p:?}");
    }
}

#[test]
fn ambient_interval_and_darkness() {
    // ASP-16 §11: голос животных — в среднем 160 тиков (задержка 120). Тьма: полдень 0, полночь 11.
    let mut m = Mob::new(5, MobKind::Pig, [0.0; 3], 0.0, 1);
    let (mut ev, mut fx) = (crate::game::view::Events::default(), GameRng(1));
    let (mut n, mut last, mut sum) = (0, 0u64, 0u64);
    for t in 1..=400_000u64 {
        m.tick_ambient(&mut ev, &mut fx);
        if m.ambient < 0 && m.ambient == -m.def().ambient_delay {
            if last > 0 {
                sum += t - last;
                n += 1;
            }
            last = t;
        }
        ev.drain();
    }
    let mean = sum as f64 / n as f64;
    assert!((mean - 160.3).abs() < 2.0, "среднее {mean}");
    assert_eq!(spawn::ambient_darkness(6000), 0);
    assert_eq!(spawn::ambient_darkness(18000), 11);
    assert!(spawn::ambient_darkness(13200) >= 4, "к 19:12 ночь: зомби уже могут появиться");
}

#[test]
fn zombies_spawn_at_night_not_by_day() {
    // Поле без стен (над рельефом): днём — ни одного; ночью к 400 тикам появляются (в 24…72 блоках), не больше капа.
    let w = World::new(1337);
    let mut g = Game::new(&w);
    g.spawn(300.0, 300.0);
    g.server.sim.sim.mobs.herds = false;
    g.set_time_of_day(0.5);
    g.set_day_cycle(false);
    for _ in 0..200 {
        g.tick();
    }
    assert_eq!(g.server.sim.sim.mobs.count(true), 0, "днём нет");
    g.set_time_of_day(0.0);
    for _ in 0..400 {
        g.tick();
    }
    let n = g.server.sim.sim.mobs.count(true);
    assert!(n > 0, "ночью появились");
    assert!(n <= 19, "кап 70·81/289: {n}");
}
