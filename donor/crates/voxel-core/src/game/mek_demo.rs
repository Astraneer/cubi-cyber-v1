//! Площадки приёмки Mekanism (DebugCmd::Build, сцены web/scenes/mek_*.json, проверки): строятся у ног игрока
//! дорогой игры (Sim::set_world — рукава кабелей, сети, блок-сущности), только с отладкой.
//!
//! Раскладка от клетки ступней P (игрок смотрит на север, −Z):
//!   G = P + (0, 0, −3) — место теплового генератора: лава на западе, востоке, севере и снизу (4 грани → 52 Дж/т), лицо
//!   (порт энергии) — на юг, к игроку; кабель — от G + (0, 0, 1) на восток 10 штук; инфузор — в конце линии.
//!   1 «сталь»: только площадка с лавой и набор (генератор, инфузор, 16 кабелей, уголь, железо, печь) — ставит игрок;
//!   2 «линия»: всё уже стоит — генератор, кабели, инфузор с углём и железом (проверки, сцена mek_line);
//!   3 «генератор»: только тепловой генератор в лаве, без кабеля — копит энергию у себя (пуля, сцена mek_heat).
//!   4 «обогащение» (M2, сцена mek_enrich): в ряд на z − 3 лицом к игроку — обогатительная камера (запад, x − 1), кабель,
//!     плавильня (восток, x + 1); за кабелем полный куб энергии лицом к кабелю. В камере 9 сырого железа; набор — сырьё,
//!     уголь, гравий, ещё по камере и плавильне.
//!   5 «ветер» (M3, сцена mek_wind): площадка 11 × 11 у ног; ветряк «на горе» — у ног, на z − 3 лицом к игроку; ветряк
//!     «у моря» — основание на Y 49 в 4 блоках западнее (шахта над ним до неба). Справа на z − 2 лицом к игроку — линия
//!     «склад»: обогатительная камера (x + 2, 9 сырого железа, 4 улучшения скорости) → плавильня (x + 3) → сундук (x + 4);
//!     правая грань — выход, выталкивание включено. Энергия — кабелем в полу (y − 1) от низа ветряка под камеру и
//!     плавильню, на кабеле полный куб энергии (x + 1, y − 1, z − 1). Набор: ветряк, планшет, кабель, конфигуратор,
//!     улучшения скорости и энергии.
//!   6 «удвоение» (M4, сцена mek_double): ряд на z − 3 лицом к игроку — сепаратор, очиститель, дробилка, камера,
//!     плавильня, сундук (double_layout); 10 сырого железа во входе очистителя → 20 слитков в сундуке без рук.
//!   7 «логистика» (M5, сцена mek_logistics): та же линия ×2, но сырьё — в сундуке A слева перед машинами: 5
//!     транспортёров (первый тянет из A) несут его к лицу очистителя; выход плавильни — 2 транспортёра → сундук B.
//!     Воды в сепараторе нет: насос в каменной чаше с водой западнее качает её вверх, механическая труба над кубом
//!     энергии насоса ведёт к верху сепаратора (logistics_layout).
//!   8 «стенд» (M5, сцена mek_logistics_bench): 8 параллельных линий по 30 транспортёров от сундуков с булыжником к
//!     сундукам — сразу 480 едущих предметов (по 2 на транспортёр) и забор дальше (цена кадра, perf/log.md); 9 — те же
//!     линии без предметов (сравнение A/B, сцена mek_logistics_bench0).

use crate::cell::{cell_of, Cell, F_NORTH, F_SOUTH, F_WEST};
use crate::game::give_into;
use crate::game::inventory::Inventory;
use crate::game::sim::Sim;
use crate::game::view::Events;
use crate::registry::ids::*;
use crate::registry::items::*;
use crate::world::mek::{CH_IN, INF_EXTRA, INF_IN, UP_SPEED};
use crate::world::stack::ItemStack;
use crate::world::WorldData;

/// Длина линии кабеля от генератора до инфузора.
pub(crate) const LINE: i32 = 10;

/// Клетки площадки от ступней p: (генератор, первый кабель, инфузор).
pub(crate) fn layout(p: [i32; 3]) -> ([i32; 3], [i32; 3], [i32; 3]) {
    let g = [p[0], p[1], p[2] - 3];
    let c0 = [g[0], g[1], g[2] + 1];
    let inf = [c0[0] + LINE, c0[1], c0[2]];
    (g, c0, inf)
}

/// Площадка «обогащение» от ступней p: (камера, кабель, плавильня, куб).
pub(crate) fn enrich_layout(p: [i32; 3]) -> ([i32; 3], [i32; 3], [i32; 3], [i32; 3]) {
    let c = [p[0], p[1], p[2] - 3];
    ([c[0] - 1, c[1], c[2]], c, [c[0] + 1, c[1], c[2]], [c[0], c[1], c[2] - 1])
}

/// Площадка «ветер» от ступней p: (ветряк у ног, ветряк у моря).
pub(crate) fn wind_layout(p: [i32; 3]) -> ([i32; 3], [i32; 3]) {
    ([p[0], p[1], p[2] - 3], [p[0] - 4, 49, p[2] - 3])
}
/// Линия «склад» площадки «ветер»: (камера, плавильня, сундук, куб энергии).
pub(crate) fn store_layout(p: [i32; 3]) -> ([i32; 3], [i32; 3], [i32; 3], [i32; 3]) {
    let z = p[2] - 2;
    ([p[0] + 2, p[1], z], [p[0] + 3, p[1], z], [p[0] + 4, p[1], z], [p[0] + 1, p[1] - 1, z + 1])
}

/// Ветряк в клетке b лицом на юг: основание и 4 части, столб над верхом — воздух до неба, под основанием — камень.
fn wind_at(sim: &mut Sim, w: &mut WorldData, ev: &mut Events, b: [i32; 3]) {
    for y in b[1]..crate::cell::H {
        sim.set_world(w, [b[0], y, b[2]], AIR as Cell, ev);
    }
    sim.set_world(w, [b[0], b[1] - 1, b[2]], STONE as Cell, ev);
    sim.set_world(w, b, cell_of(WIND_GENERATOR, F_SOUTH as u16), ev);
    for dy in 1..=4 {
        sim.set_world(w, [b[0], b[1] + dy, b[2]], cell_of(BOUNDING, dy as u16), ev);
    }
}

/// Площадка «удвоение» (M4) от ступней p — ряд на z − 3 лицом к игроку (на юг), с запада на восток: сепаратор,
/// очиститель (справа от сепаратора — кислород идёт в него сам), дробилка, обогатительная камера, плавильня, сундук;
/// правая грань каждой машины — выход в левую («вход») следующей. Ответ: (сепаратор, очиститель, дробилка, камера,
/// плавильня, сундук, куб энергии). Кабель — в полу (y − 1) под машинами, куб — в начале кабеля лицом на восток.
pub(crate) type DoubleLayout = ([i32; 3], [i32; 3], [i32; 3], [i32; 3], [i32; 3], [i32; 3], [i32; 3]);
pub(crate) fn double_layout(p: [i32; 3]) -> DoubleLayout {
    let (x, y, z) = (p[0] - 2, p[1], p[2] - 3);
    ([x, y, z], [x + 1, y, z], [x + 2, y, z], [x + 3, y, z], [x + 4, y, z], [x + 5, y, z], [x - 1, y - 1, z])
}

/// Площадка «удвоение» (сцена mek_double): 10 сырого железа во входе очистителя, в сепараторе 12 000 мБ воды и водород
/// в «сбросе излишка» (копится до 2 040 — видно в окне), выталкивание включено, полный куб энергии. Набор — вёдра воды, сырьё, кремень, улучшения.
fn build_double(sim: &mut Sim, w: &mut WorldData, ev: &mut Events, inv: &mut Inventory, p: [i32; 3]) {
    use crate::cell::F_EAST;
    use crate::world::mek::{recipes::SUB_WATER, Tank, GM_EXCESS, PUR_IN};
    let (sep, pur, cru, ch, sm, chest, cube) = double_layout(p);
    for x in sep[0] - 3..=chest[0] + 3 {
        for z in sep[2] - 3..=p[2] + 3 {
            sim.set_world(w, [x, p[1] - 2, z], STONE as Cell, ev);
            sim.set_world(w, [x, p[1] - 1, z], STONE as Cell, ev);
            for y in p[1]..p[1] + 5 {
                sim.set_world(w, [x, y, z], AIR as Cell, ev);
            }
        }
    }
    for x in sep[0]..=sm[0] {
        sim.set_world(w, [x, p[1] - 1, sep[2]], UNIVERSAL_CABLE as Cell, ev);
    }
    sim.set_world(w, cube, cell_of(ENERGY_CUBE, F_EAST as u16), ev);
    for (q, b) in [(sep, ELECTROLYTIC_SEPARATOR), (pur, PURIFICATION_CHAMBER), (cru, CRUSHER), (ch, ENRICHMENT_CHAMBER), (sm, ENERGIZED_SMELTER)] {
        sim.set_world(w, q, cell_of(b, F_SOUTH as u16), ev);
        if let Some(m) = w.be_mut().mek_mut(q) {
            m.eject = b != ELECTROLYTIC_SEPARATOR;
        }
    }
    sim.set_world(w, chest, CHEST as Cell, ev);
    if let Some(m) = w.be_mut().mek_mut(cube) {
        m.energy = m.cap();
    }
    if let Some(m) = w.be_mut().mek_mut(sep) {
        m.tank = Tank { sub: SUB_WATER, amount: 12_000 };
        m.gas_mode[0] = GM_EXCESS;
    }
    if let Some(m) = w.be_mut().mek_mut(pur) {
        m.slots[PUR_IN] = ItemStack::of(I_RAW_IRON, 10);
    }
    for &(item, n) in &[
        (I_WATER_BUCKET, 1),
        (I_WATER_BUCKET, 1),
        (I_WATER_BUCKET, 1),
        (I_RAW_IRON, 10),
        (RAW_IRON_BLOCK as ItemId, 1),
        (I_FLINT, 8),
        (I_UPGRADE_SPEED, 8),
        (I_UPGRADE_GAS, 4),
        (I_CONFIGURATOR, 1),
    ] {
        give_into(inv, item, n);
    }
}

/// Площадка «логистика» (M5) от ступней p: (сундук A, первый и последний транспортёры входа, сундук B, насос, куб
/// насоса, верх трубы над сепаратором). Машины — double_layout (сундук M4 заменён транспортёром).
pub(crate) type LogisticsLayout = ([i32; 3], [i32; 3], [i32; 3], [i32; 3], [i32; 3], [i32; 3], [i32; 3]);
pub(crate) fn logistics_layout(p: [i32; 3]) -> LogisticsLayout {
    let (sep, pur, _, _, sm, _, _) = double_layout(p);
    let z = pur[2] + 1;
    let a = [pur[0] - 5, p[1], z];
    (a, [a[0] + 1, p[1], z], [pur[0], p[1], z], [sm[0] + 3, p[1], sm[2]], [sep[0] - 2, p[1], sep[2]], [sep[0] - 1, p[1], sep[2]], [sep[0], p[1] + 1, sep[2]])
}

/// Режим стороны передатчика (как конфигуратор у сервера): рукава у него и у соседа заново.
pub(crate) fn set_link(w: &mut WorldData, p: [i32; 3], f: u8, m: u8) {
    w.grid.set_mode(p, f, m);
    crate::game::sim::fix_arms(w, p);
    let q = crate::cell::offset(p, f);
    if crate::world::grid::is_transmitter(w.get_cell(q[0], q[1], q[2])) {
        crate::game::sim::fix_arms(w, q);
    }
    w.grid.relink(p);
}

/// Площадка «логистика» (сцена mek_logistics): линия ×2 M4 с входом из сундука через транспортёры и выходом в сундук
/// транспортёрами, вода — насосом из чаши трубой (без вёдер).
fn build_logistics(sim: &mut Sim, w: &mut WorldData, ev: &mut Events, inv: &mut Inventory, p: [i32; 3]) {
    use crate::cell::F_WEST;
    use crate::world::grid::M_PULL;
    build_double(sim, w, ev, &mut Inventory::new(), p);
    let (sep, pur, _, _, _, chest, _) = double_layout(p);
    let (a, t0, t1, b, pump, pcube, top) = logistics_layout(p);
    // Площадку шире на запад (насос) и восток (выход).
    for x in a[0] - 2..=b[0] + 2 {
        for z in sep[2] - 3..=p[2] + 3 {
            if x >= sep[0] - 3 && x <= chest[0] + 3 {
                continue;
            }
            sim.set_world(w, [x, p[1] - 2, z], STONE as Cell, ev);
            sim.set_world(w, [x, p[1] - 1, z], STONE as Cell, ev);
            for y in p[1]..p[1] + 5 {
                sim.set_world(w, [x, y, z], AIR as Cell, ev);
            }
        }
    }
    // Сырьё — в сундуке A, не в очистителе.
    if let Some(m) = w.be_mut().mek_mut(pur) {
        m.slots[crate::world::mek::PUR_IN] = ItemStack::EMPTY;
    }
    sim.set_world(w, a, CHEST as Cell, ev);
    w.be_mut().container(a).unwrap().set(0, ItemStack::of(I_RAW_IRON, 10));
    for x in t0[0]..=t1[0] {
        sim.set_world(w, [x, p[1], t0[2]], LOGISTICAL_TRANSPORTER as Cell, ev);
    }
    set_link(w, t0, F_WEST, M_PULL);
    // Выход: сундук M4 → транспортёр, ещё один, сундук B.
    sim.set_world(w, chest, LOGISTICAL_TRANSPORTER as Cell, ev);
    sim.set_world(w, [chest[0] + 1, p[1], chest[2]], LOGISTICAL_TRANSPORTER as Cell, ev);
    sim.set_world(w, b, CHEST as Cell, ev);
    // Вода: сепаратор пуст; чаша воды 2 × 3 западнее (камень вокруг и снизу), насос над ней лицом на запад — зад (вход
    // энергии) к своему полному кубу; труба от верха насоса над кубом к верху сепаратора.
    if let Some(m) = w.be_mut().mek_mut(sep) {
        m.tank = crate::world::mek::Tank::default();
    }
    for x in pump[0] - 1..=pump[0] {
        for z in pump[2] - 1..=pump[2] + 1 {
            sim.set_world(w, [x, p[1] - 2, z], STONE as Cell, ev);
            sim.set_world(w, [x, p[1] - 1, z], WATER as Cell, ev);
        }
    }
    sim.set_world(w, pcube, cell_of(ENERGY_CUBE, F_WEST as u16), ev);
    if let Some(m) = w.be_mut().mek_mut(pcube) {
        m.energy = m.cap();
    }
    sim.set_world(w, pump, cell_of(ELECTRIC_PUMP, F_WEST as u16), ev);
    for x in pump[0]..=top[0] {
        sim.set_world(w, [x, top[1], top[2]], MECHANICAL_PIPE as Cell, ev);
    }
    for &(item, n) in &[
        (LOGISTICAL_TRANSPORTER as ItemId, 16),
        (MECHANICAL_PIPE as ItemId, 8),
        (PRESSURIZED_TUBE as ItemId, 8),
        (ELECTRIC_PUMP as ItemId, 1),
        (CHEST as ItemId, 2),
        (I_RAW_IRON, 10),
        (I_CONFIGURATOR, 1),
    ] {
        give_into(inv, item, n);
    }
}

/// Линий и длина линии стенда (площадка 8).
pub(crate) const BENCH_LINES: i32 = 8;
pub(crate) const BENCH_LEN: i32 = 30;
/// Площадка «стенд» (сцена mek_logistics_bench): 8 линий по 30 транспортёров на юг от сундуков с булыжником (27 × 64)
/// к сундукам — в каждой ~55 едущих предметов (забор раз в 11 тиков, 20 тиков на блок).
fn build_bench(sim: &mut Sim, w: &mut WorldData, ev: &mut Events, p: [i32; 3], items: bool) {
    use crate::cell::F_NORTH;
    use crate::world::grid::M_PULL;
    let (x0, z0) = (p[0] - BENCH_LINES, p[2] - BENCH_LEN - 3);
    for x in x0 - 2..=x0 + 2 * BENCH_LINES + 1 {
        for z in z0 - 2..=p[2] + 2 {
            sim.set_world(w, [x, p[1] - 1, z], STONE as Cell, ev);
            for y in p[1]..p[1] + 3 {
                sim.set_world(w, [x, y, z], AIR as Cell, ev);
            }
        }
    }
    for k in 0..BENCH_LINES {
        let x = x0 + 2 * k;
        let src = [x, p[1], z0];
        sim.set_world(w, src, CHEST as Cell, ev);
        for i in 0..27 {
            w.be_mut().container(src).unwrap().set(i, ItemStack::of(COBBLE as ItemId, 64));
        }
        for d in 1..=BENCH_LEN {
            sim.set_world(w, [x, p[1], z0 + d], LOGISTICAL_TRANSPORTER as Cell, ev);
        }
        sim.set_world(w, [x, p[1], z0 + BENCH_LEN + 1], CHEST as Cell, ev);
        if !items {
            continue;
        }
        set_link(w, [x, p[1], z0 + 1], F_NORTH, M_PULL);
        // Сразу в пути — по 2 стека на транспортёр (центр и вход): установившийся поток без 30 с ожидания.
        let list = (1..=BENCH_LEN)
            .flat_map(|d| {
                let at = [x, p[1], z0 + d];
                let from = [x, p[1], z0 + d - 1];
                [0u16, 50].map(|pr| (at, from, pr, ItemStack::of(COBBLE as ItemId, 1), 0u8, crate::world::logistics::Goal::Dest, Some(src), None))
            })
            .collect();
        w.logistics.restore(list);
    }
}

/// Построить площадку kind у ступней p; набор — в инвентарь.
pub(crate) fn build(sim: &mut Sim, w: &mut WorldData, ev: &mut Events, inv: &mut Inventory, p: [i32; 3], kind: u8) {
    if kind == 6 {
        build_double(sim, w, ev, inv, p);
        return;
    }
    if kind == 7 {
        build_logistics(sim, w, ev, inv, p);
        return;
    }
    if kind == 8 || kind == 9 {
        build_bench(sim, w, ev, p, kind == 8);
        return;
    }
    if kind == 5 {
        for x in p[0] - 5..=p[0] + 5 {
            for z in p[2] - 7..=p[2] + 3 {
                sim.set_world(w, [x, p[1] - 2, z], STONE as Cell, ev);
                sim.set_world(w, [x, p[1] - 1, z], STONE as Cell, ev);
                for y in p[1]..p[1] + 6 {
                    sim.set_world(w, [x, y, z], AIR as Cell, ev);
                }
            }
        }
        let (hill, sea) = wind_layout(p);
        wind_at(sim, w, ev, hill);
        if sea[1] + 5 < p[1] {
            wind_at(sim, w, ev, sea);
        }
        let (ch, sm, chest, cube) = store_layout(p);
        let y = p[1] - 1;
        for q in [[hill[0], y, hill[2]], [hill[0], y, ch[2]], [hill[0] + 1, y, ch[2]], [ch[0], y, ch[2]], [sm[0], y, sm[2]]] {
            sim.set_world(w, q, UNIVERSAL_CABLE as Cell, ev);
        }
        sim.set_world(w, cube, cell_of(ENERGY_CUBE, F_NORTH as u16), ev);
        sim.set_world(w, ch, cell_of(ENRICHMENT_CHAMBER, F_SOUTH as u16), ev);
        sim.set_world(w, sm, cell_of(ENERGIZED_SMELTER, F_SOUTH as u16), ev);
        sim.set_world(w, chest, CHEST as Cell, ev);
        if let Some(m) = w.be_mut().mek_mut(cube) {
            m.energy = m.cap();
        }
        if let Some(m) = w.be_mut().mek_mut(ch) {
            m.slots[CH_IN] = ItemStack::of(I_RAW_IRON, 9);
            m.install(UP_SPEED, 4);
            m.eject = true;
        }
        if let Some(m) = w.be_mut().mek_mut(sm) {
            m.eject = true;
        }
        for &(item, n) in &[
            (WIND_GENERATOR as ItemId, 1),
            (I_ENERGY_TABLET, 1),
            (UNIVERSAL_CABLE as ItemId, 8),
            (I_CONFIGURATOR, 1),
            (I_UPGRADE_SPEED, 4),
            (I_UPGRADE_ENERGY, 4),
            (I_RAW_IRON, 9),
        ] {
            give_into(inv, item, n);
        }
        return;
    }
    let (g, c0, inf) = layout(p);
    let stone = STONE as Cell;
    // Пол и воздух: 15 × 15 вокруг, от генератора до конца линии.
    for x in g[0] - 3..=inf[0] + 3 {
        for z in g[2] - 3..=p[2] + 3 {
            sim.set_world(w, [x, g[1] - 2, z], stone, ev);
            sim.set_world(w, [x, g[1] - 1, z], stone, ev);
            for y in g[1]..g[1] + 4 {
                sim.set_world(w, [x, y, z], AIR as Cell, ev);
            }
        }
    }
    if kind == 4 {
        let (ch, cable, sm, cube) = enrich_layout(p);
        sim.set_world(w, cube, cell_of(ENERGY_CUBE, F_SOUTH as u16), ev);
        sim.set_world(w, cable, UNIVERSAL_CABLE as Cell, ev);
        sim.set_world(w, ch, cell_of(ENRICHMENT_CHAMBER, F_SOUTH as u16), ev);
        sim.set_world(w, sm, cell_of(ENERGIZED_SMELTER, F_SOUTH as u16), ev);
        if let Some(m) = w.be_mut().mek_mut(cube) {
            m.energy = m.cap();
        }
        if let Some(m) = w.be_mut().mek_mut(ch) {
            m.slots[CH_IN] = ItemStack::of(I_RAW_IRON, 9);
        }
        for &(item, n) in &[
            (I_RAW_IRON, 9),
            (RAW_IRON_BLOCK as ItemId, 1),
            (I_RAW_OSMIUM, 6),
            (I_COAL, 8),
            (GRAVEL as ItemId, 4),
            (ENRICHMENT_CHAMBER as ItemId, 1),
            (ENERGIZED_SMELTER as ItemId, 1),
            (I_REDSTONE, 8),
        ] {
            give_into(inv, item, n);
        }
        return;
    }
    // Лава вокруг места генератора (запад, восток, север, снизу) в каменной чаше.
    for q in [[g[0] - 1, g[1], g[2]], [g[0] + 1, g[1], g[2]], [g[0], g[1], g[2] - 1], [g[0], g[1] - 1, g[2]]] {
        sim.set_world(w, q, LAVA as Cell, ev);
    }
    // Чаша закрыта и с юга у запада и востока (лава течёт, ASP-15): кабель линии встанет на место восточного камня.
    for q in [[g[0] - 2, g[1], g[2]], [g[0] + 2, g[1], g[2]], [g[0], g[1], g[2] - 2], [g[0] - 1, g[1], g[2] - 1], [g[0] + 1, g[1], g[2] - 1], [g[0] - 1, g[1], g[2] + 1], [g[0] + 1, g[1], g[2] + 1]] {
        sim.set_world(w, q, stone, ev);
    }
    if kind == 3 {
        sim.set_world(w, g, cell_of(HEAT_GENERATOR, F_SOUTH as u16), ev);
    }
    if kind == 2 {
        sim.set_world(w, g, cell_of(HEAT_GENERATOR, F_SOUTH as u16), ev);
        for k in 0..LINE {
            sim.set_world(w, [c0[0] + k, c0[1], c0[2]], UNIVERSAL_CABLE as Cell, ev);
        }
        sim.set_world(w, inf, cell_of(METALLURGIC_INFUSER, F_WEST as u16), ev);
        if let Some(m) = w.be_mut().mek_mut(inf) {
            m.slots[INF_EXTRA] = ItemStack::of(I_COAL, 16);
            m.slots[INF_IN] = ItemStack::of(I_IRON_INGOT, 6);
        }
    }
    let kit: &[(ItemId, i32)] = if kind >= 2 {
        &[(I_COAL, 16), (I_IRON_INGOT, 8), (FURNACE as ItemId, 1)]
    } else {
        &[
            (HEAT_GENERATOR as ItemId, 1),
            (METALLURGIC_INFUSER as ItemId, 1),
            (UNIVERSAL_CABLE as ItemId, 16),
            (I_COAL, 16),
            (I_IRON_INGOT, 8),
            (FURNACE as ItemId, 1),
            (I_REDSTONE, 8),
            (ENERGY_CUBE as ItemId, 1),
        ]
    };
    for &(item, n) in kit {
        give_into(inv, item, n);
    }
}
