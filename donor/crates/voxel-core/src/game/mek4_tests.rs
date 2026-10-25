//! Mekanism M4 «Удвоение» через игру (клиент + локальный сервер, как в браузере) и по сети (два клиента): площадка mek:6
//! (сцена mek_double), окно сепаратора с кислородом и водородом, кнопка режима бака, линия ×2 до сундука без рук.

use super::*;
use crate::abi::ScreenKind;
use crate::cell::{block_of, state_of};
use crate::game::interact::Hit;
use crate::game::mek_demo::double_layout;
use crate::registry::blocks::{MekKind, ST_LIT};
use crate::registry::items::I_IRON_INGOT;
use crate::world::blockentity::BlockEntity;
use crate::world::mek::{GM_DUMP, GM_EXCESS};

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

/// Слитков железа в сундуке (сервер).
fn ingots(g: &Game, chest: [i32; 3]) -> i32 {
    match g.sim().world.d.borrow_mut().be_mut().get_mut(chest) {
        Some(BlockEntity::Chest(c)) => c.slots.iter().filter(|s| s.item == I_IRON_INGOT).map(|s| s.count).sum(),
        _ => -1,
    }
}

#[test]
fn double_bullet_separator_window_and_line() {
    // Пуля G12: сепаратор из воды даёт кислород и водород — видно в окне; «Готово, когда»: 10 сырого железа → 20 слитков в
    // сундуке без рук (сцена mek_double: энергия — куб по кабелю, кислород — сепаратор справа от очистителя).
    let (w, mut g) = scene();
    g.mek_build(6);
    g.tick();
    g.tick();
    let (sep, pur, cru, ch, sm, chest, _) = double_layout([0, 100, 0]);
    for (q, b) in [(sep, ELECTROLYTIC_SEPARATOR), (pur, PURIFICATION_CHAMBER), (cru, CRUSHER), (ch, ENRICHMENT_CHAMBER), (sm, ENERGIZED_SMELTER)] {
        assert_eq!(block_of(w.get_cell(q[0], q[1], q[2])), b, "у клиента {q:?}");
    }
    let hit = Hit { pos: sep, face: crate::cell::F_SOUTH, point: [sep[0] as f64 + 0.5, sep[1] as f64 + 0.5, sep[2] as f64 + 1.0], inside: false };
    g.interact(0, hit);
    for _ in 0..60 {
        g.tick();
    }
    assert_eq!(g.screen_kind(), ScreenKind::Machine as i32);
    assert_eq!(g.client.sim.screen_title(), "Электролитический сепаратор");
    let water = g.screen_tip(0);
    assert!(water.starts_with("Вода: ") && water.ends_with(" / 24 000 мБ"), "{water}");
    // Водород в «сбросе излишка» (сцена) — копится до 2 040; кислород уходит в очиститель (1 мБ/т — ровно его расход).
    assert_eq!(g.screen_tip(5), "H2: сброс излишка");
    assert_eq!(g.screen_tip(7), "O2: простой");
    let st = g.screen_tip(8);
    assert_eq!(st, "Даёт H2 2 и O2 1 мБ/т");
    let h2 = g.screen_tip(1);
    assert!(h2.starts_with("Водород: ") && h2.ends_with(" / 2 400 мБ"), "водород виден в окне: {h2}");
    assert_ne!(state_of(w.get_cell(sep[0], sep[1], sep[2])) & ST_LIT, 0, "сепаратор «активен» у клиента");
    // Один сепаратор ровно кормит один очиститель (1 мБ/т): очиститель работает, кислород не копится.
    let (pp, plast) = g.sim().world.d.borrow().be().mek(pur).map(|m| (m.progress, m.last)).unwrap();
    assert!(pp > 0 && plast == crate::world::mek::Step::Worked as u8, "очиститель работает на кислороде: P {pp}, шаг {plast}");
    // Кнопка режима водорода: сброс излишка → сброс (у сервера).
    g.screen_button(4);
    g.tick();
    g.tick();
    assert_eq!(g.sim().world.d.borrow().be().mek(sep).unwrap().gas_mode[0], GM_DUMP);
    assert_eq!(g.screen_tip(5), "H2: сброс");
    g.screen_button(4);
    g.screen_button(4);
    g.close_screen();
    // Линия без рук: 20 слитков в сундуке (~4 600 тиков от холодного старта).
    let mut t = 0;
    while ingots(&g, chest) < 20 && t < 5200 {
        g.tick();
        t += 1;
    }
    assert_eq!(ingots(&g, chest), 20, "через {t} тиков");
    assert_eq!(g.sim().world.d.borrow().be().mek(sep).unwrap().gas_mode[0], GM_EXCESS);
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
fn two_players_separator_window_and_mode() {
    // По сети: A строит площадку «удвоение», B открывает окно сепаратора — вода, водород, кислород и режимы приходят с
    // сервера (WinData); B жмёт кнопку режима кислорода — у сервера «сброс излишка»; очиститель у B «активен».
    let mut sv = ServerSim::new(World::new(1337));
    sv.debug = true;
    let (wa, wb) = (World::new(1337), World::new(1337));
    let mut net = Net { sv, cl: vec![(Client::new(&wa, "A"), None), (Client::new(&wb, "B"), None)] };
    net.pump();
    for _ in 0..3 {
        net.tick();
    }
    let ia = net.cl[0].1.unwrap();
    let ib = net.cl[1].1.unwrap();
    let a = net.sv.player_mut(ia).unwrap().player.pos;
    let feet = [a[0].floor() as i32, a[1].floor() as i32, a[2].floor() as i32];
    net.cl[0].0.sim.debug_build(6);
    for _ in 0..5 {
        net.tick();
    }
    let (sep, pur, ..) = double_layout(feet);
    assert_eq!(block_of(wb.get_cell(sep[0], sep[1], sep[2])), ELECTROLYTIC_SEPARATOR, "B видит сепаратор");
    net.cl[1].0.sim.teleport(sep[0] as f64 + 0.5, sep[1] as f64, sep[2] as f64 + 2.5);
    for _ in 0..5 {
        net.tick();
    }
    let i = net.sv.idx(ib).unwrap();
    net.sv.open_screen(i, crate::game::screen::Menu::mek(sep, MekKind::Separator));
    for _ in 0..300 {
        net.tick();
    }
    let b = &net.cl[1].0.sim;
    assert_eq!(b.screen_kind(), ScreenKind::Machine as i32);
    assert!(b.screen_tip(0).starts_with("Вода: "), "{}", b.screen_tip(0));
    assert_eq!(b.screen_tip(5), "H2: сброс излишка");
    assert!(b.screen_tip(1).starts_with("Водород: "), "водород у B: {}", b.screen_tip(1));
    assert!(b.screen_values()[2] > 0, "шкала водорода у B");
    assert_eq!(state_of(wb.get_cell(pur[0], pur[1], pur[2])) & ST_LIT, ST_LIT, "B видит работающий очиститель");
    net.cl[1].0.sim.screen_button(6);
    net.tick();
    net.tick();
    assert_eq!(net.sv.sim.world.d.borrow().be().mek(sep).unwrap().gas_mode[1], 1, "режим кислорода у сервера");
    assert_eq!(net.cl[1].0.sim.screen_tip(7), "O2: сброс излишка");
}
