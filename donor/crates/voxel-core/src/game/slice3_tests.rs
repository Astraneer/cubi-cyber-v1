//! Проверки среза 3A (только для cargo test): тики блоков и зона, падающие блоки (ASP-05 §5, §10),
//! опад листвы, факел и лестница (ставление, опора), лазанье, сундук и печь в мире и в окне (ASP-10 §9), рассыпание,
//! случайная выборка вокруг игрока, путь «каменной киркой железо → печь → железная кирка». Мир и сущности решает
//! сервер (Game — пара клиента и локального сервера): прямые правки идут в мир сервера между тиками (Game::put,
//! interact, break_block — с синхронизацией клиента), w — мир клиента (зеркало, им же рисует рендер).

use super::*;
use crate::abi::*;
use crate::cell::*;
use crate::game::crafting::RecipeBook;
use crate::game::entity::*;
use crate::game::screen::UiEv;
use crate::game::interact::*;
use crate::registry::blocks::*;
use crate::registry::ids::*;
use crate::registry::items::*;
use crate::world::blockentity::*;
use crate::world::stack::Container;

/// Сцена высоко над рельефом: пол из камня 9×9 на y = 100, игрок в (0.5, 101, 0.5), взгляд на юг (+Z).
fn scene() -> (World, Game) {
    let w = World::new(1337);
    for x in -4..=4 {
        for z in -4..=4 {
            w.set_block(x, 100, z, STONE);
        }
    }
    let mut g = Game::new(&w);
    g.teleport(0.5, 101.0, 0.5);
    g.body(|p| {
        p.on_ground = true;
        p.vel = [0.0, -0.0784, 0.0];
    });
    g.set_look(0.0, 0.0);
    g.tick();
    (w, g)
}
/// Запись в мир «как игра» (обновления соседей, тики, блок-сущности) — в мир сервера, в текущее время; клиент видит
/// правку сразу.
fn put(g: &mut Game, p: [i32; 3], cell: Cell) {
    g.put(p, cell);
}
/// ПКМ по грани блока (interactBlock главной рукой, сервер) с предметом item в руке (кладётся в хотбар 0).
fn use_on(g: &mut Game, item: ItemId, pos: [i32; 3], face: u8) -> Use {
    if item != 0 {
        g.set_slot(0, ItemStack::of(item, 64.min(idef(item).max_stack as i32)));
    }
    g.select(0);
    let point = [pos[0] as f64 + 0.5, pos[1] as f64 + 0.5, pos[2] as f64 + 0.5];
    g.interact(0, Hit { pos, face, point, inside: false })
}
fn cell(w: &World, p: [i32; 3]) -> Cell {
    w.get_cell(p[0], p[1], p[2])
}
fn items_of(g: &Game, it: ItemId) -> Vec<&ItemEntity> {
    g.sim().items.list.iter().filter(|e| !e.removed && e.stack.item == it).collect()
}

#[test]
fn e12_sand_column_and_fall_times() {
    // Столб из 3 песков на земле; опору убрали в тик T → блоки встают в T + 8, T + 10, T + 12 (ASP-05 §10 п. 7).
    let (w, mut g) = scene();
    w.set_block(3, 101, 3, DIRT);
    for y in 102..=104 {
        w.set_block(3, y, 3, SAND);
    }
    put(&mut g, [3, 101, 3], AIR as Cell);
    let mut landed = Vec::new();
    let mut was: Vec<bool> = (101..=103).map(|y| w.get_block(3, y, 3) == SAND).collect();
    for t in 1..=20 {
        g.tick();
        for (k, y) in (101..=103).enumerate() {
            let now = w.get_block(3, y, 3) == SAND;
            if now && !was[k] {
                landed.push((t, y));
            }
            was[k] = now;
        }
    }
    assert_eq!(landed, vec![(8, 101), (10, 102), (12, 103)]);
    assert_eq!((w.get_block(3, 104, 3), g.sim().falling.len(), g.sim().items.list.len()), (AIR, 0, 0));
    // Высота падения → тиков хода до установки: 1 → 7, 2 → 10, 5 → 17, 10 → 24, 20 → 35 (первый ход — в тике срыва,
    // срыв — через 2 тика после ставления).
    for (h, moves) in [(1, 7), (2, 10), (5, 17), (10, 24), (20, 35)] {
        let (w, mut g) = scene();
        put(&mut g, [2, 101 + h, -2], SAND as Cell);
        let mut t = 0;
        while w.get_block(2, 101, -2) != SAND {
            g.tick();
            t += 1;
            assert!(t < 100, "не упал с {h}");
        }
        assert_eq!(t, moves + 1, "падение с {h}");
    }
    // Предельная скорость: 0.04·0.98/0.02 = 1.96 б/т.
    let (_w, mut g) = scene();
    put(&mut g, [3, 127, 3], GRAVEL as Cell);
    for _ in 0..30 {
        g.tick();
    }
    let v = g.sim().falling[0].vel[1];
    let want = -1.96 * (1.0 - 0.98f64.powi(29)); // 29 ходов: v = −1.96·(1 − 0.98ⁿ) → предел 1.96
    assert!((v - want).abs() < 1e-9, "скорость {v} ≠ {want}");
}

#[test]
fn e12_sand_over_torch_and_scheduled_dedup() {
    // Песок над факелом не падает; падая на факел — выпадает предметом «песок» (гравий — гравием), факел цел.
    let (w, mut g) = scene();
    w.set_block(1, 101, -3, TORCH);
    put(&mut g, [1, 102, -3], SAND as Cell);
    put(&mut g, [2, 101, -3], TORCH as Cell);
    put(&mut g, [2, 104, -3], GRAVEL as Cell);
    for _ in 0..40 {
        g.tick();
    }
    assert_eq!(w.get_block(1, 102, -3), SAND, "песок над факелом висит");
    assert_eq!((w.get_block(2, 101, -3), w.get_block(2, 102, -3), w.get_block(2, 104, -3)), (TORCH, AIR, AIR));
    let gr = items_of(&g, GRAVEL as ItemId);
    assert_eq!(gr.len(), 1, "гравий выпал предметом");
    assert!(gr[0].pickup_delay <= DELAY_BLOCK);
    // Запланированный тик не дублируется (клетка, блок); срабатывает только если блок тот же.
    let (w, mut g) = scene();
    w.set_block(-2, 101, -2, SAND);
    let s = &mut g.server.sim.sim;
    s.ticks.schedule(s.time, [-2, 101, -2], SAND, 2);
    s.ticks.schedule(s.time, [-2, 101, -2], SAND, 5);
    assert_eq!(g.sim().ticks.len(), 1);
    w.set_block(-2, 101, -2, DIRT); // блок сменился — тик пропадёт
    g.tick();
    g.tick();
    assert_eq!((g.sim().ticks.len(), g.sim().falling.len()), (0, 0));
}

#[test]
fn e13_zone_freezes_falling_and_furnace() {
    // Вне зоны 9×9 кусков песок висит в воздухе (сущность стоит), печь не тикает; вернулись — продолжают.
    let (w, mut g) = scene();
    put(&mut g, [3, 110, 3], SAND as Cell);
    for _ in 0..4 {
        g.tick();
    }
    assert_eq!(g.sim().falling.len(), 1);
    let y0 = g.sim().falling[0].pos[1];
    g.teleport(80.5, 101.0, 0.5); // кусок игрока (5, 0): кусок песка (0, 0) — вне зоны
    for _ in 0..10 {
        g.tick();
    }
    assert_eq!(g.sim().falling[0].pos[1], y0, "вне зоны стоит");
    g.teleport(0.5, 101.0, 0.5);
    for _ in 0..30 {
        g.tick();
    }
    assert_eq!(w.get_block(3, 101, 3), SAND);
}

#[test]
fn leaves_decay_bfs_and_random_ticks() {
    // Ствол 4 и крона 5×5 в два слоя (как генератор — состояние 0); срубили ствол — вся листва «опадает»; бревно,
    // поставленное в крону, возвращает её; убрали — опадает случайными тиками: за 1365 тиков ≈ 63 % (1 − e⁻¹).
    let (w, mut g) = scene();
    let (x, z) = (12, 12);
    for y in 101..=104 {
        w.set_block(x, y, z, LOG);
    }
    let mut leaves = Vec::new();
    for y in 104..=105 {
        for dx in -2..=2 {
            for dz in -2..=2 {
                if (dx, dz) != (0, 0) || y == 105 {
                    w.set_block(x + dx, y, z + dz, LEAVES);
                    leaves.push([x + dx, y, z + dz]);
                }
            }
        }
    }
    let decaying = |w: &World| leaves.iter().filter(|p| state_of(cell(w, **p)) & ST_DECAY != 0).count();
    put(&mut g, [x, 101, z], AIR as Cell);
    assert_eq!(decaying(&w), 0, "лист ещё держится за бревно 102…104");
    for y in 102..=104 {
        put(&mut g, [x, y, z], AIR as Cell);
    }
    assert_eq!(decaying(&w), leaves.len(), "без бревна — вся крона");
    put(&mut g, [x, 104, z], cell_of(LOG, 1)); // лежачее бревно в кроне
    assert_eq!(decaying(&w), 0);
    // Лист игрока не опадает и держит соседей как обычная листва (DISTANCE считается и через него).
    put(&mut g, [x, 104, z], AIR as Cell);
    assert_eq!(decaying(&w), leaves.len());
    let n = leaves.len() as f64;
    for _ in 0..1365 {
        g.tick();
    }
    let left = leaves.iter().filter(|p| is_leaves(w.get_block(p[0], p[1], p[2]))).count() as f64;
    let gone = 1.0 - left / n;
    assert!(gone > 0.4 && gone < 0.85, "опало {gone:.2} из {n}");
    // Поставленная игроком листва не опадает: бит persistent.
    let (w, mut g) = scene();
    assert_eq!(use_on(&mut g, LEAVES as ItemId, [3, 100, 3], F_UP), Use::Swing);
    assert_eq!(cell(&w, [3, 101, 3]), cell_of(LEAVES, ST_PERSIST | ST_DECAY), "далеко от бревна — бит есть, но лист вечный");
    g.on_server(|sv, _| {
        let d = sv.sim.world.d.clone();
        sv.sim.random_tick(&mut d.borrow_mut(), [3, 101, 3], cell_of(LEAVES, ST_PERSIST | ST_DECAY), &mut sv.events);
    });
    assert_eq!(w.get_block(3, 101, 3), LEAVES);
}

#[test]
fn torch_wall_floor_and_support() {
    // ASP-05 §10 п. 6: верх камня → стоячий; северная грань камня (клик с севера) → настенный FACING = NORTH;
    // верх листвы без стен → не ставится; убрали опору — факел выпадает предметом.
    let (w, mut g) = scene();
    w.set_block(0, 101, 3, STONE);
    assert_eq!(use_on(&mut g, TORCH as ItemId, [0, 101, 3], F_UP), Use::Swing);
    assert_eq!(cell(&w, [0, 102, 3]), TORCH as Cell);
    assert_eq!(use_on(&mut g, TORCH as ItemId, [0, 101, 3], F_NORTH), Use::Swing);
    assert_eq!(cell(&w, [0, 101, 2]), cell_of(WALL_TORCH, F_NORTH as State));
    w.set_block(3, 106, 3, LEAVES); // в воздухе: ни пола, ни стен рядом
    assert_eq!(use_on(&mut g, TORCH as ItemId, [3, 106, 3], F_UP), Use::Fail);
    assert_eq!(use_on(&mut g, TORCH as ItemId, [3, 106, 3], F_NORTH), Use::Fail, "на листву и сбоку нельзя");
    // Клик по боку листвы над полом: стены нет, но пол есть — встаёт стоячим на пол (порядок направлений).
    w.set_block(3, 101, 3, LEAVES);
    assert_eq!(use_on(&mut g, TORCH as ItemId, [3, 101, 3], F_NORTH), Use::Swing);
    assert_eq!(cell(&w, [3, 101, 2]), TORCH as Cell);
    // Клик по низу блока (потолок): клетка под ним, пол — камень → стоячий.
    w.set_block(-3, 103, 3, STONE);
    assert_eq!(use_on(&mut g, TORCH as ItemId, [-3, 103, 3], F_DOWN), Use::Fail, "под потолком воздух до пола 101: опоры нет");
    w.set_block(-3, 101, 3, STONE);
    assert_eq!(use_on(&mut g, TORCH as ItemId, [-3, 103, 3], F_DOWN), Use::Swing);
    assert_eq!(cell(&w, [-3, 102, 3]), TORCH as Cell);
    // Цепочка соседей (п. 9): камень с факелом сверху и лестницей сбоку — оба выпадают в одном тике.
    let (w, mut g) = scene();
    w.set_block(0, 101, 3, STONE);
    use_on(&mut g, TORCH as ItemId, [0, 101, 3], F_UP);
    use_on(&mut g, LADDER as ItemId, [0, 101, 3], F_NORTH);
    assert_eq!(cell(&w, [0, 101, 2]), cell_of(LADDER, F_NORTH as State));
    put(&mut g, [0, 101, 3], AIR as Cell);
    assert_eq!((w.get_block(0, 102, 3), w.get_block(0, 101, 2)), (AIR, AIR));
    assert_eq!((items_of(&g, TORCH as ItemId).len(), items_of(&g, LADDER as ItemId).len()), (1, 1));
}

#[test]
fn ladder_placement_rules_and_climb() {
    let (w, mut g) = scene();
    // На стену — лицом к игроку; на листву нельзя; лицевой стороной лестницы лестницу не поставить; блок под
    // лестницей ей не нужен.
    for y in 101..=110 {
        w.set_block(0, y, 1, STONE);
    }
    w.set_block(2, 101, 1, LEAVES);
    assert_eq!(use_on(&mut g, LADDER as ItemId, [2, 101, 1], F_NORTH), Use::Fail);
    assert_eq!(use_on(&mut g, LADDER as ItemId, [0, 101, 1], F_NORTH), Use::Swing);
    assert_eq!(cell(&w, [0, 101, 0]), cell_of(LADDER, F_NORTH as State));
    assert_eq!(use_on(&mut g, LADDER as ItemId, [0, 101, 0], F_NORTH), Use::Fail, "по лицу лестницы");
    // Клик по верху лестницы — новая выше на ту же стену (взгляд на юг).
    assert_eq!(use_on(&mut g, LADDER as ItemId, [0, 101, 0], F_UP), Use::Swing);
    assert_eq!(cell(&w, [0, 102, 0]), cell_of(LADDER, F_NORTH as State));
    for y in 103..=110 {
        w.set_cell(0, y, 0, cell_of(LADDER, F_NORTH as State));
    }
    // Лазанье: упор в стену (W) — вверх 0.2 после хода, т. е. 0.1176 б/т; без ввода — вниз 0.15 б/т; Shift — стоит.
    g.teleport(0.5, 101.0, 0.5);
    g.body(|p| p.on_ground = true);
    g.set_look(0.0, 0.0);
    g.set_keys(K_LOCKED | K_FWD);
    let mut ys = Vec::new();
    for _ in 0..30 {
        g.tick();
        ys.push(g.ctl().player.pos[1]);
    }
    let dy = ys[20] - ys[19];
    assert!((dy - 0.1176).abs() < 1e-6, "подъём {dy}");
    assert_eq!(g.ctl().player.fall, 0.0);
    g.set_keys(K_LOCKED);
    for _ in 0..5 {
        g.tick();
    }
    let y = g.ctl().player.pos[1];
    g.tick();
    assert!((g.ctl().player.pos[1] - y + 0.15).abs() < 1e-6, "спуск {}", g.ctl().player.pos[1] - y);
    assert!(g.ctl().player.fall <= 0.15 + 1e-6, "падение не копится: {}", g.ctl().player.fall);
    g.set_keys(K_LOCKED | K_SNEAK);
    g.tick();
    g.tick();
    let y = g.ctl().player.pos[1];
    for _ in 0..10 {
        g.tick();
    }
    assert_eq!(g.ctl().player.pos[1], y, "Shift — держится");
    // Space на лестнице — тоже вверх.
    g.set_keys(K_LOCKED | K_JUMP);
    g.tick();
    g.tick();
    assert!(g.ctl().player.pos[1] > y);
    // Сломали стену за лестницей — лестница выпадает; блок под ней не нужен.
    let (w, mut g) = scene();
    w.set_block(0, 101, 1, STONE);
    w.set_block(0, 102, 1, STONE);
    use_on(&mut g, LADDER as ItemId, [0, 102, 1], F_NORTH);
    assert_eq!(w.get_block(0, 102, 0), LADDER);
    put(&mut g, [0, 101, 0], STONE as Cell);
    put(&mut g, [0, 101, 0], AIR as Cell);
    assert_eq!(w.get_block(0, 102, 0), LADDER, "низ не нужен");
    put(&mut g, [0, 102, 1], AIR as Cell);
    assert_eq!(w.get_block(0, 102, 0), AIR);
}

#[test]
fn snow_layers_stack_and_drop() {
    let (w, mut g) = scene();
    assert_eq!(use_on(&mut g, SNOW_LAYER as ItemId, [2, 100, 2], F_UP), Use::Swing);
    assert_eq!(cell(&w, [2, 101, 2]), SNOW_LAYER as Cell);
    // Снег по снегу — слой +1 (в ту же клетку, по любой грани).
    assert_eq!(use_on(&mut g, SNOW_LAYER as ItemId, [2, 101, 2], F_UP), Use::Swing);
    assert_eq!(snow_layers(cell(&w, [2, 101, 2])), 2);
    // Другим блоком по слою 2 — не заменить (рядом), по слою 1 — заменить.
    assert_eq!(collision_of(cell(&w, [2, 101, 2]))[0].hi[1], 2.0 / 16.0, "коллизия на слой ниже");
    g.set_slot(0, ItemStack::of(I_WOODEN_PICKAXE + 2, 1)); // деревянная лопата
    g.break_block([2, 101, 2]);
    assert_eq!(items_of(&g, I_SNOWBALL).iter().map(|e| e.stack.n()).sum::<i32>(), 2);
    // Без опоры (камень убрали) — исчезает без снежков.
    use_on(&mut g, SNOW_LAYER as ItemId, [-2, 100, -2], F_UP);
    put(&mut g, [-2, 100, -2], AIR as Cell);
    assert_eq!(w.get_block(-2, 101, -2), AIR);
    assert_eq!(items_of(&g, I_SNOWBALL).len(), 1);
}

#[test]
fn e28_chest_open_lid_quick_move_scatter() {
    let (w, mut g) = scene();
    // Ставлю сундук, глядя на юг: лицом на север (ко мне).
    assert_eq!(use_on(&mut g, CHEST as ItemId, [0, 100, 2], F_UP), Use::Swing);
    let p = [0, 101, 2];
    assert_eq!(cell(&w, p), cell_of(CHEST, F_NORTH as State));
    assert!(w.d.borrow().be().chest(p).is_some());
    // ПКМ — окно, звук открытия; крышка за 10 тиков, угол (1 − (1 − p)³)·90°.
    g.take_events();
    assert_eq!(use_on(&mut g, 0, p, F_NORTH), Use::Swing);
    assert_eq!(g.screen_kind(), 3);
    assert!(g.take_events().chunks(5).any(|e| e[0] == EV_CHEST_OPEN && e[1..4] == p));
    let mut prog = Vec::new();
    for _ in 0..11 {
        g.tick();
        prog.push(w.d.borrow().be().chest(p).unwrap().lid.progress);
    }
    assert_eq!(prog[0], 0.1);
    assert_eq!(prog[9], 1.0);
    let md = g.model_draws(0.0, 2);
    assert_eq!(&md[..6], &[0.0, 101.0, 2.0, 0.0, F_NORTH as f32, std::f32::consts::FRAC_PI_2]);
    // Shift из инвентаря — в сундук с начала; из сундука — в хотбар справа.
    g.set_slot(9, ItemStack::of(DIRT as ItemId, 64));
    g.set_slot(10, ItemStack::of(I_STICK, 10));
    g.screen_click(27, 0, A_QUICK_MOVE);
    g.screen_click(28, 0, A_QUICK_MOVE);
    let v = g.screen_view();
    assert_eq!((v[3], v[4], v[6], v[7]), (DIRT as i32, 64, I_STICK as i32, 10));
    g.screen_click(1, 0, A_QUICK_MOVE);
    assert_eq!(g.ctl().inv.main[8], ItemStack::of(I_STICK, 10));
    g.screen_click(62, 0, A_QUICK_MOVE);
    // Esc — звук закрытия (сервер: на границе тика), крышка вниз.
    g.close_screen();
    g.sync();
    assert!(g.take_events().chunks(5).any(|e| e[0] == EV_CHEST_CLOSE));
    g.tick();
    assert!(w.d.borrow().be().chest(p).unwrap().lid.progress < 1.0);
    // Сверху камень — не открыть (взмах есть); листва — открывается.
    w.set_block(0, 102, 2, STONE);
    assert_eq!(use_on(&mut g, 0, p, F_NORTH), Use::Swing);
    assert_eq!(g.screen_kind(), 0);
    w.set_block(0, 102, 2, LEAVES);
    use_on(&mut g, 0, p, F_NORTH);
    assert_eq!(g.screen_kind(), 3);
    g.close_screen();
    assert!(solid_block(STONE as Cell) && !solid_block(LEAVES as Cell) && !solid_block(CHEST as Cell) && !solid_block(cell_of(SNOW_LAYER, 7)) && !solid_block(TORCH as Cell));
    // Ломаю сундук топором: сундук выпал (задержка 10), земля 64 — кусками 10…30, палки — одним, подбор сразу.
    g.on_server(|sv, _| sv.sim.items.list.clear());
    g.set_slot(0, ItemStack::of(I_WOODEN_PICKAXE + 1, 1));
    g.select(0);
    g.break_block(p);
    assert_eq!(w.get_block(p[0], p[1], p[2]), AIR);
    assert!(w.d.borrow().be().get(p).is_none());
    let dirt = items_of(&g, DIRT as ItemId);
    assert!(dirt.len() >= 3 && dirt.len() <= 6, "кусков {}", dirt.len());
    assert_eq!(dirt.iter().map(|e| e.stack.n()).sum::<i32>(), 64);
    assert!(dirt.iter().all(|e| e.stack.n() >= 4 && e.stack.n() <= 30 && e.pickup_delay == 0));
    assert!(dirt.iter().all(|e| e.pos == dirt[0].pos), "одна точка на слот");
    assert!(dirt.iter().all(|e| e.vel[1] >= 0.085 && e.vel[1] <= 0.315));
    let chest = items_of(&g, CHEST as ItemId);
    assert_eq!((chest.len(), chest[0].pickup_delay), (1, DELAY_BLOCK));
    g.assert_in_sync(); // окно сундука через сеть: хеши кликов сошлись, мир и инвентарь у клиента — как у сервера
}

#[test]
fn e29_scatter_pieces() {
    let mut items = Items::new();
    let mut rng = GameRng(99);
    for _ in 0..200 {
        items.list.clear();
        items.scatter([0, 64, 0], ItemStack::of(DIRT as ItemId, 64), &mut rng, &mut Ids(1));
        let n: Vec<i32> = items.list.iter().map(|e| e.stack.n()).collect();
        assert_eq!(n.iter().sum::<i32>(), 64);
        assert!(n.len() >= 3 && n.len() <= 7);
        assert!(n[..n.len() - 1].iter().all(|&k| (10..=30).contains(&k)), "{n:?}");
        let p = items.list[0].pos;
        assert!(p[0] >= 0.125 && p[0] <= 0.875 && p[1] >= 64.0 && p[1] <= 64.75);
    }
}

/// Окно печи над своей печью и инвентарём (screen.rs + blockentity.rs).
struct FW {
    inv: Inventory,
    book: RecipeBook,
    ev: Vec<UiEv>,
    f: Furnace,
    m: Menu,
}
impl FW {
    fn new() -> FW {
        FW { inv: Inventory::new(), book: RecipeBook::standard(), ev: Vec::new(), f: Furnace::default(), m: Menu::furnace([0, 0, 0]) }
    }
    fn click(&mut self, slot: i32, button: i32, action: i32) {
        let mut u = Ui { inv: &mut self.inv, block: Some(&mut self.f), book: &self.book, ev: &mut self.ev, alive: true, creative: false, locked: false };
        self.m.click(&mut u, slot, button, action);
    }
}

#[test]
fn e18_e24_furnace_window() {
    let st = ItemStack::of;
    // Бревно Shift-кликом — во вход (плавится), а не в топливо; уголь и доски — в топливо; земля — рюкзак ↔ хотбар.
    let mut t = FW::new();
    t.inv.main[9] = st(LOG as ItemId, 4);
    t.inv.main[10] = st(I_COAL, 3);
    t.inv.main[11] = st(PLANKS as ItemId, 5);
    t.inv.main[12] = st(DIRT as ItemId, 7);
    t.click(3, 0, A_QUICK_MOVE);
    t.click(4, 0, A_QUICK_MOVE);
    assert_eq!((t.f.slots[FURNACE_IN], t.f.slots[FURNACE_FUEL]), (st(LOG as ItemId, 4), st(I_COAL, 3)));
    t.click(5, 0, A_QUICK_MOVE);
    assert_eq!(t.inv.main[11], st(PLANKS as ItemId, 5), "топливо занято углём — доски остаются");
    t.click(6, 0, A_QUICK_MOVE);
    assert_eq!(t.inv.main[0], st(DIRT as ItemId, 7));
    // 18: во входе железо — бревно никуда; 19: топливо 64 угля — уголь никуда.
    let mut t = FW::new();
    t.f.slots[FURNACE_IN] = st(I_RAW_IRON, 1);
    t.f.slots[FURNACE_FUEL] = st(I_COAL, 64);
    t.inv.main[9] = st(LOG as ItemId, 4);
    t.inv.main[10] = st(I_COAL, 3);
    t.click(3, 0, A_QUICK_MOVE);
    t.click(4, 0, A_QUICK_MOVE);
    assert_eq!((t.inv.main[9].n(), t.inv.main[10].n()), (4, 3));
    // 20: пустое ведро Shift-ом — рюкзак → хотбар; руками в топливо — можно, по одному.
    let mut t = FW::new();
    t.inv.main[9] = st(I_BUCKET, 3);
    t.click(3, 0, A_QUICK_MOVE);
    assert_eq!(t.inv.main[0], st(I_BUCKET, 3));
    t.click(30, 0, A_PICKUP);
    t.click(1, 0, A_PICKUP);
    assert_eq!((t.f.slots[FURNACE_FUEL], t.m.cursor.n()), (st(I_BUCKET, 1), 2));
    // Земля в топливо не кладётся; в выход — ничего.
    let mut t = FW::new();
    t.m.cursor = st(DIRT as ItemId, 5);
    t.click(1, 0, A_PICKUP);
    t.click(2, 0, A_PICKUP);
    assert!(t.f.slots[FURNACE_FUEL].is_empty() && t.f.slots[FURNACE_OUT].is_empty() && t.m.cursor.n() == 5);
    // 21: выход Shift-ом — в хотбар справа (окно 38 = хотбар 8).
    let mut t = FW::new();
    t.f.slots[FURNACE_OUT] = st(I_IRON_INGOT, 8);
    t.click(2, 0, A_QUICK_MOVE);
    assert_eq!(t.inv.main[8], st(I_IRON_INGOT, 8));
    // 22: курсор 60 слитков, выход 8 — ничего; 23: ПКМ пустым курсором по 9 — курсор 5, выход 4; 24: Q — один.
    let mut t = FW::new();
    t.f.slots[FURNACE_OUT] = st(I_IRON_INGOT, 8);
    t.m.cursor = st(I_IRON_INGOT, 60);
    t.click(2, 0, A_PICKUP);
    assert_eq!((t.m.cursor.n(), t.f.slots[FURNACE_OUT].n()), (60, 8));
    t.m.cursor = st(I_IRON_INGOT, 50);
    t.click(2, 0, A_PICKUP);
    assert_eq!((t.m.cursor.n(), t.f.slots[FURNACE_OUT].n()), (58, 0), "весь выход влез — берётся");
    let mut t = FW::new();
    t.f.slots[FURNACE_OUT] = st(I_IRON_INGOT, 9);
    t.click(2, 1, A_PICKUP);
    assert_eq!((t.m.cursor.n(), t.f.slots[FURNACE_OUT].n()), (5, 4));
    let mut t = FW::new();
    t.f.slots[FURNACE_OUT] = st(I_IRON_INGOT, 9);
    t.click(2, 0, A_THROW);
    assert_eq!(t.f.slots[FURNACE_OUT].n(), 8);
    assert!(matches!(t.ev[..], [UiEv::Drop { stack, .. }] if stack == st(I_IRON_INGOT, 1)));
    // Сброс прогресса через окно: забрал половину входа — цел, весь — с нуля, вернул — с нуля.
    let mut t = FW::new();
    t.f.set(FURNACE_IN, st(I_RAW_IRON, 8));
    t.f.cook = 120;
    t.click(0, 1, A_PICKUP);
    assert_eq!((t.f.cook, t.f.slots[FURNACE_IN].n()), (120, 4));
    t.click(0, 0, A_PICKUP); // положить курсор обратно (тот же предмет)
    assert_eq!(t.f.cook, 120);
    t.click(0, 0, A_PICKUP); // взять всё
    assert_eq!(t.f.cook, 0);
}

#[test]
fn furnace_in_world_lit_and_window() {
    let (w, mut g) = scene();
    assert_eq!(use_on(&mut g, FURNACE as ItemId, [0, 100, 2], F_UP), Use::Swing);
    let p = [0, 101, 2];
    assert_eq!(cell(&w, p), cell_of(FURNACE, F_NORTH as State));
    g.set_slot(0, ItemStack::EMPTY);
    assert_eq!(use_on(&mut g, 0, p, F_NORTH), Use::Swing);
    assert_eq!(g.screen_kind(), 4);
    // Вход — 3 сырого железа, топливо — уголь (окно: хотбар 30…38).
    g.set_slot(1, ItemStack::of(I_RAW_IRON, 3));
    g.set_slot(2, ItemStack::of(I_COAL, 1));
    g.screen_click(31, 0, A_PICKUP);
    g.screen_click(0, 0, A_PICKUP);
    g.screen_click(32, 0, A_PICKUP);
    g.screen_click(1, 0, A_PICKUP);
    g.tick();
    assert_eq!(g.furnace_view(), vec![14, 1], "огонь и стрелка — в одном тике");
    assert_eq!(state_of(cell(&w, p)) & ST_LIT, ST_LIT, "горит — LIT в мире, сторона цела");
    assert_eq!(facing_of(cell(&w, p)), F_NORTH);
    // Окно закрыл, отошёл на 20 блоков (в зоне) — плавка идёт.
    g.close_screen();
    g.teleport(0.5, 101.0, 20.5);
    for _ in 0..599 {
        g.tick();
    }
    let f = *g.sw().d.borrow().be().furnace(p).unwrap(); // печь — у сервера (клиенту её числа идут, пока открыто окно)
    assert_eq!((f.slots[FURNACE_OUT], f.slots[FURNACE_IN].n()), (ItemStack::of(I_IRON_INGOT, 3), 0));
    // Частицы и треск у горящей печи (случайная выборка вокруг игрока): стою в 2 блоках — ≈ 3 раза в секунду.
    g.teleport(2.5, 101.0, 2.5);
    g.take_events();
    let mut fx = 0;
    let mut crackle = 0;
    for _ in 0..200 {
        g.tick();
        for e in g.take_events().chunks(5) {
            fx += (e[0] == EV_FURNACE_FX) as i32;
            crackle += (e[0] == EV_FURNACE_CRACKLE) as i32;
            if e[0] == EV_FURNACE_FX {
                // Точка — у северной (лицевой) грани: z = 2.5 − 0.52, низ блока + 0…6/16.
                assert!((e[3] - 1980).abs() <= 1 && e[2] >= 101000 && e[2] <= 101375 && (e[1] - 500).abs() <= 300, "{e:?}");
            }
        }
    }
    assert!(fx > 15 && fx < 60, "частиц {fx}");
    assert!(crackle < fx);
    // Ломаю печь рукой: печь не выпала, вещи (3 слитка) — сразу подбираемы; LIT ушёл вместе с блоком.
    g.on_server(|sv, _| sv.sim.items.list.clear());
    g.select(8);
    g.break_block(p);
    assert!(items_of(&g, FURNACE as ItemId).is_empty());
    assert_eq!(items_of(&g, I_IRON_INGOT).iter().map(|e| (e.stack.n(), e.pickup_delay)).collect::<Vec<_>>(), vec![(3, 0)]);
    g.assert_in_sync();
}

#[test]
fn e30_display_sampling_rate() {
    // Горящая печь в клетке ступней: 0.1832 вызова за тик (667 точек r 16 и 667 — r 32, ASP-10 §9 п. 30).
    let (w, mut g) = scene();
    w.set_cell(0, 101, 0, cell_of(FURNACE, F_NORTH as State | ST_LIT));
    w.d.borrow_mut().be_mut().furnace_mut([0, 101, 0]).unwrap().burn = 100_000; // горит (выборка — только при горящих)
    let n = 3000;
    let mut calls = 0;
    g.take_events();
    for _ in 0..n {
        g.client.sim.tick_visual();
        calls += g.take_events().chunks(5).filter(|e| e[0] == EV_FURNACE_FX).count();
    }
    let rate = calls as f64 / n as f64;
    assert!((rate - 0.1832).abs() < 0.025, "вызовов за тик {rate:.4}");
}

#[test]
fn path_stone_pickaxe_to_iron_pickaxe() {
    // Каменной киркой железная руда → сырое железо (23 тика); 3 руды + уголь → печь → 3 слитка (600 тиков) →
    // железная кирка на верстаке.
    let (w, mut g) = scene();
    g.set_slot(0, ItemStack::of(I_WOODEN_PICKAXE + 5, 1));
    g.select(0);
    g.set_look(0.0, 30.0);
    for k in 0..3 {
        w.set_block(0, 101, 1, IRON_ORE);
        g.set_keys(K_LOCKED);
        g.tick();
        g.set_keys(K_LOCKED | K_ATTACK);
        g.press(P_ATTACK);
        let mut t = 0;
        while w.get_block(0, 101, 1) != AIR {
            g.tick();
            t += 1;
            assert!(t < 100);
        }
        assert_eq!(t, if k == 0 { 23 } else { 23 + 5 }, "руда каменной киркой");
        g.set_keys(K_LOCKED);
        for _ in 0..40 {
            g.tick();
        }
    }
    // Укатившееся — подобрать (встать на предмет).
    for _ in 0..20 {
        let Some(p) = g.sim().items.list.first().map(|e| e.pos) else { break };
        g.teleport(p[0], 101.0, p[2]);
        g.tick();
    }
    g.teleport(0.5, 101.0, 0.5);
    let raw: i32 = g.ctl().inv.main.iter().filter(|s| s.item == I_RAW_IRON).map(|s| s.n()).sum();
    assert_eq!(raw, 3, "подобрано сырое железо");
    // Печь (у сервера): вход — сырое железо, топливо — уголь.
    put(&mut g, [0, 101, 2], cell_of(FURNACE, F_NORTH as State));
    let f = [0, 101, 2];
    let d = g.sw().d.clone();
    {
        let mut wd = d.borrow_mut();
        let fu = wd.be_mut().furnace_mut(f).unwrap();
        fu.set(FURNACE_IN, ItemStack::of(I_RAW_IRON, 3));
        fu.set(FURNACE_FUEL, ItemStack::of(I_COAL, 1));
    }
    for _ in 0..600 {
        g.tick();
    }
    let out = d.borrow().be().furnace(f).unwrap().slots[FURNACE_OUT];
    assert_eq!(out, ItemStack::of(I_IRON_INGOT, 3), "3 слитка: третий — на 600-м тике с поджига (30 с)");
    // Железная кирка: слитки — верхний ряд, палки — 5 и 8.
    g.set_slot(3, out);
    g.set_slot(4, ItemStack::of(I_STICK, 2));
    put(&mut g, [1, 101, 1], CRAFTING_TABLE as Cell);
    g.open_screen(Menu::crafting([1, 101, 1]));
    g.screen_click(37 + 3, 0, A_PICKUP);
    for c in 1..=3 {
        g.screen_click(c, 1, A_PICKUP);
    }
    g.screen_click(37 + 4, 0, A_PICKUP);
    g.screen_click(5, 1, A_PICKUP);
    g.screen_click(8, 1, A_PICKUP);
    g.screen_click(0, 0, A_QUICK_MOVE);
    assert!(g.ctl().inv.main.iter().any(|s| s.item == I_WOODEN_PICKAXE + 10), "железная кирка");
    // Как у прежней игры (до разделения, libm), без поправок и откатов. Окно верстака у клиента ещё открыто: последние
    // клики — в очереди до тика.
    g.sync();
    g.assert_in_sync();
    let fp = super::game_tests::fingerprint(&g);
    if super::game_tests::fnv(&fp) != FP_PATH_IRON {
        std::fs::write(std::env::temp_dir().join("fp_path_iron_new.txt"), &fp).unwrap();
    }
    assert_eq!(super::game_tests::fnv(&fp), FP_PATH_IRON, "отпечаток разошёлся с прежней игрой (fp_path_iron_new.txt)");
}
/// Отпечаток пути «каменная → железная кирка» у прежней игры.
const FP_PATH_IRON: u64 = 0x890b_1551_58c4_e5a6;

#[test]
#[ignore]
fn tick_cost() {
    // Цена тика игры с горящей печью рядом (случайная выборка 2 × 667), правками в зоне и сундуком: мкс на тик.
    let (w, mut g) = scene();
    put(&mut g, [2, 101, 2], cell_of(FURNACE, F_NORTH as State));
    g.sw().d.borrow_mut().be_mut().furnace_mut([2, 101, 2]).unwrap().burn = 1_000_000;
    let _ = &w;
    put(&mut g, [3, 101, 2], cell_of(CHEST, F_NORTH as State));
    for k in 0..200 {
        put(&mut g, [k % 9 - 4, 102 + k / 81, (k / 9) % 9 - 4], cell_of(LEAVES, ST_DECAY));
    }
    for _ in 0..100 {
        g.tick();
    }
    let t = std::time::Instant::now();
    for _ in 0..2000 {
        g.tick();
        g.take_events();
    }
    println!("TICK {:.1} мкс на тик", t.elapsed().as_secs_f64() * 1e6 / 2000.0);
}

#[test]
fn cactus_breaks_next_tick() {
    // Кактус на песке; поставили блок вплотную сбоку — через 1 тик кактус ломается с дропом (CactusBlock).
    let (w, mut g) = scene();
    w.set_block(2, 101, 2, SAND);
    w.set_block(2, 102, 2, CACTUS);
    put(&mut g, [3, 102, 2], DIRT as Cell);
    assert_eq!(w.get_block(2, 102, 2), CACTUS, "в этом же тике ещё стоит");
    assert!(g.sim().ticks.contains([2, 102, 2], CACTUS));
    g.tick();
    assert_eq!(w.get_block(2, 102, 2), AIR);
    assert_eq!(items_of(&g, CACTUS as ItemId).len(), 1);
}

#[test]
fn placement_directions_like_java() {
    // Direction.getEntityFacingOrder: взгляд на юг и чуть вниз — S, D, E/W…; прямо вниз — D первым.
    assert_eq!(facing_order(0.0, 10.0)[0], F_SOUTH);
    assert_eq!(facing_order(0.0, 89.0)[0], F_DOWN);
    assert_eq!(facing_order(90.0, 0.0)[0], F_WEST);
    assert_eq!(facing_order(-90.0, -60.0)[0], F_UP);
    let o = facing_order(30.0, 20.0);
    assert_eq!(o[5], o[0] ^ 1);
    // ASP-05 §10 п. 4: горизонтальное направление и лицо печи.
    for (yaw, f) in [(0.0, F_SOUTH), (44.9, F_SOUTH), (45.0, F_WEST), (135.0, F_NORTH), (-45.0, F_SOUTH), (-45.1, F_EAST), (270.0, F_EAST), (360.0, F_SOUTH)] {
        assert_eq!(horizontal_facing(yaw), f, "yaw {yaw}");
    }
    // Клик по грани, кликнутая клетка не заменяется: «в кликнутый блок» — первым.
    let d = placement_dirs(0.0, 0.0, false, F_EAST);
    assert_eq!(d[0], F_WEST);
}

#[test]
fn mesh_reads_orientation() {
    // Печь: лицо по FACING и «горит»; бревно X — торцы на ±X, кора повёрнута; модели (лестница, сундук) мешем не
    // рисуются и соседей не закрывают.
    let face = |c: Cell, f: usize| layer_cell(block_of(c), state_of(c), f);
    assert_eq!(face(cell_of(FURNACE, F_EAST as State), 0).0, L_FURNACE_FRONT);
    assert_eq!(face(cell_of(FURNACE, F_EAST as State | ST_LIT), 0).0, L_FURNACE_FRONT_ON);
    assert_eq!(face(cell_of(FURNACE, F_EAST as State), 5).0, L_FURNACE_SIDE);
    assert_eq!(face(cell_of(LOG, 1), 0), (L_LOG_TOP, false));
    assert_eq!(face(cell_of(LOG, 1), 2), (L_LOG, true));
    assert_eq!(face(cell_of(LOG, 2), 0), (L_LOG, true));
    assert_eq!(face(cell_of(LOG, 2), 2), (L_LOG, false));
    assert_eq!(face(LOG as Cell, 4), (L_LOG, false));
    assert!(!opaque(CHEST) && !opaque(LADDER) && !opaque(WALL_TORCH) && opaque(FURNACE));
    let w = World::new(1337);
    w.set_block(3, 120, 3, STONE);
    let before = w.mesh_chunk(0, 0, 1);
    w.set_cell(4, 120, 3, cell_of(LADDER, F_EAST as State));
    let after = w.mesh_chunk(0, 0, 1);
    assert_eq!((before.solid.len(), before.cutout.len()), (after.solid.len(), after.cutout.len()), "лестница — не в меше, грань камня к ней есть");
    w.set_cell(5, 120, 3, cell_of(LOG, 1));
    let m = w.mesh_chunk(0, 0, 1);
    assert!(m.solid.chunks(2).any(|v| v[0] >> 31 == 1), "поворот коры — w0 бит 31");
}
