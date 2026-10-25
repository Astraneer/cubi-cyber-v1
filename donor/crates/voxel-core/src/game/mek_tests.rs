//! Mekanism через игру (клиент + локальный сервер, как в браузере): площадки mek_build, окно машины, линия до стали.

use super::*;
use crate::abi::ScreenKind;
use crate::cell::{block_of, state_of};
use crate::game::interact::Hit;
use crate::game::mek_demo::layout;
use crate::registry::blocks::ST_LIT;
use crate::registry::items::I_ENRICHED_IRON;
use crate::world::mek::{INF_IN, INF_OUT};

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

#[test]
fn heat_generator_window_bullet() {
    // Пуля: генератор в лаве копит энергию — окно показывает её (значения — с сервера, WinData).
    let (w, mut g) = scene();
    g.mek_build(3);
    for _ in 0..3 {
        g.tick();
    }
    let feet = [0, 100, 0];
    let (gp, _, _) = layout(feet);
    assert_eq!(block_of(w.get_cell(gp[0], gp[1], gp[2])), HEAT_GENERATOR, "генератор у клиента");
    assert_eq!(block_of(w.get_cell(gp[0] - 1, gp[1], gp[2])), LAVA);
    for _ in 0..200 {
        g.tick();
    }
    let hit = Hit { pos: gp, face: crate::cell::F_SOUTH, point: [gp[0] as f64 + 0.5, gp[1] as f64 + 0.5, gp[2] as f64 + 1.0], inside: false };
    g.interact(0, hit);
    g.tick();
    g.tick();
    assert_eq!(g.screen_kind(), ScreenKind::Machine as i32);
    let v = g.screen_values();
    assert!(v[2] > 0, "полоса энергии растёт: {v:?}");
    let tip = g.screen_tip(1);
    assert!(tip.contains("Вырабатывает: 4") || tip.contains("Вырабатывает: 5"), "{tip}");
    assert!(g.screen_tip(3).ends_with(" K"), "{}", g.screen_tip(3));
    assert_eq!(state_of(w.get_cell(gp[0], gp[1], gp[2])) & ST_LIT, 0, "в лаве без топлива — не «активен»");
}

#[test]
fn line_makes_enriched_iron_and_glow_reaches_client() {
    let (w, mut g) = scene();
    g.mek_build(2);
    for _ in 0..420 {
        g.tick();
    }
    let (_, c0, inf) = layout([0, 100, 0]);
    let s = g.sim();
    let wd = s.world.d.borrow();
    let m = wd.be().mek(inf).unwrap();
    assert_eq!(m.slots[INF_OUT].item, I_ENRICHED_IRON, "{:?}", m.slots);
    assert!(m.slots[INF_IN].n() < 6);
    drop(wd);
    // Клиент знает свечение кабеля (Msg::NetGlow) и рисует его в модели.
    let draws = g.model_draws(0.0, 4);
    let cable = draws.chunks(6).find(|d| d[0] as i32 == c0[0] && d[1] as i32 == c0[1] && d[2] as i32 == c0[2]).expect("кабель в моделях");
    assert!((cable[5] as u32) >> 6 >= 900, "кабель светится: {}", cable[5] as u32 >> 6);
    let _ = w;
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
fn two_players_see_line_and_window() {
    // Готово, когда: «работает вдвоём по сети» — A строит линию (DebugCmd::Build), B видит кабели со свечением, открывает
    // окно инфузора (значения — Msg::WinData) и жмёт «сброс» — бак пуст у сервера.
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
    net.cl[0].0.sim.debug_build(2);
    for _ in 0..300 {
        net.tick();
    }
    let (g, c0, inf) = layout(feet);
    assert_eq!(block_of(wb.get_cell(g[0], g[1], g[2])), HEAT_GENERATOR, "B видит генератор");
    assert_eq!(block_of(wb.get_cell(c0[0] + 5, c0[1], c0[2])), UNIVERSAL_CABLE, "и кабель");
    let glow = net.cl[1].0.sim.cable_glow.get(&c0).copied().unwrap_or(0);
    assert!(glow >= 900, "свечение сети у B: {glow}");
    // B подходит к инфузору (окно держится ближе 8.5) и открывает его (решение сервера — как по ПКМ).
    net.cl[1].0.sim.teleport(inf[0] as f64 - 1.5, inf[1] as f64, inf[2] as f64 + 2.5);
    for _ in 0..5 {
        net.tick();
    }
    let i = net.sv.idx(ib).unwrap();
    net.sv.open_screen(i, crate::game::screen::Menu::mek(inf, crate::registry::blocks::MekKind::Infuser));
    net.tick();
    net.tick();
    assert_eq!(net.cl[1].0.sim.screen_kind(), ScreenKind::Machine as i32);
    let tip = net.cl[1].0.sim.screen_tip(0);
    assert!(tip.starts_with("Углерод:"), "бак инфузора у B: {tip}");
    let e = net.cl[1].0.sim.screen_tip(1);
    assert!(e.contains("кДж") || e.contains("Дж"), "{e}");
    net.cl[1].0.sim.screen_button(3);
    net.tick();
    let tank = net.sv.sim.world.d.borrow().be().mek(inf).unwrap().tank;
    assert!(tank.amount <= 10, "сброс у сервера: {tank:?}");
}

#[test]
fn enrich_scene_chamber_then_smelter() {
    // G9 «Готово, когда»: площадка «обогащение» (сцена mek_enrich): 9 сырого железа в камере → 12 пыли за 600 тиков от
    // куба через кабель; окно камеры у клиента показывает прогресс; пыль в плавильню → 12 слитков без топлива.
    use crate::game::mek_demo::enrich_layout;
    use crate::registry::items::{I_DUST_IRON, I_IRON_INGOT};
    use crate::world::mek::{CH_IN, CH_OUT};
    let (w, mut g) = scene();
    g.mek_build(4);
    g.tick();
    g.tick();
    let (ch, cable, sm, cube) = enrich_layout([0, 100, 0]);
    assert_eq!(block_of(w.get_cell(ch[0], ch[1], ch[2])), ENRICHMENT_CHAMBER, "камера у клиента");
    assert_eq!(block_of(w.get_cell(sm[0], sm[1], sm[2])), ENERGIZED_SMELTER);
    assert_eq!(block_of(w.get_cell(cube[0], cube[1], cube[2])), ENERGY_CUBE);
    assert_eq!(block_of(w.get_cell(cable[0], cable[1], cable[2])), UNIVERSAL_CABLE);
    let hit = Hit { pos: ch, face: crate::cell::F_SOUTH, point: [ch[0] as f64 + 0.5, ch[1] as f64 + 0.5, ch[2] as f64 + 1.0], inside: false };
    g.interact(0, hit);
    for _ in 0..100 {
        g.tick();
    }
    assert_eq!(g.screen_kind(), ScreenKind::Machine as i32);
    let v = g.screen_values();
    assert!(v[2] > 0, "прогресс в окне камеры: {v:?}");
    assert_eq!(state_of(w.get_cell(ch[0], ch[1], ch[2])) & ST_LIT, ST_LIT, "камера «активна» у клиента");
    for _ in 0..520 {
        g.tick();
    }
    {
        let s = g.sim();
        let mut wd = s.world.d.borrow_mut();
        let m = wd.be().mek(ch).unwrap();
        assert_eq!((m.slots[CH_OUT].item, m.slots[CH_OUT].count, m.slots[CH_IN].n()), (I_DUST_IRON, 12, 0), "{:?}", m.slots);
        // Пыль — в плавильню (руками; выталкивание — M3).
        let dust = std::mem::take(&mut wd.be_mut().mek_mut(ch).unwrap().slots[CH_OUT]);
        wd.be_mut().mek_mut(sm).unwrap().slots[CH_IN] = dust;
    }
    for _ in 0..2410 {
        g.tick();
    }
    let s = g.sim();
    let wd = s.world.d.borrow();
    let m = wd.be().mek(sm).unwrap();
    assert_eq!((m.slots[CH_OUT].item, m.slots[CH_OUT].count), (I_IRON_INGOT, 12), "{:?}", m.slots);
}

#[test]
fn two_players_chamber_window() {
    // По сети: A строит площадку «обогащение», B открывает окно камеры — прогресс и энергия приходят с сервера (WinData).
    use crate::game::mek_demo::enrich_layout;
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
    net.cl[0].0.sim.debug_build(4);
    for _ in 0..5 {
        net.tick();
    }
    let (ch, _, _, _) = enrich_layout(feet);
    assert_eq!(block_of(wb.get_cell(ch[0], ch[1], ch[2])), ENRICHMENT_CHAMBER, "B видит камеру");
    net.cl[1].0.sim.teleport(ch[0] as f64 + 0.5, ch[1] as f64, ch[2] as f64 + 2.5);
    for _ in 0..5 {
        net.tick();
    }
    let i = net.sv.idx(ib).unwrap();
    net.sv.open_screen(i, crate::game::screen::Menu::mek(ch, crate::registry::blocks::MekKind::Enrich));
    for _ in 0..100 {
        net.tick();
    }
    assert_eq!(net.cl[1].0.sim.screen_kind(), ScreenKind::Machine as i32);
    assert_eq!(net.cl[1].0.sim.screen_title(), "Обогатительная камера");
    let v = net.cl[1].0.sim.screen_values();
    assert!(v[0] > 0 && v[2] > 0, "энергия и прогресс у B: {v:?}");
    assert!(net.cl[1].0.sim.screen_tip(0).contains("Использует: 50 Дж/т"), "{}", net.cl[1].0.sim.screen_tip(0));
    assert_eq!(state_of(wb.get_cell(ch[0], ch[1], ch[2])) & ST_LIT, ST_LIT, "B видит «активна»");
}

#[test]
fn broken_machine_keeps_energy_and_contents() {
    // G9 «Готово, когда»: сломанная машина сохраняет энергию и содержимое (решение после MEK-01): игрок ломает работающую
    // камеру стальной киркой — выпадает одна камера (вход и выход не рассыпаются), с номером данных в damage; подобрал,
    // поставил — энергия и слоты те же, запись в хранилище освобождена. Пустая машина — обычный складываемый предмет.
    use crate::abi::{K_ATTACK, K_LOCKED};
    use crate::game::mek_demo::enrich_layout;
    use crate::registry::items::{I_DUST_IRON, I_RAW_IRON, I_STEEL_PICKAXE};
    use crate::world::mek::{CH_IN, CH_OUT};
    let (w, mut g) = scene();
    g.mek_build(4);
    for _ in 0..230 {
        g.tick();
    }
    let (ch, _, _, _) = enrich_layout([0, 100, 0]);
    g.clear_inventory();
    g.give(I_STEEL_PICKAXE, 1);
    g.set_look(161.6, 19.5);
    g.tick();
    g.press(crate::abi::Press::Attack as u32);
    let mut before = None;
    for _ in 0..60 {
        g.set_keys(K_LOCKED | K_ATTACK);
        if let Some(m) = g.sim().world.d.borrow().be().mek(ch) {
            before = Some((m.energy, m.slots));
        }
        g.tick();
        if block_of(g.sim().world.d.borrow_mut().get_cell(ch[0], ch[1], ch[2])) == AIR {
            break;
        }
    }
    g.set_keys(K_LOCKED);
    g.tick();
    assert_eq!(block_of(w.get_cell(ch[0], ch[1], ch[2])), AIR, "камера сломана");
    let (energy, slots) = before.unwrap();
    assert!(energy > 0 && slots[CH_IN].item == I_RAW_IRON && slots[CH_OUT].item == I_DUST_IRON, "{energy} {slots:?}");
    let drops: Vec<ItemStack> = g.sim().items.list.iter().filter(|e| !e.removed).map(|e| e.stack).collect();
    assert_eq!(drops.len(), 1, "выпала только сама машина: {drops:?}");
    assert_eq!((drops[0].item, drops[0].count), (ENRICHMENT_CHAMBER as ItemId, 1));
    let id = drops[0].damage;
    assert!(id != 0 && g.sim().mek_items.get(id).is_some(), "данные в хранилище: {id}");
    // Подобрать: встать на предмет.
    g.teleport(ch[0] as f64 + 0.5, ch[1] as f64, ch[2] as f64 + 0.5);
    for _ in 0..20 {
        g.tick();
    }
    let inv = &g.me().inv;
    let k = (0..9).find(|&i| inv.get(i).item == ENRICHMENT_CHAMBER as ItemId).expect("камера в хотбаре");
    assert_eq!(inv.get(k).damage, id, "номер данных доехал до инвентаря");
    g.teleport(0.5, 100.0, 0.5);
    g.press(crate::abi::Press::Hotbar as u32 + k as u32);
    for _ in 0..3 {
        g.tick();
    }
    assert_eq!(g.me().inv.selected, k);
    let floor = [ch[0], ch[1] - 1, ch[2]];
    let hit = Hit { pos: floor, face: crate::cell::F_UP, point: [floor[0] as f64 + 0.5, floor[1] as f64 + 1.0, floor[2] as f64 + 0.5], inside: false };
    g.interact(0, hit);
    g.tick();
    assert_eq!(block_of(w.get_cell(ch[0], ch[1], ch[2])), ENRICHMENT_CHAMBER, "поставлена");
    let wd = g.sim().world.d.borrow();
    let m = wd.be().mek(ch).unwrap();
    assert_eq!(m.slots[..3], slots[..3], "слоты те же");
    assert!(m.energy > 0 && m.energy <= energy, "энергия та же (минус работа после ставления): {} / {energy}", m.energy);
    drop(wd);
    assert!(g.sim().mek_items.get(id).is_none(), "запись освобождена");
}

#[test]
fn mek_items_store() {
    use crate::registry::blocks::MekKind;
    use crate::world::mek::items::MekItems;
    use crate::world::mek::MekBe;
    let mut st = MekItems::default();
    let empty = MekBe::new(MekKind::Smelter);
    assert_eq!(st.stash(&empty), 0, "пустая машина — без данных");
    let mut m = MekBe::new(MekKind::Cube);
    m.energy = 1_234_567;
    let a = st.stash(&m);
    let b = st.stash(&m);
    assert!(a != 0 && b != 0 && a != b);
    let mut n = MekBe::new(MekKind::Cube);
    st.take(a).unwrap().apply(&mut n);
    assert_eq!(n.energy, 1_234_567);
    assert!(st.take(a).is_none() && st.get(b).is_some());
}
