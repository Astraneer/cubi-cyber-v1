//! Mekanism M3 через игру (клиент + локальный сервер, как в браузере): ветряк — площадка mek:5, окно через часть мачты,
//! ставление многоблока предметом, ломание любой части.

use super::*;
use crate::abi::{ScreenKind, K_ATTACK, K_LOCKED};
use crate::cell::{block_of, state_of, F_UP};
use crate::game::interact::Hit;
use crate::game::mek_demo::wind_layout;
use crate::registry::items::I_STEEL_PICKAXE;
use crate::world::mek::wind_power;

fn scene() -> (World, Game) {
    let w = World::new(1337);
    for x in -8..=16 {
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

/// Взгляд (градусы Minecraft) из глаз игрока на точку t.
fn look_at(g: &mut Game, t: [f64; 3]) {
    let p = g.me().player.pos;
    let eye = [p[0], p[1] + g.me().player.eye_height() as f64, p[2]];
    let d = [t[0] - eye[0], t[1] - eye[1], t[2] - eye[2]];
    let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    let yaw = (-d[0]).atan2(d[2]).to_degrees();
    let pitch = -(d[1] / len).asin().to_degrees();
    g.set_look(yaw as f32, pitch as f32);
}

#[test]
fn wind_bullet_window_through_mast() {
    // Пуля G11: ветряк стоит и копит энергию, выработка по высоте видна в окне (ПКМ по мачте открывает окно основания).
    let (w, mut g) = scene();
    g.mek_build(5);
    for _ in 0..60 {
        g.tick();
    }
    let (hill, sea) = wind_layout([0, 100, 0]);
    assert_eq!(block_of(w.get_cell(hill[0], hill[1], hill[2])), WIND_GENERATOR, "ветряк у клиента");
    assert_eq!(block_of(w.get_cell(hill[0], hill[1] + 4, hill[2])), BOUNDING);
    assert_eq!(block_of(w.get_cell(sea[0], sea[1], sea[2])), WIND_GENERATOR, "ветряк у моря");
    let part = [hill[0], hill[1] + 2, hill[2]];
    let hit = Hit { pos: part, face: crate::cell::F_SOUTH, point: [part[0] as f64 + 0.5, part[1] as f64 + 0.5, part[2] as f64 + 0.75], inside: false };
    g.interact(0, hit);
    g.tick();
    g.tick();
    assert_eq!(g.screen_kind(), ScreenKind::Machine as i32);
    assert_eq!(g.client.sim.screen_title(), "Ветряной генератор");
    let v = g.screen_values();
    assert!(v[0] > 0, "полоса энергии растёт: {v:?}");
    assert_eq!(g.screen_tip(1), format!("Выработка: {} Дж/т", wind_power(100)));
    assert_ne!(state_of(w.get_cell(hill[0], hill[1], hill[2])) & crate::registry::blocks::ST_LIT, 0, "крутится");
    // Сервер: у моря меньше, чем на горе (178 против 386).
    let s = g.sim();
    let wd = s.world.d.borrow();
    assert_eq!((wd.be().mek(sea).unwrap().produced, wd.be().mek(hill).unwrap().produced), (178, 386));
}

#[test]
fn wind_place_and_break_any_part() {
    // Ставление предметом: 4 клетки над основанием должны быть свободны; ломание мачты киркой — дроп один ветряк.
    let (w, mut g) = scene();
    g.clear_inventory();
    g.give(WIND_GENERATOR as ItemId, 2);
    g.tick();
    let floor = [0, 99, -2];
    let hit = |f: [i32; 3]| Hit { pos: f, face: F_UP, point: [f[0] as f64 + 0.5, f[1] as f64 + 1.0, f[2] as f64 + 0.5], inside: false };
    g.interact(0, hit(floor));
    g.tick();
    g.tick();
    let b = [0, 100, -2];
    assert_eq!(block_of(w.get_cell(b[0], b[1], b[2])), WIND_GENERATOR);
    for dy in 1..=4 {
        assert_eq!(block_of(w.get_cell(b[0], b[1] + dy, b[2])), BOUNDING, "часть {dy}");
    }
    // Под потолком (камень на высоте +3) — не ставится вовсе.
    w.set_block(3, 103, -2, STONE);
    g.sim().world.d.borrow_mut().set_cell(3, 103, -2, STONE as u32);
    g.interact(0, hit([3, 99, -2]));
    g.tick();
    g.tick();
    assert_eq!(block_of(w.get_cell(3, 100, -2)), AIR, "под потолком не ставится");
    assert_eq!(g.me().inv.hand().count, 1, "предмет не потрачен");
    // Ломаю верх мачты стальной киркой — ветряк уходит целиком, выпадает один предмет.
    g.clear_inventory();
    g.give(I_STEEL_PICKAXE, 1);
    let top = [b[0], b[1] + 3, b[2]];
    look_at(&mut g, [top[0] as f64 + 0.5, top[1] as f64 + 0.5, top[2] as f64 + 0.5]);
    g.tick();
    g.press(crate::abi::Press::Attack as u32);
    for _ in 0..80 {
        g.set_keys(K_LOCKED | K_ATTACK);
        g.tick();
        if block_of(g.sim().world.d.borrow_mut().get_cell(b[0], b[1], b[2])) == AIR {
            break;
        }
    }
    g.set_keys(K_LOCKED);
    g.tick();
    g.tick();
    for dy in 0..=4 {
        assert_eq!(block_of(w.get_cell(b[0], b[1] + dy, b[2])), AIR, "клиент: клетка {dy}");
        assert_eq!(block_of(g.sim().world.d.borrow_mut().get_cell(b[0], b[1] + dy, b[2])), AIR, "сервер: клетка {dy}");
    }
    let drops: Vec<ItemStack> = g.sim().items.list.iter().filter(|e| !e.removed).map(|e| e.stack).collect();
    assert_eq!(drops.len(), 1, "{drops:?}");
    assert_eq!(drops[0].item, WIND_GENERATOR as ItemId);
}

// ---------------------------------------------------------------- часть 2: склад, конфигуратор, улучшения

use crate::abi::K_SNEAK;
use crate::game::mek_demo::store_layout;
use crate::registry::items::{I_CONFIGURATOR, I_IRON_INGOT, I_UPGRADE_SPEED};
use crate::world::blockentity::BlockEntity;
use crate::world::mek::sides::{rel_of, S_IO, S_OUT};
use crate::world::mek::{CH_IN, CH_OUT, UP_SPEED};

fn chest_count(g: &Game, p: [i32; 3], item: ItemId) -> i32 {
    match g.sim().world.d.borrow().be().get(p) {
        Some(BlockEntity::Chest(c)) => c.slots.iter().filter(|s| s.item == item).map(|s| s.n()).sum(),
        _ => -1,
    }
}

#[test]
fn store_line_dust_to_smelter_ingots_to_chest() {
    // G11 «Готово, когда»: камера и плавильня вплотную с выталкиванием — пыль сама идёт в плавильню, слитки — в сундук;
    // у камеры 4 улучшения скорости: 9 сырого железа → 12 пыли за 3 × 63 тика.
    let (_w, mut g) = scene();
    g.mek_build(5);
    let (ch, sm, chest, _) = store_layout([0, 100, 0]);
    let mut dust_done = 0;
    for t in 1..=1300 {
        g.tick();
        let wd = g.sim().world.d.borrow();
        let m = wd.be().mek(ch).unwrap();
        if dust_done == 0 && m.slots[CH_IN].is_empty() && m.slots[CH_OUT].is_empty() {
            dust_done = t;
        }
    }
    assert!(dust_done > 0 && dust_done < 3 * 63 + 40, "камера отработала 9 руды за ~190 тиков: {dust_done}");
    let ingots = chest_count(&g, chest, I_IRON_INGOT);
    let wd = g.sim().world.d.borrow();
    let smelter = wd.be().mek(sm).unwrap();
    let in_line = smelter.slots[CH_IN].n() + smelter.slots[CH_OUT].n();
    assert!(ingots >= 5, "слитки в сундуке: {ingots}");
    assert!(ingots + in_line >= 11 && ingots + in_line <= 12, "все 12 — в плавильне или сундуке (одна может плавиться): {ingots} + {in_line}");
}

#[test]
fn configurator_sides_mode_wrench_upgrade() {
    let (w, mut g) = scene();
    g.mek_build(5);
    for _ in 0..3 {
        g.tick();
    }
    let (ch, _, _, _) = store_layout([0, 100, 0]);
    g.clear_inventory();
    g.give(I_CONFIGURATOR, 1);
    g.give(I_UPGRADE_SPEED, 16);
    g.tick();
    // Клиент знает стороны машин рядом (Msg::MekSides): подсветка — по 6 граней на машину.
    for _ in 0..3 {
        g.tick();
    }
    let ov = g.mek_sides_overlay();
    assert!(ov.len() >= 2 * 6 * 5 && ov.len() % 30 == 0, "подсветка граней: {}", ov.len());
    // Shift+ПКМ по верхней грани камеры — режим «вход» → «выход» (у сервера и, после Msg::MekSides, у клиента).
    g.set_keys(K_LOCKED | K_SNEAK);
    g.tick();
    g.tick();
    let hit = Hit { pos: ch, face: F_UP, point: [ch[0] as f64 + 0.5, ch[1] as f64 + 1.0, ch[2] as f64 + 0.5], inside: false };
    g.interact(0, hit);
    for _ in 0..3 {
        g.tick();
    }
    let rel = rel_of(crate::cell::F_SOUTH, F_UP);
    assert_eq!(g.sim().world.d.borrow().be().mek(ch).unwrap().sides[rel], S_OUT, "сервер");
    assert_eq!(w.d.borrow().be().mek(ch).unwrap().sides[rel], S_OUT, "клиент");
    g.interact(0, hit);
    g.tick();
    assert_eq!(g.sim().world.d.borrow().be().mek(ch).unwrap().sides[rel], S_IO);
    // Улучшение скорости в руке, Shift+ПКМ — сразу до 8 (было 4 — ставится 4), в руке 12.
    g.press(crate::abi::Press::Hotbar as u32 + 1);
    g.tick();
    g.interact(0, hit);
    g.tick();
    g.tick();
    assert_eq!(g.sim().world.d.borrow().be().mek(ch).unwrap().upgrades[UP_SPEED], 8);
    assert_eq!(g.me().inv.get(1).n(), 12);
    // Клавиша N: режим конфигуратора вперёд (у клиента и сервера) — до «ключа» три нажатия.
    g.press(crate::abi::Press::Hotbar as u32);
    g.tick();
    for _ in 0..3 {
        g.press(crate::abi::Press::Mode as u32);
        g.tick();
    }
    g.tick();
    assert_eq!(g.me().inv.hand().damage, crate::game::mek_tool::CONF_WRENCH);
    assert_eq!(g.client.sim.view.overlay_msg, crate::abi::MSG_CONF + 3);
    let sp = g.server.sim.players[0].inv.hand().damage;
    assert_eq!(sp, crate::game::mek_tool::CONF_WRENCH, "сервер знает режим");
    // Ключ: Shift+ПКМ — камера в инвентаре с данными (руда, улучшения, стороны).
    g.interact(0, hit);
    for _ in 0..3 {
        g.tick();
    }
    assert_eq!(block_of(w.get_cell(ch[0], ch[1], ch[2])), AIR);
    let inv = &g.me().inv;
    let k = (0..36).find(|&i| inv.get(i).item == ENRICHMENT_CHAMBER as ItemId).expect("камера в инвентаре");
    let id = inv.get(k).damage;
    let d = g.sim().mek_items.get(id).cloned().expect("данные камеры");
    assert_eq!((d.upgrades[UP_SPEED], d.sides[rel]), (8, S_IO));
}
