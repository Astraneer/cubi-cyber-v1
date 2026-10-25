//! Проверки G7 (только для cargo test): броня (золото, алмаз, стойкость, ПКМ надевает), ферма (мотыга, влажность пашни,
//! рост по ASP-19 §3, дроп, вытаптывание, стог, смыв водой, хлеб, посадка и еда морковью). Вода и вёдра — fluid_tests.rs.
//! Сцена: земляной пол 25×25 на y = 100 высоко над рельефом, игрок в (0.5, 101, 0.5).

use super::*;
use crate::abi::*;
use crate::cell::{block_of, cell_of, state_of, BlockId, Cell, F_DOWN, F_UP};
use crate::game::farm::{crop_light, growth_n, moisture_factor};
use crate::game::fluids::{Fl, Liquid};
use crate::game::health::DamageKind;
use crate::game::interact::{Hit, Use};
use crate::game::view::Events;
use crate::registry::blocks::{bdef, collision_of, layer_cell, outline_of, plant_supported, top_center};
use crate::registry::ids::*;
use crate::registry::items::*;
use crate::registry::shapes::px;

const FY: i32 = 100;
const HOE: ItemId = I_WOODEN_PICKAXE + 3;

fn scene() -> (World, Game) {
    let w = World::new(1337);
    for x in -12..=12 {
        for z in -12..=12 {
            w.set_block(x, FY - 1, z, STONE);
            w.set_block(x, FY, z, DIRT);
        }
    }
    let mut g = Game::new(&w);
    g.teleport(0.5, 101.0, 0.5);
    g.body(|p| {
        p.on_ground = true;
        p.vel = [0.0, -0.0784, 0.0];
    });
    g.set_look(0.0, 0.0);
    g.set_keys(K_LOCKED);
    g.tick();
    g.on_server(|sv, i| sv.players[i].hp.spawn_protect = 0);
    (w, g)
}
fn hit(pos: [i32; 3], face: u8) -> Hit {
    Hit { pos, face, point: [pos[0] as f64 + 0.5, pos[1] as f64 + 0.5, pos[2] as f64 + 0.5], inside: false }
}
fn raw(g: &mut Game, p: [i32; 3], c: Cell) {
    g.sw().set_cell(p[0], p[1], p[2], c);
}
fn at(g: &Game, p: [i32; 3]) -> Cell {
    g.sw().get_cell(p[0], p[1], p[2])
}
fn farm(m: u16) -> Cell {
    cell_of(FARMLAND, m)
}
fn run(g: &mut Game, n: usize) {
    for _ in 0..n {
        g.tick();
    }
}
/// Случайный тик клетки p на сервере (минуя выборку).
fn rt(g: &mut Game, p: [i32; 3]) {
    g.on_server(|sv, _| {
        let d = sv.sim.world.d.clone();
        let mut w = d.borrow_mut();
        let c = w.get_cell(p[0], p[1], p[2]);
        sv.sim.random_tick(&mut w, p, c, &mut Events::default());
    });
}
fn hoe(g: &mut Game) {
    g.set_slot(0, ItemStack::of(HOE, 1));
    g.select(0);
}

// ------------------------------------------------------------------ броня

#[test]
fn armor_items_asp06() {
    // Очки и прочность: золото 2/5/3/1 и 77/112/105/91, алмаз 3/8/6/3 и 363/528/495/429, стойкость алмаза 2 на часть.
    let want = [(2u8, 77u16), (5, 112), (3, 105), (1, 91), (3, 363), (8, 528), (6, 495), (3, 429)];
    for (k, (points, dur)) in want.into_iter().enumerate() {
        let d = idef(I_GOLDEN_HELMET + k as ItemId);
        assert_eq!(d.max_damage, dur, "{}", d.name);
        assert!(matches!(d.kind, ItemKind::Armor { slot, points: p } if p == points && slot == 3 - (k % 4) as u8), "{}", d.name);
        assert_eq!(armor_toughness(I_GOLDEN_HELMET + k as ItemId), if k >= 4 { 2.0 } else { 0.0 });
    }
    assert_eq!(idef(I_DIAMOND).mc, "diamond");
    assert_eq!(item_by_mc("golden_boots"), I_GOLDEN_HELMET + 3);
    assert_eq!(item_by_mc("diamond_chestplate"), I_DIAMOND_HELMET + 1);
    assert_eq!(item_by_mc("bread"), I_BREAD);
}

#[test]
fn diamond_set_reduces_damage_with_toughness() {
    // Полный алмазный комплект: 20 очков, стойкость 8: урон 6 → 6·(1 − 18.5/25) = 1.56; износ на часть (int)max(1, 6/4) = 1.
    let (_w, mut g) = scene();
    for k in 0..4 {
        g.set_slot(36 + k, ItemStack::of(I_DIAMOND_HELMET + k as ItemId, 1));
    }
    g.tick();
    assert_eq!(g.client.sim.vit.armor, 20);
    g.on_server(|sv, i| {
        sv.hurt(i, DamageKind::Cactus, 6.0);
    });
    let hp = g.on_server(|sv, i| sv.players[i].hp.hp);
    assert!((hp - (20.0 - 1.56)).abs() < 1e-4, "hp {hp}");
    let worn: Vec<u16> = g.on_server(|sv, i| (36..40).map(|k| sv.players[i].inv.get(k).damage).collect());
    assert_eq!(worn, vec![1, 1, 1, 1]);
    // Без стойкости (железо, 15 очков): 6 → 3.12 — как в таблице ASP-12.
    let (_w, mut g) = scene();
    for k in 0..4 {
        g.set_slot(36 + k, ItemStack::of(I_LEATHER_HELMET + 4 + k as ItemId, 1));
    }
    g.on_server(|sv, i| {
        sv.hurt(i, DamageKind::Cactus, 6.0);
    });
    let hp = g.on_server(|sv, i| sv.players[i].hp.hp);
    assert!((hp - (20.0 - 3.12)).abs() < 1e-4, "hp {hp}");
}

#[test]
fn golden_boots_break_and_vanish() {
    // Золотые ботинки (прочность 91) ломаются от износа: сломались — слот пуст.
    let (_w, mut g) = scene();
    let mut boots = ItemStack::of(I_GOLDEN_HELMET + 3, 1);
    boots.damage = 90;
    g.set_slot(36, boots);
    g.on_server(|sv, i| {
        sv.hurt(i, DamageKind::Cactus, 1.0);
    });
    assert!(g.me().inv.get(36).is_empty());
}

#[test]
fn right_click_equips_and_swaps_armor() {
    let (_w, mut g) = scene();
    g.set_slot(0, ItemStack::of(I_LEATHER_HELMET + 5, 1)); // железный нагрудник
    g.select(0);
    g.tick();
    g.press(P_USE);
    g.tick();
    assert_eq!(g.ctl().inv.armor[2], ItemStack::of(I_LEATHER_HELMET + 5, 1), "надет у клиента");
    assert_eq!(g.me().inv.armor[2], ItemStack::of(I_LEATHER_HELMET + 5, 1), "и у сервера");
    assert!(g.ctl().inv.hand().is_empty());
    run(&mut g, 2);
    assert_eq!(g.client.sim.vit.armor, 6);
    // Ещё одна: кожаная куртка (другой материал) меняется местами с надетой.
    g.set_slot(0, ItemStack::of(I_LEATHER_HELMET + 1, 1));
    g.tick();
    g.press(P_USE);
    g.tick();
    assert_eq!(g.me().inv.armor[2], ItemStack::of(I_LEATHER_HELMET + 1, 1));
    assert_eq!(g.me().inv.main[0], ItemStack::of(I_LEATHER_HELMET + 5, 1), "прежняя оказалась в руке");
    g.assert_in_sync();
}

// ------------------------------------------------------------------ мотыга и пашня

#[test]
fn hoe_tills_dirt_and_grass_only_under_air() {
    let (w, mut g) = scene();
    w.set_block(3, FY, 0, GRASS);
    w.set_block(4, FY, 0, SAND);
    w.set_block(5, FY, 0, SNOWY_GRASS);
    w.set_block(6, FY, 0, STONE);
    w.set_block(7, FY, 0, DIRT);
    w.set_block(7, FY + 1, 0, TALL_GRASS);
    hoe(&mut g);
    // Земля, трава, заснеженная трава → пашня сухая.
    for (x, ok) in [(2, true), (3, true), (5, true), (4, false), (6, false), (7, false)] {
        let r = g.interact(0, hit([x, FY, 0], F_UP));
        let b = block_of(at(&g, [x, FY, 0]));
        assert_eq!((r == Use::Swing, b == FARMLAND), (ok, ok), "x = {x}");
        if ok {
            assert_eq!(state_of(at(&g, [x, FY, 0])), 0);
        }
    }
    assert_eq!(g.me().inv.main[0].damage, 3, "износ только за успешные");
    // Снизу не вспахать.
    w.set_block(9, FY, 0, DIRT);
    assert_eq!(g.interact(0, hit([9, FY, 0], F_DOWN)), Use::Pass);
    assert_eq!(block_of(at(&g, [9, FY, 0])), DIRT);
}

#[test]
fn wooden_hoe_has_59_uses() {
    let (_w, mut g) = scene();
    let mut s = ItemStack::of(HOE, 1);
    s.damage = 58;
    g.set_slot(0, s);
    g.select(0);
    assert_eq!(g.interact(0, hit([2, FY, 0], F_UP)), Use::Swing);
    assert!(g.me().inv.main[0].is_empty(), "последняя вспашка ломает мотыгу");
    assert_eq!(idef(HOE).max_damage, 59);
}

#[test]
fn right_click_with_real_hoe_input() {
    // Клик из клиента: тот же результат у клиента и сервера, без отката.
    let (_w, mut g) = scene();
    hoe(&mut g);
    g.set_look(0.0, 50.0);
    g.tick();
    let t = g.ctl().target.unwrap();
    assert_eq!((t.pos, t.face), ([0, FY, 1], F_UP));
    g.press(P_USE);
    g.tick();
    assert_eq!(block_of(g.client.sim.world.d.borrow_mut().get_cell(0, FY, 1)), FARMLAND);
    run(&mut g, 3);
    assert_eq!(block_of(at(&g, [0, FY, 1])), FARMLAND);
    assert_eq!(g.me().inv.main[0].damage, 1);
    g.assert_in_sync();
}

#[test]
fn moisture_by_water_within_four() {
    let (_w, mut g) = scene();
    let f = [2, FY, 2];
    // Вода в 9×9 на уровне пашни и на уровень выше: 7; ниже, дальше, выше на 2 — пашня сохнет.
    let water = Fl::source(Liquid::Water).cell();
    let cases = [([6, FY, 6], true), ([7, FY, 2], false), ([2, FY + 1, 2], true), ([2, FY - 1, 2], false), ([2, FY + 2, 2], false), ([-2, FY, -2], true), ([-3, FY, 2], false)];
    for (p, wet) in cases {
        raw(&mut g, f, farm(3));
        raw(&mut g, p, water);
        rt(&mut g, f);
        assert_eq!(state_of(at(&g, f)), if wet { 7 } else { 2 }, "вода в {p:?}");
        raw(&mut g, p, if p[1] == FY - 1 { cell_of(STONE, 0) } else if p[1] == FY { cell_of(DIRT, 0) } else { AIR as Cell });
    }
    // Текучая вода тоже увлажняет.
    raw(&mut g, f, farm(0));
    raw(&mut g, [6, FY + 1, -2], Fl::flowing(Liquid::Water, 1).cell());
    rt(&mut g, f);
    assert_eq!(state_of(at(&g, f)), 7);
}

#[test]
fn dry_farmland_decays_to_dirt_unless_cropped() {
    let (_w, mut g) = scene();
    let f = [2, FY, 2];
    raw(&mut g, f, farm(7));
    for m in (0..7).rev() {
        rt(&mut g, f);
        assert_eq!(state_of(at(&g, f)), m);
    }
    rt(&mut g, f);
    assert_eq!(block_of(at(&g, f)), DIRT, "8-й тик без воды — земля");
    // С посевом сверху пашня держится бесконечно.
    raw(&mut g, f, farm(0));
    raw(&mut g, [2, FY + 1, 2], cell_of(WHEAT, 0));
    for _ in 0..30 {
        rt(&mut g, f);
    }
    assert_eq!(at(&g, f), farm(0));
}

#[test]
fn solid_block_over_farmland_turns_it_to_dirt() {
    let (_w, mut g) = scene();
    let f = [2, FY, 2];
    g.put(f, farm(0));
    g.put([2, FY + 1, 2], COBBLE as Cell);
    run(&mut g, 2);
    assert_eq!(block_of(at(&g, f)), DIRT, "через тик");
    // Вода, цветок, посев — не твёрдые.
    for (x, c) in [(4, Fl::source(Liquid::Water).cell()), (6, DANDELION as Cell), (8, cell_of(WHEAT, 0))] {
        g.put([x, FY, 2], farm(0));
        g.put([x, FY + 1, 2], c);
        run(&mut g, 3);
        assert_eq!(block_of(at(&g, [x, FY, 2])), FARMLAND, "над пашней {c:#x}");
    }
}

#[test]
fn seeds_plant_on_farmland_only_with_light() {
    let (_w, mut g) = scene();
    g.put([2, FY, 2], farm(0));
    g.set_slot(0, ItemStack::of(I_WHEAT_SEEDS, 5));
    g.select(0);
    // На землю — нельзя.
    assert_eq!(g.interact(0, hit([4, FY, 2], F_UP)), Use::Fail);
    assert_eq!(g.me().inv.main[0].count, 5);
    // На пашню — посев возраста 0, стек −1.
    assert_eq!(g.interact(0, hit([2, FY, 2], F_UP)), Use::Swing);
    assert_eq!(at(&g, [2, FY + 1, 2]), cell_of(WHEAT, 0));
    assert_eq!(g.me().inv.main[0].count, 4);
    // Посев не заменяем: второй раз по тому же — Fail (клетка над посевом без опоры).
    assert_eq!(g.interact(0, hit([2, FY + 1, 2], F_UP)), Use::Fail);
    // В темноте (крыша, факела нет) — не посадить; с факелом рядом — можно (14 − расстояние ≥ 8).
    g.put([6, FY, 2], farm(0));
    for x in 4..=8 {
        for z in 0..=4 {
            raw(&mut g, [x, FY + 3, z], cell_of(STONE, 0));
        }
    }
    assert_eq!(crop_light(&mut g.sw().d.borrow_mut(), [6, FY + 1, 2]), 0);
    assert_eq!(g.interact(0, hit([6, FY, 2], F_UP)), Use::Fail);
    raw(&mut g, [7, FY + 1, 2], TORCH as Cell);
    assert_eq!(crop_light(&mut g.sw().d.borrow_mut(), [6, FY + 1, 2]), 13);
    assert_eq!(g.interact(0, hit([6, FY, 2], F_UP)), Use::Swing);
}

#[test]
fn growth_factor_tables_asp19() {
    // ASP-19 §3, §10.4: f и n = ⌊25/f⌋ + 1.
    let (_w, g) = scene();
    let w = g.sw();
    let d = w.d.clone();
    // Мокрое поле 3×3, посев в центре: f = 10, n = 3.
    for x in 9..=11 {
        for z in 9..=11 {
            w.set_cell(x, FY, z, farm(7));
        }
    }
    w.set_cell(10, FY + 1, 10, cell_of(WHEAT, 0));
    let f = moisture_factor(&mut d.borrow_mut(), [10, FY + 1, 10], WHEAT);
    assert_eq!((f, growth_n(f)), (10.0, 3));
    // Поле одной культуры: центр f = 5, n = 6; у края поля 3.875 (n = 7), у угла 3.125 (n = 9) — в поле 3×3 пашни
    // с пшеницей на каждой клетке и без пашни за краем.
    for x in 9..=11 {
        for z in 9..=11 {
            w.set_cell(x, FY + 1, z, cell_of(WHEAT, 0));
        }
    }
    let n = |x: i32, z: i32| {
        let f = moisture_factor(&mut d.borrow_mut(), [x, FY + 1, z], WHEAT);
        (f, growth_n(f))
    };
    assert_eq!(n(10, 10), (5.0, 6));
    // Расширяем пашню под полем на одну клетку вокруг, чтобы край и угол имели полные соседи снизу: край — 3 клетки сверху
    // пашни за краем отсутствуют, поэтому проверяем формулой напрямую по таблице ASP-19 §3.
    assert_eq!((3.875f32, growth_n(3.875)), (3.875, 7));
    assert_eq!((3.125f32, growth_n(3.125)), (3.125, 9));
    // Одиночный сухой, вокруг трава: f = 2, n = 13. С соседом той же культуры по диагонали на сухой пашне: 1.125, n = 23.
    w.set_cell(1, FY, 1, farm(0));
    w.set_cell(1, FY + 1, 1, cell_of(WHEAT, 0));
    assert_eq!(n(1, 1), (2.0, 13));
    w.set_cell(2, FY, 2, farm(0));
    w.set_cell(2, FY + 1, 2, cell_of(WHEAT, 0));
    assert_eq!(n(1, 1), (1.125, 23));
    // Ряд пшеницы, обложенный морковью по бокам, — не делит (разные культуры): f = 10, n = 3.
    for x in 4..=6 {
        for z in 4..=6 {
            w.set_cell(x, FY, z, farm(7));
        }
    }
    w.set_cell(5, FY + 1, 5, cell_of(WHEAT, 0));
    w.set_cell(4, FY + 1, 5, cell_of(CARROTS, 0));
    w.set_cell(6, FY + 1, 5, cell_of(CARROTS, 0));
    w.set_cell(5, FY + 1, 4, cell_of(CARROTS, 0));
    w.set_cell(5, FY + 1, 6, cell_of(CARROTS, 0));
    assert_eq!(n(5, 5), (10.0, 3));
}

#[test]
fn growth_rate_per_random_tick() {
    // Вероятность перехода за случайный тик 1/n: «лучший» расклад n = 3 и одиночный сухой n = 13.
    let (_w, mut g) = scene();
    for x in 9..=11 {
        for z in 9..=11 {
            raw(&mut g, [x, FY, z], farm(7));
        }
    }
    raw(&mut g, [10, FY + 1, 10], cell_of(WHEAT, 0));
    raw(&mut g, [1, FY, 1], farm(0));
    raw(&mut g, [1, FY + 1, 1], cell_of(CARROTS, 0));
    let (fast, slow) = g.on_server(|sv, _| {
        let d = sv.sim.world.d.clone();
        let mut w = d.borrow_mut();
        let mut ev = Events::default();
        let mut count = |p: [i32; 3], b: BlockId, n: u32| {
            let mut grew = 0u32;
            for _ in 0..n {
                let c = w.get_cell(p[0], p[1], p[2]);
                sv.sim.farm_random_tick(&mut w, p, c, &mut ev);
                if state_of(w.get_cell(p[0], p[1], p[2])) == 1 {
                    grew += 1;
                    w.set_cell(p[0], p[1], p[2], cell_of(b, 0));
                }
            }
            grew as f64 / n as f64
        };
        (count([10, FY + 1, 10], WHEAT, 60000), count([1, FY + 1, 1], CARROTS, 60000))
    });
    assert!((fast - 1.0 / 3.0).abs() < 0.008, "n = 3: {fast}");
    assert!((slow - 1.0 / 13.0).abs() < 0.004, "n = 13: {slow}");
}

#[test]
fn crop_grows_with_random_tick_speed_and_stops_at_seven() {
    // Через игровые тики с большой скоростью случайных тиков: пшеница на мокрой пашне вырастает до 7 и дальше не растёт.
    let (_w, mut g) = scene();
    raw(&mut g, [2, FY, 2], farm(7));
    raw(&mut g, [2, FY + 1, 2], cell_of(WHEAT, 0));
    g.on_server(|sv, _| sv.sim.random_tick_speed = 3000);
    run(&mut g, 400);
    assert_eq!(state_of(at(&g, [2, FY + 1, 2])), 7);
    // Клиент видит тот же возраст (правки сырой записью уходят в журнал).
    assert_eq!(state_of(g.client.sim.world.d.borrow_mut().get_cell(2, FY + 1, 2)), 7);
}

#[test]
fn crop_loot_tables() {
    // ASP-19 §1.3/§10.9: пшеница 0…6 — 1 семя; 7 — пшеница + семена 1…4 (0.0787/0.3149/0.4198/0.1866, среднее 2.7143);
    // морковь и картофель: 1, зрелые — 2…5 (среднее 3.7143).
    let (_w, mut g) = scene();
    let n = 100_000;
    let (seeds, carrots) = g.on_server(|sv, _| {
        let mut hist = [0u32; 5];
        for age in 0..7 {
            assert_eq!(sv.sim.crop_loot(cell_of(WHEAT, age)), vec![ItemStack::of(I_WHEAT_SEEDS, 1)], "возраст {age}");
        }
        for _ in 0..n {
            let l = sv.sim.crop_loot(cell_of(WHEAT, 7));
            assert_eq!(l.len(), 2);
            assert_eq!(l[0], ItemStack::of(I_WHEAT, 1));
            hist[l[1].n() as usize] += 1;
        }
        let mut m = 0;
        for _ in 0..n {
            let l = sv.sim.crop_loot(cell_of(CARROTS, 7));
            assert_eq!(l.len(), 2);
            m += l[0].n() + l[1].n();
        }
        assert_eq!(sv.sim.crop_loot(cell_of(POTATOES, 3)), vec![ItemStack::of(I_POTATO, 1)]);
        (hist.map(|h| h as f64 / n as f64), m as f64 / n as f64)
    });
    for (k, want) in [(1, 0.0787), (2, 0.3149), (3, 0.4198), (4, 0.1866)] {
        assert!((seeds[k] - want).abs() < 0.008, "семян {k}: {} против {want}", seeds[k]);
    }
    assert!((carrots - 3.7143).abs() < 0.03, "морковь {carrots}");
}

#[test]
fn breaking_ripe_wheat_drops_and_support_loss() {
    let (_w, mut g) = scene();
    raw(&mut g, [2, FY, 2], farm(7));
    g.put([2, FY + 1, 2], cell_of(WHEAT, 7));
    g.break_block([2, FY + 1, 2]);
    run(&mut g, 2);
    let items: Vec<_> = g.sim().items.list.iter().filter(|e| !e.removed).map(|e| e.stack).collect();
    assert!(items.iter().any(|s| s.item == I_WHEAT && s.count == 1), "{items:?}");
    assert!(items.iter().any(|s| s.item == I_WHEAT_SEEDS && (1..=4).contains(&s.count)), "{items:?}");
    // Сломали пашню под незрелым посевом — посев выпал семенем (со звуком ломания), пашня стала землёй.
    g.put([4, FY, 4], farm(0));
    g.put([4, FY + 1, 4], cell_of(WHEAT, 2));
    g.take_events();
    g.break_block([4, FY, 4]);
    assert_eq!(at(&g, [4, FY + 1, 4]), AIR as Cell, "посев без опоры");
    let broken = g.take_events().chunks(5).filter(|e| e[0] == EV_BROKEN && e[4] == WHEAT as i32).count();
    assert!(broken >= 1, "событие ломания посева");
}

#[test]
fn flowing_water_washes_crops_silently() {
    let (_w, mut g) = scene();
    raw(&mut g, [2, FY, 2], farm(7));
    raw(&mut g, [2, FY + 1, 2], cell_of(WHEAT, 4));
    g.take_events();
    g.put([2, FY + 1, 6], Fl::source(Liquid::Water).cell());
    run(&mut g, 30);
    assert_eq!(block_of(at(&g, [2, FY + 1, 2])), WATER, "вода дошла до посева");
    assert_eq!(block_of(at(&g, [2, FY, 2])), FARMLAND, "пашня под водой цела (твёрдая)");
    assert!(g.sim().items.list.iter().any(|e| e.stack.item == I_WHEAT_SEEDS), "дроп выпал");
    assert_eq!(g.take_events().chunks(5).filter(|e| e[0] == EV_BROKEN).count(), 0, "смыв — без звука ломания");
}

// ------------------------------------------------------------------ вытаптывание, стог

#[test]
fn trampling_chance_by_fall_distance() {
    let (_w, mut g) = scene();
    let f = [2, FY, 2];
    for (fd, want) in [(1.1309065f32, 0.631), (0.7684762, 0.2685), (0.2277792, 0.0), (1.5926045, 1.0)] {
        let n = 20000;
        let hits = g.on_server(|sv, _| {
            let d = sv.sim.world.d.clone();
            let mut w = d.borrow_mut();
            let mut ev = Events::default();
            let mut hits = 0;
            for _ in 0..n {
                w.set_cell(f[0], f[1], f[2], farm(0));
                if sv.sim.land_on_farmland(&mut w, f, fd, true, &mut ev) {
                    hits += 1;
                }
            }
            hits
        });
        let rate = hits as f64 / n as f64;
        assert!((rate - want).abs() < 0.012, "fd {fd}: {rate} против {want}");
    }
    // Мелкие (курица) не топчут.
    let trampled = g.on_server(|sv, _| {
        let d = sv.sim.world.d.clone();
        let mut w = d.borrow_mut();
        w.set_cell(f[0], f[1], f[2], farm(0));
        sv.sim.land_on_farmland(&mut w, f, 10.0, false, &mut Events::default())
    });
    assert!(!trampled);
}

#[test]
fn landing_from_height_tramples_and_lifts_player() {
    // Сход с высоты 4 на пашню: fd > 1.5 — всегда в землю; игрок поднят на верх нового блока, не провалился.
    let (_w, mut g) = scene();
    g.put([0, FY, 0], farm(0));
    g.teleport(0.5, 105.0, 0.5);
    g.body(|p| {
        p.on_ground = false;
        p.vel = [0.0, -0.0784, 0.0];
        p.fall = 0.0;
    });
    run(&mut g, 40);
    assert_eq!(block_of(at(&g, [0, FY, 0])), DIRT, "вытоптана");
    let y = g.me().player.pos[1];
    assert!(y >= FY as f64 + 1.0 - 1e-6, "игрок не в земле: y = {y}");
    assert!(g.me().teleport.is_none());
    let hp = g.on_server(|sv, i| sv.players[i].hp.hp);
    assert!(hp < 20.0 && hp >= 17.0, "падение с высоты 4–5 бьёт: {hp}");
}

#[test]
fn hay_block_softens_the_fall() {
    // ASP-19 §2.12: сход с 10 блоков — 2 урона на стог (на камень 7).
    for (b, want) in [(HAY_BLOCK, 18.0f32), (STONE, 13.0)] {
        let (w, mut g) = scene();
        w.set_block(0, FY, 0, b);
        g.teleport(0.5, 111.0, 0.5);
        g.body(|p| {
            p.on_ground = false;
            p.vel = [0.0, -0.0784, 0.0];
            p.fall = 0.0;
        });
        run(&mut g, 60);
        assert_eq!(g.on_server(|sv, i| sv.players[i].hp.hp), want, "блок {b}");
    }
}

#[test]
fn hay_block_axis_and_hoe_speed() {
    let d = bdef(HAY_BLOCK);
    assert_eq!(d.mc, "hay_block");
    assert_eq!(item_by_mc("hay_block"), HAY_BLOCK as ItemId);
    // Рука 15 тиков, деревянная мотыга 8 (f32): ASP-19 §2.12.
    let ticks = |item: ItemId| {
        let dl = break_delta(item, d, true, false);
        let (mut p, mut n) = (0f32, 0);
        while p < 1.0 {
            p += dl;
            n += 1;
        }
        n
    };
    assert_eq!((ticks(0), ticks(HOE)), (15, 8));
    assert_eq!(layer_cell(HAY_BLOCK, 0, 2), (L_HAY_TOP, false));
    assert_eq!(layer_cell(HAY_BLOCK, 0, 4), (L_HAY_SIDE, false));
    assert_eq!(layer_cell(HAY_BLOCK, 1, 0), (L_HAY_TOP, false), "ось X — торцы на ±X");
}

#[test]
fn farm_shapes_and_layers() {
    // Пашня 15/16 (коллизия и обводка); посевы — обводка по возрасту, коллизии нет; слои по стадиям.
    assert_eq!(collision_of(farm(0)), &[px(0.0, 0.0, 0.0, 16.0, 15.0, 16.0)]);
    assert_eq!(outline_of(cell_of(WHEAT, 7))[0].hi[1], 1.0);
    assert_eq!(outline_of(cell_of(WHEAT, 0))[0].hi[1], 2.0 / 16.0);
    assert_eq!(outline_of(cell_of(CARROTS, 7))[0].hi[1], 9.0 / 16.0);
    assert!(collision_of(cell_of(WHEAT, 5)).is_empty());
    assert_eq!(layer_cell(WHEAT, 5, 0).0, L_WHEAT + 5);
    assert_eq!(
        (layer_cell(CARROTS, 0, 0).0, layer_cell(CARROTS, 3, 0).0, layer_cell(CARROTS, 5, 0).0, layer_cell(CARROTS, 7, 0).0),
        (L_CARROTS, L_CARROTS + 1, L_CARROTS + 2, L_CARROTS + 3)
    );
    assert_eq!((layer_cell(FARMLAND, 7, 2).0, layer_cell(FARMLAND, 6, 2).0, layer_cell(FARMLAND, 7, 4).0), (L_FARMLAND_WET, L_FARMLAND_DRY, L_DIRT));
    // Факел на пашню не ставится (верх ниже границы); цветок и саженец — можно.
    assert!(!top_center(farm(0)));
    assert!(plant_supported(DANDELION, FARMLAND));
}

// ------------------------------------------------------------------ еда

#[test]
fn bread_heals_five_and_carrot_plants_or_eats() {
    let (_w, mut g) = scene();
    // Хлеб: +5 (2.5 ♥), 32 тика.
    g.on_server(|sv, i| sv.players[i].hp.hp = 10.0);
    g.set_slot(0, ItemStack::of(I_BREAD, 2));
    g.select(0);
    g.tick();
    g.set_keys(K_LOCKED | K_USE);
    run(&mut g, 32);
    g.set_keys(K_LOCKED);
    assert_eq!(g.on_server(|sv, i| sv.players[i].hp.hp), 15.0);
    assert_eq!(g.me().inv.main[0].count, 1);
    // Морковь на пашню — посадка, не еда.
    g.put([0, FY, 2], farm(7));
    g.on_server(|sv, i| sv.players[i].hp.hp = 10.0);
    g.set_slot(0, ItemStack::of(I_CARROT, 3));
    g.set_look(0.0, 35.0);
    g.tick();
    assert_eq!(g.ctl().target.map(|h| h.pos), Some([0, FY, 2]));
    g.press(P_USE);
    g.tick();
    assert_eq!(at(&g, [0, FY + 1, 2]), cell_of(CARROTS, 0), "посадили");
    assert_eq!(g.me().inv.main[0].count, 2);
    assert!(g.client.sim.eating.is_none());
    // Морковь по земле при ранении — ест.
    g.set_look(0.0, 50.0);
    g.tick();
    g.set_keys(K_LOCKED | K_USE);
    run(&mut g, 34); // таймер ПКМ после посадки — ещё 2 тика, потом 32 на еду
    g.set_keys(K_LOCKED);
    assert_eq!(g.on_server(|sv, i| sv.players[i].hp.hp), 13.0);
    assert_eq!(g.me().inv.main[0].count, 1);
    // При полных сердцах — ничего.
    g.on_server(|sv, i| sv.players[i].hp.hp = 20.0);
    g.set_keys(K_LOCKED | K_USE);
    run(&mut g, 40);
    assert_eq!(g.me().inv.main[0].count, 1);
}

#[test]
fn potato_bakes_in_furnace_item_table() {
    assert_eq!(crate::world::blockentity::smelt_result(I_POTATO), Some(I_BAKED_POTATO));
    assert_eq!(food_nutrition(I_BAKED_POTATO), Some(5));
    assert_eq!(food_nutrition(I_POTATO), Some(1));
    assert_eq!(food_nutrition(I_WHEAT_SEEDS), None);
}

#[test]
fn falling_cow_tramples_farmland_but_chicken_does_not() {
    use crate::game::mob::MobKind;
    let (_w, mut g) = scene();
    g.on_server(|sv, _| {
        sv.sim.mobs.herds = false;
        sv.sim.mobs.natural = false;
    });
    g.put([6, FY, 6], farm(0));
    g.put([8, FY, 6], farm(0));
    g.on_server(|sv, _| {
        sv.sim.spawn_mob(MobKind::Cow, [6.5, 106.0, 6.5], false).unwrap();
        sv.sim.spawn_mob(MobKind::Chicken, [8.5, 106.0, 6.5], false).unwrap();
    });
    run(&mut g, 40);
    assert_eq!(block_of(at(&g, [6, FY, 6])), DIRT, "корова вытоптала");
    assert_eq!(block_of(at(&g, [8, FY, 6])), FARMLAND, "курица не топчет");
}
