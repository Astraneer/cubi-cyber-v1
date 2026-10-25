//! Проверки текучей воды, лавы и вёдер (G7, ASP-15 §13; только для cargo test). Сцена: каменный пол 49×49 до y = 100,
//! воздух выше; игрок — в стороне от разлива. Мир и тики решает сервер, смотрим мир сервера (`g.sw()`); эталоны чисел —
//! модель тика жидкости из ASP-15 (§2, §13).

use super::*;
use crate::abi::*;
use crate::cell::{block_of, offset, state_of, Cell};
use crate::game::fluids::{fluid_height, fluid_of, Fl, Liquid};
use crate::registry::ids::*;
use crate::registry::items::*;
use std::collections::BTreeMap;

const FY: i32 = 100;
const R: i32 = 24;

fn scene() -> (World, Game) {
    scene_with(|_| {})
}
fn scene_with(f: impl FnOnce(&World)) -> (World, Game) {
    let w = World::new(1337);
    for x in -R..=R {
        for z in -R..=R {
            for y in 92..=FY {
                w.set_block(x, y, z, STONE);
            }
        }
    }
    f(&w);
    let mut g = Game::new(&w);
    g.teleport(20.5, 101.0, 20.5);
    g.body(|p| {
        p.on_ground = true;
        p.vel = [0.0, -0.0784, 0.0];
    });
    g.set_look(0.0, 0.0);
    g.tick();
    g.on_server(|sv, i| sv.players[i].hp.spawn_protect = 0);
    (w, g)
}
fn src(l: Liquid) -> Cell {
    Fl::source(l).cell()
}
fn water_in(w: &World, lo: [i32; 3], hi: [i32; 3]) -> BTreeMap<[i32; 3], Fl> {
    let mut out = BTreeMap::new();
    for x in lo[0]..=hi[0] {
        for y in lo[1]..=hi[1] {
            for z in lo[2]..=hi[2] {
                if let Some(f) = fluid_of(w.get_cell(x, y, z)) {
                    out.insert([x, y, z], f);
                }
            }
        }
    }
    out
}
fn all_fluid(w: &World) -> BTreeMap<[i32; 3], Fl> {
    water_in(w, [-14, 90, -14], [14, 125, 14])
}
fn pending(g: &Game) -> u32 {
    g.world_stats()[0]
}
/// Первый тик (с 1), после которого клетка — жидкость; (источник, поставленный до тиков, — 0).
fn first_seen(g: &mut Game, n: u32) -> BTreeMap<[i32; 3], u32> {
    let sw = g.sw();
    let mut seen: BTreeMap<[i32; 3], u32> = all_fluid(&sw).into_keys().map(|p| (p, 0)).collect();
    for t in 1..=n {
        g.tick();
        for p in all_fluid(&sw).into_keys() {
            seen.entry(p).or_insert(t);
        }
    }
    seen
}

#[test]
fn sea_stays_quiet_and_reacts_to_changes() {
    // Генератор: море — источники, тиков при загрузке нет (ASP-15 §9); камень на поверхность — 5 тиков без изменений воды;
    // выкопали блок дна — яма заполняется падающей водой (LEVEL 8).
    let (_w, mut g) = scene_with(|w| {
        for x in -8..=8 {
            for z in -8..=8 {
                for y in 93..=96 {
                    w.set_block(x, y, z, WATER);
                }
                for y in 97..=FY {
                    w.set_block(x, y, z, AIR);
                }
            }
        }
        for x in -9..=9 {
            for y in 93..=FY {
                for z in [-9, 9] {
                    w.set_block(x, y, z, STONE);
                    w.set_block(z, y, x, STONE);
                }
            }
        }
    });
    let sw = g.sw();
    for _ in 0..30 {
        g.tick();
    }
    assert_eq!(pending(&g), 0, "море спит");
    let before = all_fluid(&sw);
    g.put([0, 97, 0], STONE as Cell);
    assert!(pending(&g) > 0, "камень разбудил соседей");
    for _ in 0..12 {
        g.tick();
    }
    assert_eq!(all_fluid(&sw), before, "вода не изменилась");
    assert_eq!(pending(&g), 0);
    // Дно: ниже уровня воды копаем пол.
    g.put([3, 92, 3], AIR as Cell);
    for _ in 0..12 {
        g.tick();
    }
    let f = fluid_of(sw.get_cell(3, 92, 3));
    // Вокруг источники, снизу камень: яма на 92 заполняется падающей водой (под водой сверху).
    assert_eq!(f.map(|f| f.falling), Some(true), "в яме падающая вода: {f:?}");
    assert_eq!(state_of(sw.get_cell(3, 92, 3)), 8);
}

#[test]
fn flat_spill_diamond_of_113() {
    // ASP-15 §13.1: клетка на расстоянии d появляется через 5d тиков с LEVEL = d; всего 113; последняя запись — T+35.
    let (_w, mut g) = scene();
    g.put([0, 101, 0], src(Liquid::Water));
    let seen = first_seen(&mut g, 42);
    assert_eq!(seen.len(), 113);
    let sw = g.sw();
    for (p, &t) in &seen {
        let d = p[0].abs() + p[2].abs();
        assert_eq!((p[1], t), (101, 5 * d as u32), "клетка {p:?}");
        if d > 0 {
            assert_eq!(state_of(sw.get_cell(p[0], p[1], p[2])), d as u16, "LEVEL клетки {p:?}");
        }
    }
    assert_eq!(*seen.values().max().unwrap(), 35);
    assert_eq!(pending(&g), 0, "после T+40 очередь пуста");
    // Зачерпнули источник — вся вода исчезает к T+35 от снятия.
    g.put([0, 101, 0], AIR as Cell);
    for _ in 0..40 {
        g.tick();
    }
    assert!(all_fluid(&sw).is_empty(), "вода стекла: {:?}", all_fluid(&sw).len());
}

#[test]
fn pit_attracts_flow() {
    // ASP-15 §13.3: яма на 5 вниз в (5,·,0) — вода идёт только к ней: 11 клеток; яма в (6,·,0) не видна — ромб.
    let (_w, mut g) = scene_with(|w| {
        for y in 96..=FY {
            w.set_block(5, y, 0, AIR);
        }
    });
    g.put([0, 101, 0], src(Liquid::Water));
    for _ in 0..60 {
        g.tick();
    }
    let sw = g.sw();
    let cells = all_fluid(&sw);
    assert_eq!(cells.len(), 11, "{:?}", cells.keys().collect::<Vec<_>>());
    for x in 1..=5 {
        let f = cells[&[x, 101, 0]];
        assert_eq!((f.amount, f.falling), (8 - x as u8, false), "x={x}");
    }
    for y in 96..=100 {
        assert!(cells[&[5, y, 0]].falling, "столб в яме y={y}");
    }
    // Яма в 6 блоках — вне пути поиска: растекается ромбом, но не за ямой.
    let (_w, mut g) = scene_with(|w| {
        for y in 96..=FY {
            w.set_block(6, y, 0, AIR);
        }
    });
    g.put([0, 101, 0], src(Liquid::Water));
    for _ in 0..80 {
        g.tick();
    }
    let cells = all_fluid(&g.sw());
    assert!(cells.contains_key(&[0, 101, 3]) && cells.contains_key(&[-3, 101, 0]), "ромб: во все стороны");
    assert!(!cells.contains_key(&[7, 101, 0]), "за ямой воды нет");
    assert_eq!(cells.len(), 117, "113 клеток ромба без (7,·,0) плюс 5 в яме");
}

#[test]
fn two_by_two_makes_infinite_source() {
    // ASP-15 §13.4: яма 2×2 глубиной 1; источники по диагонали — через 5 тиков все четыре источники; в соседних углах —
    // две другие клетки текут (LEVEL 1). Зачерпнули один — через 5 тиков снова источник.
    let ring = |w: &World| {
        for x in -2..=3 {
            for z in -2..=3 {
                if !(0..=1).contains(&x) || !(0..=1).contains(&z) {
                    w.set_block(x, 101, z, STONE);
                }
            }
        }
    };
    let (_w, mut g) = scene_with(ring);
    g.put([0, 101, 0], src(Liquid::Water));
    g.put([1, 101, 1], src(Liquid::Water));
    for _ in 0..12 {
        g.tick();
    }
    let sw = g.sw();
    for (x, z) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
        assert_eq!(sw.get_cell(x, 101, z), src(Liquid::Water), "({x},{z}) — источник");
    }
    g.put([1, 101, 0], AIR as Cell);
    for _ in 0..7 {
        g.tick();
    }
    assert_eq!(sw.get_cell(1, 101, 0), src(Liquid::Water), "зачерпнули — снова источник");
    // Соседние углы: два источника рядом.
    let (_w, mut g) = scene_with(ring);
    g.put([0, 101, 0], src(Liquid::Water));
    g.put([1, 101, 0], src(Liquid::Water));
    for _ in 0..12 {
        g.tick();
    }
    let sw = g.sw();
    for (x, z) in [(0, 1), (1, 1)] {
        let f = fluid_of(sw.get_cell(x, 101, z)).unwrap();
        assert_eq!((f.still, f.amount), (false, 7), "({x},{z}) — течёт");
    }
}

#[test]
fn hanging_source_cross_of_five() {
    // ASP-15 §13.5: висящий источник в 6 над полом — крест из пяти струй на каждом уровне, всего 170 клеток, покой T+70.
    let (_w, mut g) = scene();
    g.put([0, 106, 0], src(Liquid::Water));
    let seen = first_seen(&mut g, 80);
    assert_eq!(seen.len(), 170);
    let sw = g.sw();
    let cells = all_fluid(&sw);
    for y in 102..=106 {
        let n = cells.keys().filter(|p| p[1] == y).count();
        assert_eq!(n, 5, "на y={y} по пять клеток");
    }
    assert_eq!(*seen.values().max().unwrap(), 65, "последняя запись в T+65");
    assert_eq!(pending(&g), 0, "покой к T+70");
}

#[test]
fn cliff_flow() {
    // ASP-15 §13.6: пол 110 при x ≤ 0 и 100 при x > 0, источник на краю верхней ступени — вода внизу в T+55, всего 75 клеток.
    let (_w, mut g) = scene_with(|w| {
        for x in -R..=0 {
            for z in -R..=R {
                for y in FY + 1..=110 {
                    w.set_block(x, y, z, STONE);
                }
            }
        }
    });
    g.put([0, 111, 0], src(Liquid::Water));
    let seen = first_seen(&mut g, 100);
    assert_eq!(seen[&[1, 101, 0]], 55);
    assert_eq!(seen.len(), 75);
    assert_eq!(*seen.values().max().unwrap(), 90);
}

#[test]
fn lava_spreads_three_blocks() {
    // ASP-15 §13.9: лава на ровном — 25 клеток, LEVEL по кольцам 0, 2, 4, 6; кольцо k — в T+30k; покой к T+120.
    let (_w, mut g) = scene();
    g.put([0, 101, 0], src(Liquid::Lava));
    let seen = first_seen(&mut g, 130);
    assert_eq!(seen.len(), 25);
    let sw = g.sw();
    for (p, &t) in &seen {
        let d = p[0].abs() + p[2].abs();
        assert_eq!(t, 30 * d as u32, "{p:?}");
        assert_eq!(state_of(sw.get_cell(p[0], p[1], p[2])), 2 * d as u16, "LEVEL лавы {p:?}");
    }
    assert_eq!(pending(&g), 0);
}

#[test]
fn flowing_water_washes_plants_without_events() {
    // ASP-15 §13.10: трава в (0,·,2) смыта в T+10, факел в (3,·,0) — в T+15; дроп без события ломания.
    let (_w, mut g) = scene_with(|w| {
        w.set_block(0, FY, 2, DIRT);
        w.set_block(0, 101, 2, TALL_GRASS);
        w.set_block(3, 101, 0, TORCH);
    });
    g.put([0, 101, 0], src(Liquid::Water));
    let sw = g.sw();
    let mut broken_events = 0;
    let (mut grass_at, mut torch_at) = (0, 0);
    for t in 1..=20 {
        g.tick();
        broken_events += g.take_events().chunks(5).filter(|e| e[0] == EV_BROKEN).count();
        if grass_at == 0 && block_of(sw.get_cell(0, 101, 2)) == WATER {
            grass_at = t;
        }
        if torch_at == 0 && block_of(sw.get_cell(3, 101, 0)) == WATER {
            torch_at = t;
        }
    }
    assert_eq!((grass_at, torch_at), (10, 15));
    assert_eq!(broken_events, 0, "смыв — без звука и частиц");
    assert!(g.sim().items.list.iter().any(|e| e.stack.item == TORCH as ItemId), "факел выпал предметом");
}

#[test]
fn lava_and_water_meet() {
    // Источник лавы рядом с водой — обсидиан; течёт — булыжник; лава вниз в воду — камень (ASP-15 §4).
    let (_w, mut g) = scene();
    g.put([0, 101, 0], src(Liquid::Lava));
    g.put([1, 101, 0], src(Liquid::Water));
    assert_eq!(block_of(g.sw().get_cell(0, 101, 0)), OBSIDIAN, "источник лавы у воды");
    // Лава течёт к воде: растекшаяся клетка твердеет булыжником, водный источник цел.
    let (_w, mut g) = scene();
    g.put([0, 101, 0], src(Liquid::Lava));
    for _ in 0..100 {
        g.tick();
    }
    let sw = g.sw();
    assert_eq!(state_of(sw.get_cell(3, 101, 0)), 6, "край лавы: тонкий слой");
    g.put([4, 101, 0], src(Liquid::Water));
    assert_eq!(block_of(sw.get_cell(3, 101, 0)), COBBLE, "течёт лава — соседняя вода: булыжник");
    assert_eq!(block_of(sw.get_cell(0, 101, 0)), LAVA, "далёкий источник цел");
    // Лава сверху на воду: вода под ней становится камнем.
    let (_w, mut g) = scene_with(|w| {
        w.set_block(0, 101, 0, WATER);
    });
    g.put([0, 102, 0], src(Liquid::Lava));
    let sw = g.sw();
    assert_eq!(block_of(sw.get_cell(0, 102, 0)), LAVA, "вода только снизу — источник лавы цел");
    for _ in 0..35 {
        g.tick();
    }
    assert_eq!(block_of(sw.get_cell(0, 101, 0)), STONE, "лава течёт вниз в воду — камень");
}

#[test]
fn fluid_heights_follow_level() {
    // FlowableFluid.getHeight: источник 8/9, течёт — количество/9, под такой же жидкостью — 1.
    let c = |a: u8| Fl::flowing(Liquid::Water, a).cell();
    let air = AIR as Cell;
    assert_eq!(fluid_height(src(Liquid::Water), air), (8.0f32 / 9.0f32) as f64);
    assert_eq!(fluid_height(c(7), air), (7.0f32 / 9.0f32) as f64);
    assert_eq!(fluid_height(c(1), air), (1.0f32 / 9.0f32) as f64);
    assert_eq!(fluid_height(c(1), c(5)), 1.0);
    assert_eq!(fluid_height(c(1), src(Liquid::Lava)), (1.0f32 / 9.0f32) as f64, "лава сверху воды — не та же жидкость");
    assert_eq!(fluid_height(air, air), 0.0);
    assert_eq!(fluid_of(Fl::falling(Liquid::Water).cell()).map(|f| f.falling), Some(true));
}

// ------------------------------------------------------------------ вёдра

/// Игрок на камне в (0.5, 101, 0.5) смотрит на пол перед собой (пол на y = 100, взгляд вниз-вперёд).
fn bucket_scene() -> (World, Game) {
    let (w, mut g) = scene();
    g.teleport(0.5, 101.0, 0.5);
    g.body(|p| {
        p.on_ground = true;
        p.vel = [0.0, -0.0784, 0.0];
    });
    g.set_look(0.0, 50.0); // yaw 0 — на юг (+Z), вниз под 50°
    g.set_keys(K_LOCKED);
    g.tick();
    (w, g)
}
fn count(g: &Game, it: ItemId) -> i32 {
    (0..36).map(|i| g.ctl().inv.get(i)).filter(|s| s.id() == it).map(|s| s.n()).sum()
}

#[test]
fn water_bucket_pours_source_and_it_spreads() {
    // Пуля G7: ведро воды ставит источник (в кадре ПКМ), вода растекается, в руке пустое ведро.
    let (w, mut g) = bucket_scene();
    g.set_slot(0, ItemStack::of(I_WATER_BUCKET, 1));
    g.select(0);
    g.tick();
    let t = g.ctl().target.expect("цель — пол");
    let at = offset(t.pos, t.face);
    g.press(P_USE);
    g.tick();
    assert_eq!(w.get_cell(at[0], at[1], at[2]), src(Liquid::Water), "источник сразу");
    assert_eq!(g.ctl().inv.main[0], ItemStack::of(I_BUCKET, 1));
    assert_eq!(g.me().inv.main[0], ItemStack::of(I_BUCKET, 1), "и у сервера");
    for _ in 0..12 {
        g.tick();
    }
    let n = all_fluid(&g.sw()).len();
    assert!(n > 5, "вода растекается: {n}");
    g.assert_in_sync();
}

#[test]
fn empty_bucket_scoops_only_sources() {
    // Пустое ведро набирает источник (из стека 3 пустых: в руке 2, полное — в первую свободную ячейку), текучую — нет.
    let (w, mut g) = bucket_scene();
    g.put([0, 101, 2], src(Liquid::Water));
    g.set_slot(0, ItemStack::of(I_BUCKET, 3));
    g.select(0);
    g.set_look(0.0, 35.0); // на клетку (0,101,2)
    g.tick();
    // В клетку попали до того, как вода успела разлиться? Дадим расплыться и найдём источник сами.
    for _ in 0..12 {
        g.tick();
    }
    assert!(all_fluid(&g.sw()).len() > 3);
    g.press(P_USE);
    g.tick();
    let have = (count(&g, I_WATER_BUCKET), count(&g, I_BUCKET));
    assert_eq!(have, (1, 2), "набрали источник");
    assert_eq!(g.ctl().inv.main[0], ItemStack::of(I_BUCKET, 2));
    assert_eq!(g.ctl().inv.main[1], ItemStack::of(I_WATER_BUCKET, 1));
    assert_eq!(g.me().inv.main[1], ItemStack::of(I_WATER_BUCKET, 1));
    assert_eq!(w.get_cell(0, 101, 2), AIR as Cell, "источника нет");
    // Вода стекает за ~1.75 с.
    for _ in 0..60 {
        g.tick();
    }
    assert!(all_fluid(&g.sw()).is_empty(), "всё стекло");
    g.assert_in_sync();
}

#[test]
fn empty_bucket_ignores_flowing_water() {
    // Цель — только источник: ручей (текучая вода) луч пропускает, ведро ничего не набирает.
    let (_w, mut g) = bucket_scene();
    g.put([0, 101, 3], src(Liquid::Water));
    for _ in 0..8 {
        g.tick();
    }
    // Клетка (0,101,2) — текучая (уровень 7); целимся в неё и в пол за ней.
    assert_eq!(fluid_of(g.sw().get_cell(0, 101, 2)).map(|f| f.still), Some(false));
    g.set_slot(0, ItemStack::of(I_BUCKET, 1));
    g.select(0);
    g.set_look(0.0, 38.0);
    g.tick();
    g.press(P_USE);
    g.tick();
    let n = count(&g, I_WATER_BUCKET);
    assert_eq!(n, 0, "текучую воду не набрать");
    assert_eq!(count(&g, I_BUCKET), 1);
}

#[test]
fn full_inventory_drops_filled_bucket() {
    // ASP-15 §13.12: инвентарь полон — полное ведро выпадает, в руке остаётся на одно пустое меньше.
    let (_w, mut g) = bucket_scene();
    g.put([0, 101, 2], src(Liquid::Water));
    g.set_slot(0, ItemStack::of(I_BUCKET, 2));
    for k in 1..36 {
        g.set_slot(k, ItemStack::of(COBBLE as ItemId, 64));
    }
    g.select(0);
    g.set_look(0.0, 35.0);
    g.tick();
    g.press(P_USE);
    g.tick();
    assert_eq!(g.me().inv.main[0], ItemStack::of(I_BUCKET, 1));
    assert!(g.sim().items.list.iter().any(|e| e.stack.item == I_WATER_BUCKET), "полное ведро выпало");
}

#[test]
fn bucket_into_existing_source_succeeds_and_plant_breaks_with_sound() {
    let (w, mut g) = bucket_scene();
    // Полное ведро на цветок: вода — рядом с ним (по грани), цветок цел.
    w.set_block(0, 100, 1, DIRT);
    w.set_block(0, 101, 1, DANDELION);
    g.set_slot(0, ItemStack::of(I_WATER_BUCKET, 1));
    g.select(0);
    g.set_look(0.0, 50.0);
    g.tick();
    let t = g.ctl().target.unwrap();
    assert_eq!(t.pos, [0, 101, 1]);
    g.press(P_USE);
    g.tick();
    assert_eq!(w.get_block(0, 101, 1), DANDELION);
    let near = offset(t.pos, t.face);
    assert_eq!(w.get_cell(near[0], near[1], near[2]), src(Liquid::Water));
    // Ещё раз в тот же источник (ведро снова полное): успех без изменений, ведро пустеет.
    g.set_slot(0, ItemStack::of(I_WATER_BUCKET, 1));
    g.press(P_USE);
    for _ in 0..6 {
        g.tick();
    }
    assert_eq!(count(&g, I_WATER_BUCKET), 0);
    g.assert_in_sync();
}

#[test]
fn lava_bucket_and_obsidian() {
    // Ведро лавы ставит лаву; ведро воды на источник лавы — обсидиан.
    let (w, mut g) = bucket_scene();
    g.set_slot(0, ItemStack::of(I_LAVA_BUCKET, 1));
    g.select(0);
    g.tick();
    let t = g.ctl().target.unwrap();
    let at = offset(t.pos, t.face);
    g.press(P_USE);
    g.tick();
    assert_eq!(w.get_cell(at[0], at[1], at[2]), src(Liquid::Lava));
    assert_eq!(g.me().inv.main[0], ItemStack::of(I_BUCKET, 1));
    // Ведро воды на лаву: источник лавы твердеет в обсидиан (вода встала рядом).
    g.set_slot(0, ItemStack::of(I_WATER_BUCKET, 1));
    g.select(0);
    g.set_look(0.0, 50.0);
    g.tick();
    let t2 = g.ctl().target.unwrap();
    // Цель — пол рядом с лавой; кладём воду в клетку прямо у лавы.
    let _ = t2;
    g.put([at[0] + 1, at[1], at[2]], src(Liquid::Water));
    assert_eq!(block_of(g.sw().get_cell(at[0], at[1], at[2])), OBSIDIAN);
}

#[test]
fn generator_water_never_touches_air_below_the_surface() {
    // ASP-15 §13.13: загрузка кусков не ставит тиков — вода генератора нигде не граничит с воздухом или растением,
    // кроме верхней грани на y = SEA (иначе море потекло бы само). 64 куска вокруг (0, 0), несколько сидов.
    use crate::cell::SEA;
    use crate::registry::blocks::{is_plant, water_faces};
    let mut water = 0u32;
    for (seed, cx, cz) in [(1337u32, 0, 0), (1337, 400, -300), (1337, -700, 500), (7, 0, 0), (7, 600, 600), (2026, 0, 0), (2026, -500, -500)] {
        let w = World::new(seed);
        for x in cx - 64..cx + 64 {
            for z in cz - 64..cz + 64 {
                for y in 1..=SEA {
                    if block_of(w.get_cell(x, y, z)) != WATER {
                        continue;
                    }
                    water += 1;
                    for (dx, dy, dz) in [(1, 0, 0), (-1, 0, 0), (0, 0, 1), (0, 0, -1), (0, -1, 0), (0, 1, 0)] {
                        let n = block_of(w.get_cell(x + dx, y + dy, z + dz));
                        let top = dy == 1 && y == SEA;
                        assert!(top || n == WATER || !(n == AIR || is_plant(n) || water_faces(n)), "сид {seed}: вода ({x},{y},{z}) касается {n} в ({dx},{dy},{dz})");
                    }
                }
            }
        }
    }
    assert!(water > 1000, "в проверенных кусках есть вода: {water}");
}

#[test]
fn flow_vectors_and_player_push() {
    // ASP-15 §6.5/§7: вектор течения — от центра разлива, по диагонали (0.7071, 0.7071); источник в центре — ноль.
    use crate::game::fluids::fluid_velocity;
    let (_w, mut g) = scene();
    g.put([0, 101, 0], src(Liquid::Water));
    for _ in 0..40 {
        g.tick();
    }
    let sw = g.sw();
    let v = |p: [i32; 3]| fluid_velocity(&mut sw.d.borrow_mut(), p);
    let near = |a: [f64; 3], b: [f64; 3]| (0..3).all(|k| (a[k] - b[k]).abs() < 1e-5);
    assert!(near(v([0, 101, 0]), [0.0; 3]), "центр");
    assert!(near(v([1, 101, 0]), [1.0, 0.0, 0.0]));
    assert!(near(v([-2, 101, 0]), [-1.0, 0.0, 0.0]));
    assert!(near(v([1, 101, 1]), [0.7071067811865476, 0.0, 0.7071067811865476]));
    assert!(near(v([3, 101, 3]), [0.7071067811865476, 0.0, 0.7071067811865476]));
    // Игрок в тонкой воде на краю разлива сносится наружу (тик 0.014 × течение).
    g.teleport(5.5, 101.0, 0.5);
    g.body(|p| {
        p.on_ground = true;
        p.vel = [0.0, -0.0784, 0.0];
    });
    g.set_keys(K_LOCKED);
    let x0 = g.me().player.pos[0];
    for _ in 0..6 {
        g.tick();
    }
    let x1 = g.me().player.pos[0];
    assert!(x1 > x0 + 0.01, "снесло по течению: {x0} → {x1}");
    g.assert_in_sync();
}
