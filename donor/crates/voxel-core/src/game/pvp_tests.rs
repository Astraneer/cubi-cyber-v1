//! PvP по сети (G10): два клиента с задержкой 75 ± 20 мс через net::Server / net::Peer. Удар доходит, урон считает
//! сервер (окно неуязвимости, броня), отбрасывание не вызывает поправок, античит режет дальность и стену, зона
//! Перекрёстка защищает, «ударил и убежал» не спасает, смерть — сообщение в чат и выпадение вещей.

use super::net_tests::{put_on, LagNet};
use crate::cell::Cell;
use crate::game::health::DamageKind;
use crate::gen::spawn::site_on;
use crate::proto::{Act, CHAT_DEATH};
use crate::registry::ids::{AIR, STONE};
use crate::registry::items::{ItemId, I_LEATHER_HELMET, I_WOODEN_PICKAXE};
use crate::world::stack::ItemStack;

/// Железный меч: урон 6, скорость 1.6 (перезарядка 12.5 тиков).
const IRON_SWORD: ItemId = I_WOODEN_PICKAXE + 14;

fn idx(net: &mut LagNet, k: usize) -> usize {
    let id = net.peer(k).id();
    net.sv.sim.idx(id).expect("игрок на сервере")
}

fn hp(net: &mut LagNet, k: usize) -> f32 {
    let i = idx(net, k);
    net.sv.sim.players[i].hp.hp
}

/// Двое входят; защита входа снята; у A (k = 0) в руке железный меч.
fn duel(delay: u64, jitter: i64) -> LagNet {
    let mut net = LagNet::new(&["Аня", "Боря"], delay, jitter);
    // Ещё одна площадка продолжает ту, что у LagNet: x 8…60, z −2…8 — через границу Перекрёстка (радиус 32) и за ней.
    for x in 8..61 {
        for z in -2..9 {
            net.sv.sim.world_edit([x, 100, z], STONE as Cell);
        }
    }
    net.sv.sim.sim.world.d.borrow_mut().take_log();
    net.until_ready();
    net.ticks(4, |_, _| {});
    for k in 0..2 {
        let i = idx(&mut net, k);
        net.sv.sim.players[i].hp.spawn_protect = 0;
    }
    net.peer(0).give(IRON_SWORD, 1);
    net.ticks(4, |_, _| {});
    net
}

/// A лицом к B на расстоянии 2 по оси Z (A в (x, z), B в (x, z + 2)): оба стоят, ждём, пока A увидит B.
fn face_off(net: &mut LagNet, x: f64, z: f64) {
    put_on(net, 0, x, z);
    put_on(net, 1, x, z + 2.0);
    net.peer(0).set_look(0.0, 0.0);
    net.peer(1).set_look(180.0, 0.0);
    net.ticks(14, |_, _| {});
}

/// A бьёт (ЛКМ) и ждёт доставки.
fn swing(net: &mut LagNet) {
    net.peer(0).click();
    net.ticks(14, |_, _| {});
}

/// Удар доходит по сети: урон мечом 6 у сервера и в сердцах B, A цел; повторный удар в окне неуязвимости не проходит,
/// после окна — проходит; отбрасывание без поправок и телепортов; клиент не предсказывает чужой урон.
#[test]
fn pvp_hit_arrives_over_lag() {
    let mut net = duel(75, 20);
    face_off(&mut net, 0.5, 0.5);
    let ib = net.peer(1).id();
    assert_eq!(net.peer(0).client.sim.aim_player, Some(ib), "A видит B под прицелом");
    swing(&mut net);
    assert_eq!(hp(&mut net, 1), 14.0, "железный меч: 6");
    assert_eq!(net.peer(1).client.sim.vit.hp, 14.0, "B видит свои сердца");
    assert_eq!(hp(&mut net, 0), 20.0);
    assert_eq!(net.peer(0).client.sim.vit.hp, 20.0);
    let i = idx(&mut net, 1);
    assert!(net.sv.sim.players[i].player.pos[2] > 2.55, "отброшен от A: {:?}", net.sv.sim.players[i].player.pos);
    assert_eq!(net.sv.sim.stats.teleports, 0, "отбрасывание без телепортов");
    let (z_sv, z_cl) = (net.sv.sim.players[i].player.pos[2], net.peer(1).pos()[2]);
    assert!((z_sv - z_cl).abs() < 0.3, "клиент B там же, где сервер: {z_sv} {z_cl}");
    // Второй удар сразу после первого (перезарядка 5 тиков из 12, окно 10 тиков): слабее — не проходит.
    face_off(&mut net, 0.5, 0.5);
    let a = idx(&mut net, 0);
    net.sv.sim.players[a].atk_ticks = 0;
    net.sv.sim.players[i].hp.time_until_regen = 20;
    net.peer(0).click();
    net.ticks(6, |_, _| {});
    assert_eq!(hp(&mut net, 1), 14.0, "в окне неуязвимости слабый удар не засчитан");
    // Окно прошло — снова полный удар.
    net.ticks(30, |_, _| {});
    swing(&mut net);
    assert_eq!(hp(&mut net, 1), 8.0);
    assert_eq!(net.peer(1).client.sim.vit.hp, 8.0);
}

/// Броня снижает удар игрока и изнашивается: железный комплект (15) — 6 → 3.12.
#[test]
fn pvp_armor_reduces_damage() {
    let mut net = duel(0, 0);
    let i = idx(&mut net, 1);
    for k in 0..4 {
        net.sv.sim.players[i].inv.armor[k] = ItemStack::of((I_LEATHER_HELMET + 4 + k as u16) as _, 1);
    }
    net.ticks(4, |_, _| {});
    face_off(&mut net, 0.5, 0.5);
    swing(&mut net);
    let left = hp(&mut net, 1);
    assert!((20.0 - left - 3.12).abs() < 0.01, "железная броня: 6 → 3.12, осталось {left}");
    assert!(net.sv.sim.players[i].inv.armor.iter().all(|s| s.damage == 1), "каждая часть брони -1 прочности");
    assert_eq!(net.peer(1).client.sim.vit.armor, 15);
}

/// Сила удара: перезарядка, крит в падении, бег — отбрасывание; урон идёт по формуле PlayerEntity.attack.
#[test]
fn pvp_strike_numbers() {
    let mut net = duel(0, 0);
    face_off(&mut net, 0.5, 0.5);
    let (a, b) = (idx(&mut net, 0), idx(&mut net, 1));
    let hit = |net: &mut LagNet, f: &dyn Fn(&mut crate::game::server::ServerPlayer)| -> f32 {
        net.sv.sim.players[b].hp.hp = 20.0;
        net.sv.sim.players[b].hp.time_until_regen = 0;
        net.sv.sim.players[b].hp.last_damage_taken = 0.0;
        net.sv.sim.players[a].atk_ticks = 40;
        let id = net.sv.sim.players[b].id;
        let p = &mut net.sv.sim.players[a];
        p.player.on_ground = true;
        p.player.fall = 0.0;
        p.player.sprinting = false;
        f(p);
        net.sv.sim.attack_player(a, id);
        20.0 - net.sv.sim.players[b].hp.hp
    };
    assert_eq!(hit(&mut net, &|_| {}), 6.0, "полная перезарядка");
    // Только что ударил: h = 0.5 / 12.5 = 0.04 → 6·(0.2 + 0.8·0.0016) ≈ 1.208.
    let weak = hit(&mut net, &|p| p.atk_ticks = 0);
    net.sv.sim.players[a].atk_ticks = 0;
    assert!(weak < 1.3, "слабый удар {weak}");
    // Крит: падение, не на земле, не бег → ×1.5; бег и крит друг друга исключают.
    assert_eq!(hit(&mut net, &|p| { p.player.fall = 1.0; p.player.on_ground = false; }), 9.0);
    assert_eq!(hit(&mut net, &|p| { p.player.fall = 1.0; p.player.on_ground = false; p.player.sprinting = true; }), 6.0);
    // Кулак: 1 и скорость 4 (перезарядка 5 тиков).
    net.sv.sim.players[a].inv.set(0, ItemStack::EMPTY);
    assert_eq!(hit(&mut net, &|_| {}), 1.0);
}

/// Бег после полной перезарядки добавляет отбрасывание (0.5 вдоль взгляда): отлёт дальше, чем от обычного удара.
#[test]
fn pvp_sprint_knockback_farther() {
    let mut fly = [0.0f64; 2];
    for (n, sprint) in [false, true].into_iter().enumerate() {
        let mut net = duel(0, 0);
        face_off(&mut net, 0.5, 0.5);
        let (a, b) = (idx(&mut net, 0), idx(&mut net, 1));
        net.sv.sim.players[a].player.sprinting = sprint;
        net.sv.sim.players[a].atk_ticks = 40;
        let id = net.sv.sim.players[b].id;
        let z0 = net.sv.sim.players[b].player.pos[2];
        net.sv.sim.attack_player(a, id);
        net.ticks(30, |_, _| {});
        fly[n] = net.sv.sim.players[b].player.pos[2] - z0;
        assert_eq!(net.sv.sim.stats.teleports, 0);
    }
    assert!(fly[0] > 0.3 && fly[1] > fly[0] + 0.3, "без бега {:.2}, с бегом {:.2}", fly[0], fly[1]);
}

/// Античит: дальняя цель и цель за стеной — отказ, здоровье цело; та же цель вплотную и открытая — бьётся.
#[test]
fn pvp_rejects_far_and_walled_targets() {
    let mut net = duel(0, 0);
    let target = net.peer(1).id();
    // Далеко: 10 блоков по Z (клиент не увидит цель под прицелом; Act::Attack подделан).
    put_on(&mut net, 0, 0.5, -6.0);
    put_on(&mut net, 1, 0.5, 6.0);
    net.ticks(8, |_, _| {});
    let r0 = net.sv.sim.stats.rejected;
    net.peer(0).client.sim.acts.push(Act::Attack { target });
    net.ticks(8, |_, _| {});
    assert_eq!(hp(&mut net, 1), 20.0, "12 блоков — мимо");
    assert!(net.sv.sim.stats.rejected > r0);
    // За стеной: 3 блока, между ними камень в два блока высотой (z = 2) на всю ширину.
    put_on(&mut net, 0, 0.5, 0.5);
    put_on(&mut net, 1, 0.5, 4.5);
    for x in -3..5 {
        for y in 101..105 {
            net.sv.sim.world_edit([x, y, 2], STONE as Cell);
        }
    }
    net.ticks(8, |_, _| {});
    let r1 = net.sv.sim.stats.rejected;
    net.peer(0).client.sim.acts.push(Act::Attack { target });
    net.ticks(8, |_, _| {});
    assert_eq!(hp(&mut net, 1), 20.0, "за стеной — отказ");
    assert!(net.sv.sim.stats.rejected > r1);
    // Стену убрали — тот же поддельный удар проходит (цель в 4 блоках: в запасе сервера).
    for x in -3..5 {
        for y in 101..105 {
            net.sv.sim.world_edit([x, y, 2], AIR as Cell);
        }
    }
    net.ticks(8, |_, _| {});
    net.peer(0).client.sim.acts.push(Act::Attack { target });
    net.ticks(8, |_, _| {});
    assert_eq!(hp(&mut net, 1), 14.0, "открытая цель в запасе дальности — удар прошёл");
    // Сам себя, мёртвый и несуществующий номер — ничего.
    let me = net.peer(0).id();
    net.peer(0).client.sim.acts.push(Act::Attack { target: me });
    net.peer(0).client.sim.acts.push(Act::Attack { target: 9_999_999 });
    net.ticks(6, |_, _| {});
    assert_eq!(hp(&mut net, 0), 20.0);
}

/// Убийство: сообщение «Боря был убит Аней» всем, вещи выпали, экран смерти знает причину, возрождение на Перекрёстке.
#[test]
fn pvp_kill_chat_drop_respawn() {
    let mut net = duel(75, 20);
    let seen = [net.peer(0).chat_total(), net.peer(1).chat_total()];
    net.peer(1).give(AIR as ItemId + 1, 7);
    net.ticks(6, |_, _| {});
    face_off(&mut net, 0.5, 0.5);
    let b = idx(&mut net, 1);
    net.sv.sim.players[b].hp.hp = 4.0;
    let drops0 = net.sv.sim.sim.items.list.len();
    swing(&mut net);
    assert!(!net.sv.sim.players[b].hp.alive(), "добит");
    assert!(net.peer(1).client.sim.vit.dead);
    assert_eq!(net.peer(1).client.sim.vit.cause, DamageKind::PlayerAttack.code());
    assert!(net.sv.sim.sim.items.list.len() > drops0, "вещи выпали");
    for k in 0..2 {
        let lines = net.peer(k).chat_since(seen[k]);
        assert!(lines.contains(&(CHAT_DEATH, "Боря был убит Аня".to_string())), "чат игрока {k}: {lines:?}");
    }
    // Мёртвого не бьют и он не бьёт.
    swing(&mut net);
    assert_eq!(net.sv.sim.stats.teleports, 0);
}

/// Перекрёсток: в зоне урона нет; выйдя, ещё 10 с защиты; кто ударил незащищённого — защиты не имеет («ударил и
/// убежал»); метка боя спадает через 10 с. Здоровье сравнивается «не меньше / меньше»: регенерация идёт сама.
#[test]
fn pvp_crossroads_protection() {
    site_on();
    let mut net = duel(75, 20);
    net.peer(1).give(IRON_SWORD, 1);
    net.ticks(4, |_, _| {});
    let place = |net: &mut LagNet, ax: f64, bx: f64| {
        net.peer(0).teleport([ax, 101.0, 2.5]);
        net.peer(1).teleport([bx, 101.0, 2.5]);
        net.peer(0).set_look(-90.0, 0.0);
        net.peer(1).set_look(90.0, 0.0);
        net.ticks(14, |_, _| {});
    };
    // k бьёт (полная перезарядка) и ждёт доставки.
    let strike = |net: &mut LagNet, k: usize| {
        let i = idx(net, k);
        net.sv.sim.players[i].atk_ticks = 99;
        net.peer(k).click();
        net.ticks(14, |_, _| {});
    };
    let (a, b) = (idx(&mut net, 0), idx(&mut net, 1));

    // Оба в зоне: никто никого.
    place(&mut net, 20.5, 22.5);
    strike(&mut net, 0);
    strike(&mut net, 1);
    assert_eq!((hp(&mut net, 0), hp(&mut net, 1)), (20.0, 20.0), "в зоне урона нет");

    // A в зоне, B снаружи и давно (защита выработана): B бьёт A — нет урона, метки у B нет.
    place(&mut net, 31.5, 33.5);
    net.sv.sim.players[b].zone_grace = 0;
    strike(&mut net, 1);
    assert_eq!(hp(&mut net, 0), 20.0, "A в зоне");
    assert_eq!(net.sv.sim.players[b].pvp_tag, 0, "удар по защищённому метку не ставит");

    // A (в зоне) бьёт B (снаружи, не защищён): B ранен; A получил метку — защиты нет.
    strike(&mut net, 0);
    assert_eq!(hp(&mut net, 1), 14.0, "A ударил вышедшего из зоны");
    assert!(net.sv.sim.players[a].pvp_tag > 0, "метка боя");

    // «Убежал в зону» — B бьёт A: проходит.
    place(&mut net, 31.5, 33.5); // после отбрасывания снова лицом к лицу (метка 10 с ещё держится)
    net.sv.sim.players[b].hp.time_until_regen = 0;
    strike(&mut net, 1);
    assert!(hp(&mut net, 0) <= 14.0 + 0.5, "ударил и убежал — не спасает: {}", hp(&mut net, 0));
    assert!(hp(&mut net, 0) < 20.0);

    // Через 10 с без ударов метки нет, A в зоне снова защищён.
    net.ticks(210, |_, _| {});
    assert_eq!((net.sv.sim.players[a].pvp_tag, net.sv.sim.players[b].pvp_tag), (0, 0));
    place(&mut net, 31.5, 33.5);
    let before = hp(&mut net, 0);
    strike(&mut net, 1);
    assert!(hp(&mut net, 0) >= before, "метка спала — зона снова защищает");

    // Выход из зоны: A на площадке за кольцом — ещё 10 с защиты, затем удар проходит.
    place(&mut net, 50.5, 52.5);
    net.sv.sim.players[b].zone_grace = 0;
    let before = hp(&mut net, 0);
    strike(&mut net, 1);
    assert!(hp(&mut net, 0) >= before, "после выхода ещё защищён");
    net.ticks(190, |_, _| {});
    let before = hp(&mut net, 0);
    strike(&mut net, 1);
    assert!(hp(&mut net, 0) < before - 5.0, "10 с прошли — бьётся: {} → {}", before, hp(&mut net, 0));
    assert_eq!(net.sv.sim.stats.teleports, 0);
}
