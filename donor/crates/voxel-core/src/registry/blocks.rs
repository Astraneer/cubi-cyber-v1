//! Таблица блоков (ASP-06 §2, Ф1 — одна таблица поведения вместо match по файлам): номер блока → BlockDef — твёрдость,
//! инструмент, звук, дроп, формы (обводка, коллизия, опора граней), как рисуется (куб, крест, листва, стекло, вода,
//! модель), слои атласа граней и оттенок, правила опоры и ставления, окно по ПКМ, блок-сущность, падение, случайный
//! тик, показ (огоньки), группы (бревно, листва, трава, #dirt, лазанье), светимость на будущее (3B).
//! Новый блок — строка в block_def; код игры спрашивает таблицу, а не номер блока. Правила с параметрами (форма по
//! стороне, опора у стены, ставление факела) — перечисления здесь, их исполнение — в мире и игре (interact.rs).
//!
//! Состояние (cell.rs: старшие 16 бит ячейки, занято младшие 8) — раскладка по блокам:
//!   бревно — ось: 0 Y, 1 X, 2 Z (ось грани клика, PillarBlock);
//!   печь, сундук, лестница, настенный факел — биты 0–2: FACING (F_NORTH…F_EAST, cell.rs); печь — бит 3: LIT;
//!   листва — бит 0: «опадает» (до бревна по листве больше 6 шагов, DISTANCE 7), бит 1: поставлена игроком (PERSISTENT);
//!   снежный слой — биты 0–2: слоёв − 1;
//!   бит 7 — зарезервирован (waterlogged, ASP-15); уровень жидкости — биты 0–3 воды (ASP-15).

use crate::abi::Model;
use crate::cell::{block_of, cell_of, state_of, BlockId, Cell, State, DIRS};
use crate::registry::ids::*;
use crate::registry::items::{
    block_item, ItemId, I_APPLE, I_CLAY_BALL, I_COAL, I_FLINT, I_RAW_COPPER, I_RAW_GOLD, I_RAW_IRON, I_RAW_OSMIUM, I_RAW_TIN,
    I_REDSTONE, I_SNOWBALL, I_STICK, I_WHEAT_SEEDS,
};
use crate::registry::shapes::*;

pub(crate) const ST_LIT: State = 8;
pub(crate) const ST_DECAY: State = 1;
pub(crate) const ST_PERSIST: State = 2;

/// Ёмкость таблицы блоков: номера пока меньше 256 (предмет блока — тот же номер 1…255, registry/items.rs).
pub(crate) const BLOCK_COUNT: usize = 256;
/// Род жидкости (BlockDef.fluid).
pub(crate) const FLUID_WATER: u8 = 1;
pub(crate) const FLUID_LAVA: u8 = 2;

/// Сторона FACING из состояния (горизонтальная; без неё — север, состояние по умолчанию Minecraft).
#[inline]
pub(crate) fn facing_of(c: Cell) -> u8 {
    let f = (state_of(c) & 7) as u8;
    if (2..=5).contains(&f) {
        f
    } else {
        2
    }
}
/// Индекс горизонтальной стороны N, S, W, E → 0…3 (таблицы форм).
#[inline]
pub(crate) fn hidx(f: u8) -> usize {
    (f.clamp(2, 5) - 2) as usize
}
/// Слоёв снега в ячейке снежного слоя (1…8).
#[inline]
pub(crate) fn snow_layers(c: Cell) -> u8 {
    (state_of(c) & 7) as u8 + 1
}
/// Грань меша (0 +X, 1 −X, 2 верх, 3 низ, 4 +Z, 5 −Z) для стороны F_* (cell.rs).
pub(crate) fn mesh_face(f: u8) -> usize {
    [3, 2, 5, 4, 1, 0][f.min(5) as usize]
}

/// Звуковая группа (ASP-06 §2.1; сами звуки — ASP-13).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Snd {
    None,
    Stone,
    Wood,
    Gravel,
    Grass,
    Sand,
    Snow,
    Wool,
    Glass,
    Metal,
    Ladder,
}

/// Смещение модели по хешу клетки (AbstractBlock.OffsetType): XZ — цветы, XYZ — трава и папоротник.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Offset {
    None,
    Xz,
    Xyz,
}

// Маска тегов mineable/*.
pub(crate) const PICKAXE: u8 = 1;
pub(crate) const AXE: u8 = 2;
pub(crate) const SHOVEL: u8 = 4;
pub(crate) const HOE: u8 = 8;

/// Строка таблицы добычи. alt = 0 — независимый пул; одинаковый alt > 0 — альтернативы: берётся первая сработавшая
/// строка группы (гравий: кремень 10 %, иначе гравий). Число — равновероятно от min до max включительно.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Drop {
    pub(crate) item: ItemId,
    pub(crate) min: u8,
    pub(crate) max: u8,
    pub(crate) chance: f32,
    pub(crate) alt: u8,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Loot {
    None,
    /// 1 предмет самого блока (по base).
    Itself,
    Table(&'static [Drop]),
    /// Снежный слой: столько снежков, сколько слоёв, — только если сломал игрок (иначе ничего).
    SnowLayers,
    /// Посев по возрасту (loot_table/blocks/wheat, carrots, potatoes): зрелый — урожай и семена с бонусом (game/farm.rs).
    Crop,
}

/// Как блок рисуется в меше куска. Грани соседей закрывают только Cube и Leaves (opaque).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Render {
    /// Не рисуется (воздух).
    None,
    /// Куб из 6 граней (склейка, затенение в углах).
    Cube,
    /// Крест из двух плоскостей (cutout): растения, стоячий факел.
    Cross,
    /// Листва: куб, kind 2 в вершине; «красивая» — в cutout, грани листва–листва.
    Leaves,
    /// Стекло: куб в cutout без склейки; стекло к стеклу не рисуется, соседи рисуют свою грань к нему.
    Glass,
    /// Вода: грани только к воздуху, растению, стеклу и модели.
    Water,
    /// Своя модель меньше куба (сундук, лестница, настенный факел, снежный слой): меш куска её не рисует — проход
    /// моделей (web/render/chest.js по Game::model_draws); у вершины куска нет дробных координат (Ф3).
    Model(Model),
}

impl Render {
    /// Закрывает ли блок грань соседа (листва закрывает листву — как «быстрая» графика Minecraft).
    pub(crate) const fn opaque(self) -> bool {
        matches!(self, Render::Cube | Render::Leaves)
    }
}

/// Слой грани по состоянию (меш куска).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TexRule {
    /// Слои tex как есть.
    Fixed,
    /// Бревно: торцы (tex[2]) на гранях своей оси, кора (tex[0]) на остальных, у лежачего — с поворотом.
    Axis,
    /// Лицо по FACING (tex[5] — лицо, при LIT — lit), верх и низ — tex[2], tex[3], прочие бока — tex[0].
    Facing { lit: u32 },
    /// Посев: слой стадии от base по возрасту (пшеница — 8 стадий подряд, морковь и картофель — 4: 0–1, 2–3, 4–6, 7).
    Crop { base: u32, wheat: bool },
    /// Пашня: верх мокрый при влажности 7, иначе сухой; бока и низ — земля.
    Farmland,
}

/// Форма по состоянию: обводка (луч, рамка) и коллизия.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Shape {
    /// outline и collision из таблицы.
    Fixed,
    /// Лестница: пластина у стены по FACING — и обводка, и коллизия.
    Ladder,
    /// Настенный факел: обводка у стены по FACING, коллизии нет.
    WallTorch,
    /// Снежный слой: вид — по слоям, коллизия — на слой ниже.
    Layers,
    /// Кабель: центр 6×6×6 и рукава по 6 битам соседей (world/grid.rs), и обводка, и коллизия.
    Cable,
    /// Большой передатчик (транспортёр, механическая труба): центр 8×8×8 (4…12) и рукава, как у кабеля.
    Large,
    /// Пшеница: обводка 16 × h × 16 px, h = 2, 4…16 по возрасту (биты 0–2); коллизии нет.
    Wheat,
    /// Морковь и картофель: h = 2, 3…9 px по возрасту.
    Root,
}

/// Форма граней для опоры (getSidesShape).
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Sides {
    /// Коллизия по состоянию.
    Collision,
    /// Вид по состоянию (снежный слой).
    Outline,
    /// Пусто (листва: на неё не повесить факел и лестницу).
    Empty,
    /// Своя коробка (кактус: настоящая коллизия 1…15, у нас пока куб).
    Box(&'static [Aabb]),
}

/// Опора блока: без неё — ломается от обновления соседа (World.breakBlock) и не ставится.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Support {
    /// Держится всегда.
    None,
    /// #dirt или пашня (PlantBlock: трава, цветы, саженцы).
    Plant,
    /// #dirt или песок, без пашни (мёртвый куст: #dead_bush_may_place_on).
    DeadBush,
    /// Верх блока снизу закрывает центр 2×2 (стоячий факел: sideCoversSmallSquare; листва — нет).
    Floor,
    /// Полная грань блока сзади по FACING (настенный факел, лестница).
    Wall,
    /// Верх блока снизу — полный квадрат или снег в 8 слоёв (снежный слой).
    Layers,
    /// Кактус: без твёрдых соседей сбоку, на кактусе или песке, без воды сверху; без опоры — тик через 1 и поломка.
    Cactus,
    /// Посев (ASP-19): на пашне и при свете не ниже 8; без опоры — ломается с дропом при любом обновлении соседа.
    Crop,
}

/// Состояние при ставлении (getPlacementState).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Place {
    /// Состояние 0.
    Plain,
    /// Ось по грани клика (бревно).
    Axis,
    /// Лицом к игроку по yaw (печь, сундук).
    FacePlayer,
    /// Вниз — стоячий на опоре, вбок — настенный вариант wall на полной грани (факел).
    Torch { wall: BlockId },
    /// Первое горизонтальное направление с полной гранью сзади (лестница).
    Ladder,
    /// На таком же блоке — слой +1 (снежный слой).
    Layers,
    /// «Поставлена игроком» — не опадает (листва).
    Persistent,
}

/// ПКМ по блоку (onUse): окно.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum OnUse {
    None,
    Crafting,
    Furnace,
    /// Окно, если сверху не сплошной блок (иначе только взмах).
    Chest,
    /// Окно машины Mekanism (вид — блок-сущность, game/mek_ui.rs).
    Machine,
}

/// Машина Mekanism (world/mek): вид задаёт строку таблицы mek_def — энергия, слоты, окно.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum MekKind {
    /// Тепловой генератор (MEK-03 §2).
    #[default]
    Heat,
    /// Металлургический инфузор (MEK-04 §2).
    Infuser,
    /// Базовый куб энергии (MEK-12 §5).
    Cube,
    /// Обогатительная камера (MEK-06 §3.1).
    Enrich,
    /// Плавильня с энергией (MEK-06 §3.3).
    Smelter,
    /// Ветряной генератор (MEK-03 §3): основание многоблока 1 × 5.
    Wind,
    /// Дробилка (MEK-06 §3.2): камера с таблицей CRUSH.
    Crusher,
    /// Очистительная камера (MEK-11 §4.1): сырьё + кислород каждый тик → 2 комка.
    Purifier,
    /// Электролитический сепаратор (MEK-11 §4.2): вода → водород (лево) + кислород (право), баки газов.
    Separator,
    /// Электрический насос (MEK-14 §11.4): источник под собой → бак 10 000 мБ → вверх 256 мБ/т.
    Pump,
}

/// Ограничивающий блок многоблока (BOUNDING): высота над основанием 1…4 в состоянии.
#[inline]
pub(crate) fn bounding_dy(c: Cell) -> i32 {
    (state_of(c) & 7) as i32
}
/// Основание клетки p: у ограничивающего блока — клетка ниже на его высоту, у прочих — сама p.
pub(crate) fn main_of(c: Cell, p: [i32; 3]) -> [i32; 3] {
    if block_of(c) == BOUNDING {
        [p[0], p[1] - bounding_dy(c).max(1), p[2]]
    } else {
        p
    }
}
/// Части многоблока над основанием блока b: высоты (ветряк — 1…4); пусто — обычный блок.
pub(crate) fn multiblock_parts(b: BlockId) -> std::ops::RangeInclusive<i32> {
    if b == WIND_GENERATOR {
        1..=4
    } else {
        1..=0
    }
}

/// Вид блок-сущности (world/blockentity.rs).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum BeKind {
    Furnace,
    Chest,
    Mek(MekKind),
}

/// Случайный тик (randomTick).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RandomTick {
    None,
    /// Опадающая листва: дроп без инструмента и удаление.
    LeafDecay,
    /// Пашня: увлажнение водой в 4 клетках, высыхание, превращение в землю (game/farm.rs).
    Farmland,
    /// Посев: рост по влажности соседней пашни (game/farm.rs).
    Crop,
}

/// Показ на клиенте (randomDisplayTick).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Display {
    None,
    /// Горящая печь: треск, дым и огонёк у низа лица.
    Furnace,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct BlockDef {
    pub(crate) mc: &'static str,
    /// Чьё поведение и предмет (варианты травы → GRASS).
    pub(crate) base: BlockId,
    /// −1 — неломаемый, 0 — мгновенный.
    pub(crate) hardness: f32,
    pub(crate) requires_tool: bool,
    /// Маска mineable/* (PICKAXE | AXE | …).
    pub(crate) tool: u8,
    /// Ярус для дропа: 0 — любой, 1 — каменный (needs_stone_tool), 2 — железный.
    pub(crate) needs: u8,
    /// Тег sword_efficient (меч 1.5).
    pub(crate) sword_fast: bool,
    pub(crate) sound: Snd,
    /// Можно поставить блок прямо в эту клетку (воздух, вода, трава, папоротник, мёртвый куст).
    pub(crate) replaceable: bool,
    /// Обводка (луч и рамка) и коллизия, в долях блока; пусто — нет формы. По состоянию — shape.
    pub(crate) outline: &'static [Aabb],
    pub(crate) collision: &'static [Aabb],
    /// Смещение модели; обводка смещается только у цветов (FlowerBlock), у травы — нет (ShortPlantBlock).
    pub(crate) offset: Offset,
    pub(crate) outline_offset: bool,
    /// Удушающий блок: выталкивание игрока (pushOutOfBlocks) — полные непрозрачные, не листва и не кактус.
    pub(crate) suffocates: bool,
    pub(crate) slip: f32,
    pub(crate) loot: Loot,
    /// Как рисуется в меше куска (и закрывает ли грани соседей — Render::opaque).
    pub(crate) render: Render,
    /// Слои атласа граней при состоянии 0 в порядке меша: 0 +X, 1 −X, 2 верх, 3 низ, 4 +Z, 5 −Z (крест — tex[0]);
    /// они же — грани иконки куба (item_icon) и текстуры моделей (web/render/chest.js).
    pub(crate) tex: [u32; 6],
    /// Слой грани по состоянию.
    pub(crate) tex_rule: TexRule,
    /// Грани, которые красятся оттенком столбца (Region::tint): бит грани 0…5, бит 6 — крест.
    pub(crate) tint: u8,
    pub(crate) shape: Shape,
    pub(crate) sides: Sides,
    pub(crate) support: Support,
    pub(crate) place: Place,
    pub(crate) on_use: OnUse,
    pub(crate) be: Option<BeKind>,
    /// Падающий (FallingBlock: песок, гравий): тик через 2 после появления и обновления соседа.
    pub(crate) falling: bool,
    pub(crate) random: RandomTick,
    pub(crate) display: Display,
    /// Группы: бревно (опад листвы), листва, трава (растительность генератора), тег #dirt, пашня, #climbable.
    pub(crate) log: bool,
    pub(crate) leaves: bool,
    pub(crate) grass: bool,
    pub(crate) dirt: bool,
    pub(crate) farmland: bool,
    pub(crate) climbable: bool,
    /// «Сплошным блоком» не бывает никогда (Blocks.never: листва, стекло) — сундук под ним открывается.
    pub(crate) never_solid: bool,
    /// Крест вдали утапливается в землю (бит 13 w1 в меше: трава, папоротник, цветы); факел и куст — нет.
    pub(crate) sinks: bool,
    /// Предмет блока — плоский спрайт (item/generated: растения, лестница), иначе куб.
    pub(crate) flat_item: bool,
    /// Светимость 0…15 (на будущее, срез 3B); lit_only — светит только с LIT (печь).
    pub(crate) light: u8,
    pub(crate) lit_only: bool,
    /// Жидкость в клетке: 0 нет, FLUID_WATER, FLUID_LAVA (физика: касание, глаза, урон — game/player.rs, hazards.rs).
    pub(crate) fluid: u8,
}

/// Воздух и всё по умолчанию.
const EMPTY_BLOCK: BlockDef = BlockDef {
    mc: "air",
    base: AIR,
    hardness: 0.0,
    requires_tool: false,
    tool: 0,
    needs: 0,
    sword_fast: false,
    sound: Snd::None,
    replaceable: true,
    outline: &[],
    collision: &[],
    offset: Offset::None,
    outline_offset: false,
    suffocates: false,
    slip: 0.6,
    loot: Loot::None,
    render: Render::None,
    tex: [L_STONE; 6],
    tex_rule: TexRule::Fixed,
    tint: 0,
    shape: Shape::Fixed,
    sides: Sides::Collision,
    support: Support::None,
    place: Place::Plain,
    on_use: OnUse::None,
    be: None,
    falling: false,
    random: RandomTick::None,
    display: Display::None,
    log: false,
    leaves: false,
    grass: false,
    dirt: false,
    farmland: false,
    climbable: false,
    never_solid: false,
    sinks: false,
    flat_item: false,
    light: 0,
    lit_only: false,
    fluid: 0,
};

/// Свободный номер (резерв 34–39, ещё не заведённые): как воздух для физики, в меше — каменный куб (как раньше).
const UNKNOWN: BlockDef = {
    let mut d = EMPTY_BLOCK;
    d.render = Render::Cube;
    d
};

const fn solid(mc: &'static str, base: BlockId, hardness: f32, req: bool, tool: u8, sound: Snd, loot: Loot, tex: [u32; 6]) -> BlockDef {
    let mut d = EMPTY_BLOCK;
    d.mc = mc;
    d.base = base;
    d.hardness = hardness;
    d.requires_tool = req;
    d.tool = tool;
    d.sound = sound;
    d.replaceable = false;
    d.outline = CUBE;
    d.collision = CUBE;
    d.suffocates = true;
    d.loot = loot;
    d.render = Render::Cube;
    d.tex = tex;
    d
}

#[allow(clippy::too_many_arguments)]
const fn plant(mc: &'static str, base: BlockId, tool: u8, replaceable: bool, outline: &'static [Aabb], offset: Offset, loot: Loot, on: Support, l: u32) -> BlockDef {
    let mut d = EMPTY_BLOCK;
    d.mc = mc;
    d.base = base;
    d.tool = tool;
    d.sword_fast = true;
    d.sound = Snd::Grass;
    d.replaceable = replaceable;
    d.outline = outline;
    d.offset = offset;
    d.outline_offset = matches!(offset, Offset::Xz);
    d.loot = loot;
    d.support = on;
    d.render = Render::Cross;
    d.tex = [l; 6];
    d.flat_item = true;
    d
}

/// Все грани одним слоем.
const fn all(l: u32) -> [u32; 6] {
    [l; 6]
}
/// Верх, низ, бока.
const fn cap(top: u32, bottom: u32, side: u32) -> [u32; 6] {
    [side, side, top, bottom, side, side]
}
/// Верх и низ, бока, лицо на севере (−Z, FACING по умолчанию).
const fn faced(top: u32, side: u32, front: u32) -> [u32; 6] {
    [side, side, top, top, side, front]
}
/// Маска оттенка: все грани и крест; все, кроме низа.
const TINT_ALL: u8 = 0x7F;
const TINT_NOT_BOTTOM: u8 = 0x7F & !(1 << 3);

// Таблицы добычи без зачарований (ASP-06 §2.2). Саженцев пока нет (блоки с 40 — срез 2–4).
const DROP_DIRT: &[Drop] = &[Drop { item: DIRT as ItemId, min: 1, max: 1, chance: 1.0, alt: 0 }];
const DROP_COBBLE: &[Drop] = &[Drop { item: COBBLE as ItemId, min: 1, max: 1, chance: 1.0, alt: 0 }];
const DROP_GRAVEL: &[Drop] = &[
    Drop { item: I_FLINT, min: 1, max: 1, chance: 0.1, alt: 1 },
    Drop { item: GRAVEL as ItemId, min: 1, max: 1, chance: 1.0, alt: 1 },
];
const DROP_OAK_LEAVES: &[Drop] = &[
    Drop { item: I_STICK, min: 1, max: 2, chance: 0.02, alt: 0 },
    Drop { item: I_APPLE, min: 1, max: 1, chance: 0.005, alt: 0 },
];
const DROP_LEAVES: &[Drop] = &[Drop { item: I_STICK, min: 1, max: 2, chance: 0.02, alt: 0 }];
const DROP_SNOW_BLOCK: &[Drop] = &[Drop { item: I_SNOWBALL, min: 4, max: 4, chance: 1.0, alt: 0 }];
const DROP_CLAY: &[Drop] = &[Drop { item: I_CLAY_BALL, min: 4, max: 4, chance: 1.0, alt: 0 }];
const DROP_SHORT_GRASS: &[Drop] = &[Drop { item: I_WHEAT_SEEDS, min: 1, max: 1, chance: 0.125, alt: 0 }];
const DROP_DEAD_BUSH: &[Drop] = &[Drop { item: I_STICK, min: 0, max: 2, chance: 1.0, alt: 0 }];
const DROP_COAL: &[Drop] = &[Drop { item: I_COAL, min: 1, max: 1, chance: 1.0, alt: 0 }];
const DROP_RAW_IRON: &[Drop] = &[Drop { item: I_RAW_IRON, min: 1, max: 1, chance: 1.0, alt: 0 }];
// Руды M1 (MEK-05 §3.1): осмий, олово, золото — 1 сырьё; медь — 2…5; редстоун — 4…5 пыли.
const DROP_RAW_OSMIUM: &[Drop] = &[Drop { item: I_RAW_OSMIUM, min: 1, max: 1, chance: 1.0, alt: 0 }];
const DROP_RAW_TIN: &[Drop] = &[Drop { item: I_RAW_TIN, min: 1, max: 1, chance: 1.0, alt: 0 }];
const DROP_RAW_COPPER: &[Drop] = &[Drop { item: I_RAW_COPPER, min: 2, max: 5, chance: 1.0, alt: 0 }];
const DROP_RAW_GOLD: &[Drop] = &[Drop { item: I_RAW_GOLD, min: 1, max: 1, chance: 1.0, alt: 0 }];
const DROP_REDSTONE: &[Drop] = &[Drop { item: I_REDSTONE, min: 4, max: 5, chance: 1.0, alt: 0 }];

/// Листва: в «быстрой» графике закрывает грани, красится оттенком, на ней ничего не висит, опадает.
const fn leaves(mc: &'static str, b: BlockId, loot: Loot, l: u32) -> BlockDef {
    let mut d = solid(mc, b, 0.2, false, HOE, Snd::Grass, loot, all(l));
    d.sword_fast = true;
    d.suffocates = false;
    d.render = Render::Leaves;
    d.tint = TINT_ALL;
    d.sides = Sides::Empty;
    d.never_solid = true;
    d.leaves = true;
    d.place = Place::Persistent;
    d.random = RandomTick::LeafDecay;
    d
}
/// Бревно: торцы по оси, ось — по грани клика.
const fn log(mc: &'static str, b: BlockId, top: u32, side: u32) -> BlockDef {
    let mut d = solid(mc, b, 2.0, false, AXE, Snd::Wood, Loot::Itself, cap(top, top, side));
    d.tex_rule = TexRule::Axis;
    d.place = Place::Axis;
    d.log = true;
    d
}

/// Посев (ASP-19): мгновенно, без коллизии, крест из двух плоскостей, слой по возрасту; пшеница — 8 стадий, прочие — 4.
const fn crop(mc: &'static str, b: BlockId, wheat: bool, base: u32) -> BlockDef {
    let mut d = plant(mc, b, 0, false, &[], Offset::None, Loot::Crop, Support::Crop, base);
    d.sword_fast = false;
    d.shape = if wheat { Shape::Wheat } else { Shape::Root };
    d.tex_rule = TexRule::Crop { base, wheat };
    d.random = RandomTick::Crop;
    d.flat_item = false;
    d
}

/// Руда (MEK-05 §3.1): твёрдость 3, кирка; needs — ярус кирки для дропа (1 каменная, 2 железная).
const fn ore(mc: &'static str, b: BlockId, needs: u8, loot: &'static [Drop], l: u32) -> BlockDef {
    let mut d = solid(mc, b, 3.0, true, PICKAXE, Snd::Stone, Loot::Table(loot), all(l));
    d.needs = needs;
    d
}
/// Машина Mekanism — своя модель (web/render/chest.js по models.json), лицо к игроку, «активна» — бит LIT (3),
/// блок-сущность Mek; коллизия и луч — полный куб (формы моделей — со срезом моделей Ф3). Иконка — куб по tex.
const fn machine(mc: &'static str, b: BlockId, k: MekKind, hardness: f32, tex: [u32; 6], light: u8) -> BlockDef {
    let mut d = solid(mc, b, hardness, true, PICKAXE, Snd::Metal, Loot::Itself, tex);
    d.render = Render::Model(Model::Mek);
    d.suffocates = false;
    d.place = Place::FacePlayer;
    d.on_use = OnUse::Machine;
    d.be = Some(BeKind::Mek(k));
    d.light = light;
    d.lit_only = true;
    d
}
/// Камера Mekanism (MEK-06): тот же блок машины, но рисуется кубом — лицо к игроку, у работающей лицо «работа»
/// (бит LIT, как у печи; анимация кадров лица — со срезом вида), свет 8 у работающей (со светом C1).
const fn chamber(mc: &'static str, b: BlockId, k: MekKind, front: u32, front_on: u32) -> BlockDef {
    let mut d = machine(mc, b, k, 3.5, [L_MEK_SIDE, L_MEK_SIDE, L_MEK_TOP, L_MEK_BOTTOM, L_MEK_SIDE, front], 8);
    d.render = Render::Cube;
    d.suffocates = true;
    d.tex_rule = TexRule::Facing { lit: front_on };
    d
}

/// Строка таблицы для номера блока.
pub(crate) const fn block_def(b: BlockId) -> BlockDef {
    use Snd::*;
    // Варианты травы: поведение и предмет — блока травы (base GRASS, тег #dirt).
    let grass = {
        let mut d = solid("grass_block", GRASS, 0.6, false, SHOVEL, Grass, Loot::Table(DROP_DIRT), cap(L_GRASS_TOP, L_DIRT, L_GRASS_SIDE));
        d.dirt = true;
        d.grass = true;
        d
    };
    match b {
        AIR => EMPTY_BLOCK,
        // Верх красится оттенком; у блока 1 (предмет, иконка) кайма бока запечена.
        GRASS => {
            let mut d = grass;
            d.tint = 1 << 2;
            d
        }
        DRY_GRASS => {
            let mut d = grass;
            d.tex = cap(L_DRY_TOP, L_DIRT, L_DRY_SIDE);
            d
        }
        DARK_GRASS => {
            let mut d = grass;
            d.tex = cap(L_DARK_TOP, L_DIRT, L_DARK_SIDE);
            d
        }
        SNOWY_GRASS => {
            let mut d = grass;
            d.tex = cap(L_SNOW, L_DIRT, L_SNOWY_SIDE);
            d.grass = false; // растительность генератора на ней не растёт (is_grass)
            d
        }
        // Бока травы в мире — слои с маской (земля не красится): ступень в 1 блок — глубокая кайма.
        GRASS_FULL => {
            let mut d = grass;
            d.tex = cap(L_GRASS_TOP, L_DIRT, L_GRASS_STEP_T);
            d.tint = TINT_NOT_BOTTOM;
            d
        }
        // Варианты каймы (номера блоков прежние): бок — один слой с маской, цвет — оттенок столбца.
        b if b >= GRASS_EDGE && b < GRASS_EDGE + GRASS_EDGES => {
            let mut d = grass;
            d.tex = cap(L_GRASS_TOP, L_DIRT, L_GRASS_SIDE_T);
            d.tint = TINT_NOT_BOTTOM;
            d
        }
        DIRT => {
            let mut d = solid("dirt", DIRT, 0.5, false, SHOVEL, Gravel, Loot::Itself, all(L_DIRT));
            d.dirt = true;
            d
        }
        STONE => solid("stone", STONE, 1.5, true, PICKAXE, Stone, Loot::Table(DROP_COBBLE), all(L_STONE)),
        SAND => {
            let mut d = solid("sand", SAND, 0.5, false, SHOVEL, Sand, Loot::Itself, all(L_SAND));
            d.falling = true;
            d
        }
        GRAVEL => {
            let mut d = solid("gravel", GRAVEL, 0.6, false, SHOVEL, Gravel, Loot::Table(DROP_GRAVEL), all(L_GRAVEL));
            d.falling = true;
            d
        }
        SNOW => solid("snow_block", SNOW, 0.2, true, SHOVEL, Snow, Loot::Table(DROP_SNOW_BLOCK), all(L_SNOW)),
        SANDSTONE => solid("sandstone", SANDSTONE, 0.8, true, PICKAXE, Stone, Loot::Itself, cap(L_SANDSTONE_TOP, L_SANDSTONE_TOP, L_SANDSTONE)),
        CLAY => solid("clay", CLAY, 0.6, false, SHOVEL, Gravel, Loot::Table(DROP_CLAY), all(L_CLAY)),
        LOG => log("oak_log", LOG, L_LOG_TOP, L_LOG),
        BIRCH_LOG => log("birch_log", BIRCH_LOG, L_BIRCH_TOP, L_BIRCH),
        SPRUCE_LOG => log("spruce_log", SPRUCE_LOG, L_SPRUCE_TOP, L_SPRUCE),
        LEAVES => leaves("oak_leaves", LEAVES, Loot::Table(DROP_OAK_LEAVES), L_LEAVES),
        BIRCH_LEAVES => leaves("birch_leaves", BIRCH_LEAVES, Loot::Table(DROP_LEAVES), L_BIRCH_LEAVES),
        SPRUCE_LEAVES => leaves("spruce_leaves", SPRUCE_LEAVES, Loot::Table(DROP_LEAVES), L_SPRUCE_LEAVES),
        COBBLE => solid("cobblestone", COBBLE, 2.0, true, PICKAXE, Stone, Loot::Itself, all(L_COBBLE)),
        BEDROCK => solid("bedrock", BEDROCK, -1.0, false, 0, Stone, Loot::None, all(L_BEDROCK)),
        CACTUS => {
            // Коллизия 14/16 × 15/16 (CactusBlock.COLLISION_SHAPE): коробка игрока заходит в клетку на 1/16 — отсюда урон
            // при касании (ASP-12 §3.2, game/hazards.rs); обводка выше на пиксель.
            let mut d = solid("cactus", CACTUS, 0.4, false, 0, Wool, Loot::Itself, all(L_CACTUS));
            d.suffocates = false;
            d.collision = CACTUS_SIDES;
            d.outline = CACTUS_OUTLINE;
            d.sides = Sides::Box(CACTUS_SIDES);
            d.support = Support::Cactus;
            d
        }
        TALL_GRASS => {
            let mut d = plant("short_grass", TALL_GRASS, AXE | HOE, true, SHORT_PLANT, Offset::Xyz, Loot::Table(DROP_SHORT_GRASS), Support::Plant, L_SHORT_GRASS);
            d.tint = TINT_ALL;
            d.sinks = true;
            d
        }
        FERN => {
            let mut d = plant("fern", FERN, AXE | HOE, true, SHORT_PLANT, Offset::Xyz, Loot::Table(DROP_SHORT_GRASS), Support::Plant, L_FERN);
            d.tint = TINT_ALL;
            d.sinks = true;
            d
        }
        DEAD_BUSH => plant("dead_bush", DEAD_BUSH, AXE | HOE, true, SHORT_PLANT, Offset::None, Loot::Table(DROP_DEAD_BUSH), Support::DeadBush, L_DEAD_BUSH),
        DANDELION => {
            let mut d = plant("dandelion", DANDELION, 0, false, FLOWER, Offset::Xz, Loot::Itself, Support::Plant, L_DANDELION);
            d.sinks = true;
            d
        }
        POPPY => {
            let mut d = plant("poppy", POPPY, 0, false, FLOWER, Offset::Xz, Loot::Itself, Support::Plant, L_POPPY);
            d.sinks = true;
            d
        }
        WATER => {
            let mut d = EMPTY_BLOCK;
            d.mc = "water";
            d.base = WATER;
            d.hardness = 100.0;
            d.render = Render::Water;
            d.tex = all(L_WATER);
            d.fluid = FLUID_WATER;
            d
        }
        // Команда items (G7): обсидиан (лава-источник под водой), твёрдость 50, кирка не ниже алмазного яруса (3).
        OBSIDIAN => {
            let mut d = solid("obsidian", OBSIDIAN, 50.0, true, PICKAXE, Stone, Loot::Itself, all(L_BEDROCK));
            d.needs = 3;
            d
        }
        // Ферма (ASP-19): пашня 15/16 (влажность — биты 0–2), посевы (возраст — биты 0–2), стог сена (ось как у бревна).
        FARMLAND => {
            let mut d = solid("farmland", FARMLAND, 0.6, false, SHOVEL, Gravel, Loot::Table(DROP_DIRT), all(L_DIRT));
            d.outline = FARMLAND_SHAPE;
            d.collision = FARMLAND_SHAPE;
            d.suffocates = false;
            d.farmland = true;
            d.tex_rule = TexRule::Farmland;
            d.random = RandomTick::Farmland;
            d
        }
        WHEAT => crop("wheat", WHEAT, true, L_WHEAT),
        CARROTS => crop("carrots", CARROTS, false, L_CARROTS),
        POTATOES => crop("potatoes", POTATOES, false, L_POTATOES),
        HAY_BLOCK => {
            let mut d = solid("hay_block", HAY_BLOCK, 0.5, false, HOE, Grass, Loot::Itself, cap(L_HAY_TOP, L_HAY_TOP, L_HAY_SIDE));
            d.tex_rule = TexRule::Axis;
            d.place = Place::Axis;
            d
        }
        // Блоки срезов 2–3 (ASP-06 §2.1).
        PLANKS => solid("oak_planks", PLANKS, 2.0, false, AXE, Wood, Loot::Itself, all(L_OAK_PLANKS)),
        BIRCH_PLANKS => solid("birch_planks", BIRCH_PLANKS, 2.0, false, AXE, Wood, Loot::Itself, all(L_BIRCH_PLANKS)),
        SPRUCE_PLANKS => solid("spruce_planks", SPRUCE_PLANKS, 2.0, false, AXE, Wood, Loot::Itself, all(L_SPRUCE_PLANKS)),
        CRAFTING_TABLE => {
            // crafting_table.json: верх, низ — доски, лицо — север и запад, бок — восток и юг.
            let tex = [L_TABLE_SIDE, L_TABLE_FRONT, L_TABLE_TOP, L_OAK_PLANKS, L_TABLE_SIDE, L_TABLE_FRONT];
            let mut d = solid("crafting_table", CRAFTING_TABLE, 2.5, false, AXE, Wood, Loot::Itself, tex);
            d.on_use = OnUse::Crafting;
            d
        }
        FURNACE => {
            let mut d = solid("furnace", FURNACE, 3.5, true, PICKAXE, Stone, Loot::Itself, faced(L_FURNACE_TOP, L_FURNACE_SIDE, L_FURNACE_FRONT));
            d.tex_rule = TexRule::Facing { lit: L_FURNACE_FRONT_ON };
            d.place = Place::FacePlayer;
            d.on_use = OnUse::Furnace;
            d.be = Some(BeKind::Furnace);
            d.display = Display::Furnace;
            d.light = 13;
            d.lit_only = true;
            d
        }
        COAL_ORE => solid("coal_ore", COAL_ORE, 3.0, true, PICKAXE, Stone, Loot::Table(DROP_COAL), all(L_COAL_ORE)),
        IRON_ORE => {
            let mut d = solid("iron_ore", IRON_ORE, 3.0, true, PICKAXE, Stone, Loot::Table(DROP_RAW_IRON), all(L_IRON_ORE));
            d.needs = 1;
            d
        }
        // Блоки железа и сырого железа — слои арта Mekanism (decisions: «вместо заглушек»); у угля своей текстуры нет.
        IRON_BLOCK => {
            let mut d = solid("iron_block", IRON_BLOCK, 5.0, true, PICKAXE, Metal, Loot::Itself, all(L_IRON_BLOCK));
            d.needs = 1;
            d
        }
        RAW_IRON_BLOCK => {
            let mut d = solid("raw_iron_block", RAW_IRON_BLOCK, 5.0, true, PICKAXE, Stone, Loot::Itself, all(L_RAW_IRON_BLOCK));
            d.needs = 1;
            d
        }
        COAL_BLOCK => solid("coal_block", COAL_BLOCK, 5.0, true, PICKAXE, Stone, Loot::Itself, all(L_BEDROCK)),
        GLASS => {
            // Стекло: без инструмента, без дропа (нужно шёлковое касание); не душит (Blocks.never), куб для выталкивания.
            let mut d = solid("glass", GLASS, 0.3, false, 0, Glass, Loot::None, all(L_GLASS));
            d.suffocates = false;
            d.render = Render::Glass;
            d.never_solid = true;
            d
        }
        TORCH => {
            let mut d = plant("torch", TORCH, 0, false, TORCH_SHAPE, Offset::None, Loot::Itself, Support::Floor, L_TORCH);
            d.sword_fast = false;
            d.sound = Wood;
            d.place = Place::Torch { wall: WALL_TORCH };
            d.light = 14;
            d
        }
        WALL_TORCH => {
            // Опора — полная грань блока сзади (по FACING), обводка — по стороне.
            let mut d = plant("wall_torch", TORCH, 0, false, WALL_TORCH_SHAPES[0], Offset::None, Loot::Itself, Support::Wall, L_TORCH);
            d.sword_fast = false;
            d.sound = Wood;
            d.render = Render::Model(Model::WallTorch);
            d.shape = Shape::WallTorch;
            d.flat_item = false;
            d.light = 14;
            d
        }
        CHEST => {
            let mut d = solid("chest", CHEST, 2.5, false, AXE, Wood, Loot::Itself, faced(L_CHEST_TOP, L_CHEST_SIDE, L_CHEST_FRONT));
            d.outline = CHEST_SHAPE;
            d.collision = CHEST_SHAPE;
            d.suffocates = false;
            d.render = Render::Model(Model::Chest);
            d.place = Place::FacePlayer;
            d.on_use = OnUse::Chest;
            d.be = Some(BeKind::Chest);
            d
        }
        LADDER => {
            let mut d = solid("ladder", LADDER, 0.4, false, AXE, Ladder, Loot::Itself, all(L_LADDER));
            d.outline = LADDER_SHAPE;
            d.collision = LADDER_SHAPE;
            d.suffocates = false;
            d.render = Render::Model(Model::Ladder);
            d.shape = Shape::Ladder;
            d.support = Support::Wall;
            d.place = Place::Ladder;
            d.climbable = true;
            d.flat_item = true; // плоский предмет (item/generated), как растения
            d
        }
        SNOW_LAYER => {
            let mut d = solid("snow", SNOW_LAYER, 0.1, true, SHOVEL, Snow, Loot::SnowLayers, all(L_SNOW));
            d.outline = SNOW_LAYER_SHAPE;
            d.collision = &[];
            d.suffocates = false;
            d.replaceable = true; // слой 1 заменяем (по состоянию — replaceable_by)
            d.render = Render::Model(Model::Snow);
            d.shape = Shape::Layers;
            d.sides = Sides::Outline;
            d.support = Support::Layers;
            d.place = Place::Layers;
            d
        }
        LAVA => {
            // Неподвижный источник: без формы (луч и физика проходят), заменяем, светит 15. Течение — ASP-15.
            let mut d = EMPTY_BLOCK;
            d.mc = "lava";
            d.base = LAVA;
            d.hardness = 100.0;
            d.render = Render::Cube;
            d.tex = all(L_LAVA);
            d.light = 15;
            d.fluid = FLUID_LAVA; // касание: ожог и урон (game/hazards.rs, player.rs)
            d
        }
        OSMIUM_ORE => ore("osmium_ore", OSMIUM_ORE, 1, DROP_RAW_OSMIUM, L_OSMIUM_ORE),
        TIN_ORE => ore("tin_ore", TIN_ORE, 1, DROP_RAW_TIN, L_TIN_ORE),
        COPPER_ORE => ore("copper_ore", COPPER_ORE, 1, DROP_RAW_COPPER, L_COPPER_ORE),
        GOLD_ORE => ore("gold_ore", GOLD_ORE, 2, DROP_RAW_GOLD, L_GOLD_ORE),
        REDSTONE_ORE => {
            // «Горит» (LIT, свет 9) — после касания (MEK-05 §3.2; зажигание и случайное угасание — позже).
            let mut d = ore("redstone_ore", REDSTONE_ORE, 2, DROP_REDSTONE, L_REDSTONE_ORE);
            d.light = 9;
            d.lit_only = true;
            d
        }
        STEEL_CASING => solid("steel_casing", STEEL_CASING, 3.5, true, PICKAXE, Metal, Loot::Itself, all(L_STEEL_CASING)),
        STEEL_BLOCK => solid("block_steel", STEEL_BLOCK, 5.0, true, PICKAXE, Metal, Loot::Itself, all(L_STEEL_BLOCK)),
        OSMIUM_BLOCK => {
            let mut d = solid("block_osmium", OSMIUM_BLOCK, 7.5, true, PICKAXE, Metal, Loot::Itself, all(L_OSMIUM_BLOCK));
            d.needs = 1;
            d
        }
        REDSTONE_BLOCK => solid("redstone_block", REDSTONE_BLOCK, 5.0, true, PICKAXE, Metal, Loot::Itself, all(L_REDSTONE_BLOCK)),
        // Шерсть (ASP-17 §5.1): твёрдость 0.8, инструмент не нужен, звуки «шерсть»; ножницы — скорость 5 (tool_speed).
        WHITE_WOOL => solid("white_wool", WHITE_WOOL, 0.8, false, 0, Wool, Loot::Itself, all(L_WOOL_WHITE)),
        BLACK_WOOL => solid("black_wool", BLACK_WOOL, 0.8, false, 0, Wool, Loot::Itself, all(L_WOOL_BLACK)),
        GRAY_WOOL => solid("gray_wool", GRAY_WOOL, 0.8, false, 0, Wool, Loot::Itself, all(L_WOOL_GRAY)),
        LIGHT_GRAY_WOOL => solid("light_gray_wool", LIGHT_GRAY_WOOL, 0.8, false, 0, Wool, Loot::Itself, all(L_WOOL_LIGHT_GRAY)),
        BROWN_WOOL => solid("brown_wool", BROWN_WOOL, 0.8, false, 0, Wool, Loot::Itself, all(L_WOOL_BROWN)),
        PINK_WOOL => solid("pink_wool", PINK_WOOL, 0.8, false, 0, Wool, Loot::Itself, all(L_WOOL_PINK)),
        // Генератор: энергия только с лица (порт); свет 13 — пока горит топливо (MEK-03 §1.4).
        HEAT_GENERATOR => machine("heat_generator", HEAT_GENERATOR, MekKind::Heat, 3.5, faced(L_HEATGEN_DRUM, L_HEATGEN_BODY, L_MEK_BACK), 13),
        METALLURGIC_INFUSER => machine("metallurgic_infuser", METALLURGIC_INFUSER, MekKind::Infuser, 3.5, faced(L_MEK_TOP, L_INFUSER_BODY, L_INFUSER_BODY), 8),
        ENRICHMENT_CHAMBER => chamber("enrichment_chamber", ENRICHMENT_CHAMBER, MekKind::Enrich, L_ENRICH_FRONT, L_ENRICH_FRONT_ON),
        ENERGIZED_SMELTER => chamber("energized_smelter", ENERGIZED_SMELTER, MekKind::Smelter, L_SMELTER_FRONT, L_SMELTER_FRONT_ON),
        ENERGY_CUBE => {
            let mut d = machine("basic_energy_cube", ENERGY_CUBE, MekKind::Cube, 2.0, all(L_CUBE_FRAME), 0);
            d.lit_only = false;
            d
        }
        // Ветряк (MEK-03 §3.5): основание — модель на 5 блоков (мачта и голова — над ним, лопасти — инстансом), свет 8 у
        // работающего; ограничивающие блоки — мачта: форма столба, без меша, свет неба не гасят, ломание и ПКМ — основанию.
        CRUSHER => chamber("crusher", CRUSHER, MekKind::Crusher, L_CRUSHER_FRONT, L_CRUSHER_FRONT_ON),
        PURIFICATION_CHAMBER => chamber("purification_chamber", PURIFICATION_CHAMBER, MekKind::Purifier, L_PURIFIER_FRONT, L_PURIFIER_FRONT_ON),
        // Сепаратор (MEK-11 §4.2): модель (art/mekanism/models.json), свет 8 у работающего.
        ELECTROLYTIC_SEPARATOR => machine("electrolytic_separator", ELECTROLYTIC_SEPARATOR, MekKind::Separator, 3.5, faced(L_MEK_TOP, L_SEPARATOR_BODY, L_SEPARATOR_BODY), 8),
        WIND_GENERATOR => machine("wind_generator", WIND_GENERATOR, MekKind::Wind, 3.5, faced(L_MEK_TOP, L_MEK_SIDE, L_MEK_BACK), 8),
        BOUNDING => {
            let mut d = solid("bounding_block", WIND_GENERATOR, 3.5, true, PICKAXE, Metal, Loot::None, all(L_MEK_SIDE));
            d.outline = MAST;
            d.collision = MAST;
            d.suffocates = false;
            d.render = Render::None;
            d.sides = Sides::Empty;
            d.on_use = OnUse::Machine;
            d.never_solid = true;
            d
        }
        UNIVERSAL_CABLE => {
            let mut d = solid("basic_universal_cable", UNIVERSAL_CABLE, 1.0, false, PICKAXE, Metal, Loot::Itself, all(L_CABLE_SIDE));
            d.outline = CABLE_CORE;
            d.collision = CABLE_CORE;
            d.suffocates = false;
            d.render = Render::Model(Model::Cable);
            d.shape = Shape::Cable;
            d.sides = Sides::Empty;
            d
        }
        // M5 (MEK-14): транспортёр и механическая труба — большие передатчики (4…12), газовая трубка — малый (как кабель).
        // Рисуются тем же путём, что кабель (Model::Cable, вид передатчика — в параметре), ломаются рукой ~1.5 с.
        LOGISTICAL_TRANSPORTER | MECHANICAL_PIPE | PRESSURIZED_TUBE => {
            let (mc, l) = match b {
                LOGISTICAL_TRANSPORTER => ("basic_logistical_transporter", L_TRANSPORTER_SIDE),
                MECHANICAL_PIPE => ("basic_mechanical_pipe", L_PIPE_SIDE),
                _ => ("basic_pressurized_tube", L_TUBE_SIDE),
            };
            let large = b != PRESSURIZED_TUBE;
            let mut d = solid(mc, b, 1.0, false, PICKAXE, Metal, Loot::Itself, all(l));
            d.outline = if large { LARGE_CORE } else { CABLE_CORE };
            d.collision = d.outline;
            d.suffocates = false;
            d.render = Render::Model(Model::Cable);
            d.shape = if large { Shape::Large } else { Shape::Cable };
            d.sides = Sides::Empty;
            d
        }
        // Насос (MEK-14 §11.4): куб из слоёв арта (модель — со срезом вида), лицо к игроку, энергия — сзади; свет 8 у
        // работающего (decisions после MEK-13/14 — в отличие от мода).
        ELECTRIC_PUMP => {
            let mut d = machine("electric_pump", ELECTRIC_PUMP, MekKind::Pump, 3.5, cap(L_PUMP_TOP, L_PUMP_BOTTOM, L_PUMP_SIDE), 8);
            d.render = Render::Cube;
            d.suffocates = true;
            d
        }
        _ => UNKNOWN,
    }
}

const fn build_blocks() -> [BlockDef; BLOCK_COUNT] {
    let mut t = [EMPTY_BLOCK; BLOCK_COUNT];
    let mut i = 0;
    while i < BLOCK_COUNT {
        t[i] = block_def(i as BlockId);
        i += 1;
    }
    t
}

static BLOCKS: [BlockDef; BLOCK_COUNT] = build_blocks();

// Плотные флаги из таблицы для горячих путей генератора и меша (байт на блок, вся выборка — в L1): непрозрачность
// соседей и углов, крест, листва, модель, трава. Источник — та же строка таблицы.
const K_OPAQUE: u8 = 1;
const K_CROSS: u8 = 2;
const K_LEAVES: u8 = 4;
const K_MODEL: u8 = 8;
const K_GRASS: u8 = 16;
/// Вода рисует грань к этому блоку (воздух, стекло, крест, модель).
const K_WATER_FACE: u8 = 32;

static KIND: [u8; BLOCK_COUNT] = {
    let mut t = [0u8; BLOCK_COUNT];
    let mut i = 0;
    while i < BLOCK_COUNT {
        let d = block_def(i as BlockId);
        let mut k = 0;
        if d.render.opaque() {
            k |= K_OPAQUE;
        }
        if matches!(d.render, Render::Cross) {
            k |= K_CROSS;
        }
        if d.leaves {
            k |= K_LEAVES;
        }
        if matches!(d.render, Render::Model(_)) {
            k |= K_MODEL;
        }
        if d.grass {
            k |= K_GRASS;
        }
        if matches!(d.render, Render::None | Render::Glass | Render::Cross | Render::Model(_)) {
            k |= K_WATER_FACE;
        }
        t[i] = k;
        i += 1;
    }
    t
};

/// Флаги блока (номер вне таблицы — как свободный номер: непрозрачный куб).
#[inline]
fn kind(b: BlockId) -> u8 {
    match KIND.get(b as usize) {
        Some(&k) => k,
        None => K_OPAQUE,
    }
}

/// Строка таблицы (номер вне таблицы — как свободный номер).
#[inline]
pub(crate) fn bdef(b: BlockId) -> &'static BlockDef {
    BLOCKS.get(b as usize).unwrap_or(&UNKNOWN)
}

/// Блок по имени Minecraft (первый с таким именем: варианты травы — блок травы; 0 — воздух или нет такого).
pub(crate) fn block_by_mc(name: &str) -> BlockId {
    (0..BLOCK_COUNT).find(|&i| BLOCKS[i].mc == name).unwrap_or(0) as BlockId
}

// ---------------------------------------------------------------- запросы к таблице

/// Закрывает ли блок грань соседа (листва закрывает листву — как «быстрая» графика Minecraft). Стекло не закрывает:
/// соседи рисуют свою грань к нему. Модели меньше куба (сундук, лестница, настенный факел, снежный слой) тоже нет.
#[inline]
pub(crate) fn opaque(b: BlockId) -> bool {
    kind(b) & K_OPAQUE != 0
}
/// Рисуется крестом (cutout): растения и стоячий факел.
#[inline]
pub(crate) fn is_plant(b: BlockId) -> bool {
    kind(b) & K_CROSS != 0
}
/// Трава (на ней генератор сажает растительность): все варианты, кроме заснеженной.
#[inline]
pub(crate) fn is_grass(b: BlockId) -> bool {
    kind(b) & K_GRASS != 0
}
#[inline]
pub(crate) fn is_leaves(b: BlockId) -> bool {
    kind(b) & K_LEAVES != 0
}
pub(crate) fn is_log(b: BlockId) -> bool {
    bdef(b).log
}
/// Блоки со своей моделью меньше куба: меш куска их не рисует (Render::Model).
#[inline]
pub(crate) fn is_model(b: BlockId) -> bool {
    kind(b) & K_MODEL != 0
}
/// Рисует ли вода грань к соседу nb (воздух, стекло, растение, модель; к воде и блокам — нет).
#[inline]
pub(crate) fn water_faces(nb: BlockId) -> bool {
    kind(nb) & K_WATER_FACE != 0
}
/// Вид модели блока (проход моделей) или None.
pub(crate) fn model_of(b: BlockId) -> Option<Model> {
    match bdef(b).render {
        Render::Model(m) => Some(m),
        _ => None,
    }
}
/// Падающий блок (FallingBlock): песок, гравий.
pub(crate) fn is_falling(b: BlockId) -> bool {
    bdef(b).falling
}
/// Слой атласа грани при состоянии 0 (face — как в меше).
#[inline]
pub(crate) fn layer(b: BlockId, face: usize) -> u32 {
    bdef(b).tex[face]
}
/// Красится ли грань (face 0…5, 6 — крест) оттенком столбца: серые слои атласа — верх травы, листва, трава и
/// папоротник; бока травы в мире — слои с маской (земля не красится).
#[inline]
pub(crate) fn tinted(b: BlockId, face: usize) -> bool {
    bdef(b).tint >> face & 1 != 0
}
/// Светимость ячейки 0…15 (на будущее: срез 3B).
#[allow(dead_code)]
pub(crate) fn light_of(c: Cell) -> u8 {
    let d = bdef(block_of(c));
    if d.lit_only && state_of(c) & ST_LIT == 0 {
        0
    } else {
        d.light
    }
}

/// Слой грани с учётом состояния (меш куска) и поворот текстуры на 90° (w0 бит 31, решение F1 после ASP-14):
/// бревно — торцы на гранях своей оси, кора на остальных; у лежачего бревна волокна идут вдоль оси — текстуру боковых
/// граней надо повернуть (по миру UV граней — worldUV: ±X (z, y), верх/низ (x, z), ±Z (x, y); у стоячего волокна по v).
/// Печь — лицо по FACING, горящая — «front_on». Остальные — слои таблицы.
pub(crate) fn layer_cell(b: BlockId, s: State, face: usize) -> (u32, bool) {
    let d = bdef(b);
    match d.tex_rule {
        TexRule::Fixed => (d.tex[face], false),
        TexRule::Axis => {
            let axis_faces = match s & 3 {
                1 => [0, 1],
                2 => [4, 5],
                _ => [2, 3],
            };
            if axis_faces.contains(&face) {
                return (d.tex[2], false);
            }
            // Ось X: волокна вдоль x — у ±Z (x, y) и верха/низа (x, z) u = x — повернуть; ось Z: у ±X (z, y) — повернуть,
            // у верха/низа (x, z) v = z — как есть.
            let rot = match s & 3 {
                1 => true,
                2 => face < 2,
                _ => false,
            };
            (d.tex[0], rot)
        }
        TexRule::Facing { lit } => {
            let front = mesh_face(facing_of(cell_of(b, s)));
            let l = if face == front {
                if s & ST_LIT != 0 { lit } else { d.tex[5] }
            } else if face == 2 || face == 3 {
                d.tex[face]
            } else {
                d.tex[0]
            };
            (l, false)
        }
        TexRule::Crop { base, wheat } => {
            let age = (s & 7) as u32;
            (base + if wheat { age } else { [0, 0, 1, 1, 2, 2, 2, 3][age as usize] }, false)
        }
        TexRule::Farmland => (if face != 2 { L_DIRT } else if s & 7 == 7 { L_FARMLAND_WET } else { L_FARMLAND_DRY }, false),
    }
}

/// Форма кабеля по рукавам ячейки (6 бит «есть рукав» по сторонам F_*): центр 5…11 и трубы до граней клетки.
fn cable_shape(c: Cell) -> &'static [Aabb] {
    let m = crate::world::grid::arm_mask(state_of(c)) as usize;
    &CABLE_SHAPES.0[m][..CABLE_SHAPES.1[m] as usize]
}
/// Форма большого передатчика (транспортёр, труба): центр 4…12 и рукава 8 × 8 до граней клетки.
fn large_shape(c: Cell) -> &'static [Aabb] {
    let m = crate::world::grid::arm_mask(state_of(c)) as usize;
    &LARGE_SHAPES.0[m][..LARGE_SHAPES.1[m] as usize]
}
/// Формы кабеля на все 64 маски рукавов (const: центр + до 6 рукавов).
static CABLE_SHAPES: ([[Aabb; 7]; 64], [u8; 64]) = transmitter_shapes(5.0, 11.0);
static LARGE_SHAPES: ([[Aabb; 7]; 64], [u8; 64]) = transmitter_shapes(4.0, 12.0);
/// Формы передатчика с центром a…b px на все 64 маски рукавов.
const fn transmitter_shapes(a: f64, b: f64) -> ([[Aabb; 7]; 64], [u8; 64]) {
    let arms = [
        px(a, 0.0, a, b, a, b),
        px(a, b, a, b, 16.0, b),
        px(a, a, 0.0, b, b, a),
        px(a, a, b, b, b, 16.0),
        px(0.0, a, a, a, b, b),
        px(b, a, a, 16.0, b, b),
    ];
    let mut t = [[px(a, a, a, b, b, b); 7]; 64];
    let mut n = [1u8; 64];
    let mut m = 0;
    while m < 64 {
        let mut f = 0;
        while f < 6 {
            if m >> f & 1 != 0 {
                t[m][n[m] as usize] = arms[f];
                n[m] += 1;
            }
            f += 1;
        }
        m += 1;
    }
    (t, n)
}

/// Обводка ячейки с учётом состояния (луч, рамка): лестница и настенный факел — по стороне, снежный слой — по слоям.
pub(crate) fn outline_of(c: Cell) -> &'static [Aabb] {
    let d = bdef(block_of(c));
    match d.shape {
        Shape::Fixed => d.outline,
        Shape::Ladder => LADDER_SHAPES[hidx(facing_of(c))],
        Shape::WallTorch => WALL_TORCH_SHAPES[hidx(facing_of(c))],
        Shape::Layers => SNOW_SHAPES[snow_layers(c) as usize],
        Shape::Cable => cable_shape(c),
        Shape::Large => large_shape(c),
        Shape::Wheat => WHEAT_SHAPES[(state_of(c) & 7) as usize],
        Shape::Root => ROOT_SHAPES[(state_of(c) & 7) as usize],
    }
}
/// Коллизия ячейки с учётом состояния (физика, «не в себя»): снежный слой — на слой ниже вида.
#[inline]
pub(crate) fn collision_of(c: Cell) -> &'static [Aabb] {
    let d = bdef(block_of(c));
    match d.shape {
        Shape::Ladder => LADDER_SHAPES[hidx(facing_of(c))],
        Shape::Layers => SNOW_SHAPES[snow_layers(c) as usize - 1],
        Shape::Cable => cable_shape(c),
        Shape::Large => large_shape(c),
        Shape::Fixed | Shape::WallTorch | Shape::Wheat | Shape::Root => d.collision,
    }
}
/// Форма граней для опоры (getSidesShape): коллизия; у листвы — пусто (на листву не повесить факел и лестницу), у
/// снежного слоя — вид, у кактуса — его настоящая коллизия 1…15 (у нас пока куб).
pub(crate) fn sides_of(c: Cell) -> &'static [Aabb] {
    match bdef(block_of(c)).sides {
        Sides::Empty => &[],
        Sides::Box(s) => s,
        Sides::Outline => outline_of(c),
        Sides::Collision => collision_of(c),
    }
}
/// Полная грань клетки в сторону dir (isSideSolidFullSquare): коробка формы граней касается границы клетки и
/// закрывает весь квадрат 1 × 1. Опора настенного факела и лестницы.
pub(crate) fn face_full(c: Cell, dir: u8) -> bool {
    let d = DIRS[dir as usize];
    let a = d.iter().position(|&v| v != 0).unwrap_or(1);
    sides_of(c).iter().any(|s| {
        let touches = if d[a] > 0 { s.hi[a] >= 1.0 } else { s.lo[a] <= 0.0 };
        touches && (0..3).filter(|&k| k != a).all(|k| s.lo[k] <= 0.0 && s.hi[k] >= 1.0)
    })
}
/// Верх клетки закрывает центр 2 × 2 пикселя (sideCoversSmallSquare UP): опора стоячего факела.
pub(crate) fn top_center(c: Cell) -> bool {
    let (lo, hi) = (7.0 / 16.0, 9.0 / 16.0);
    sides_of(c).iter().any(|s| s.hi[1] >= 1.0 && s.lo[0] <= lo && s.hi[0] >= hi && s.lo[2] <= lo && s.hi[2] >= hi)
}
/// «Сплошной блок» (isSolidBlock): полный куб коллизии; листва и стекло — никогда (Blocks.never). Сундук под таким
/// блоком не открывается (ChestBlock.isChestBlocked).
pub(crate) fn solid_block(c: Cell) -> bool {
    let s = collision_of(c);
    !bdef(block_of(c)).never_solid && s.len() == 1 && s[0] == CUBE[0]
}
/// Можно ли поставить предмет item прямо в эту клетку (AbstractBlock.canReplace): заменяемый блок и не тот же предмет
/// (или пустой стек); слоистый (снег) — своим предметом при слоях < 8 (наращивается), чем-то другим — только слой 1
/// (SnowBlock.canReplace).
pub(crate) fn replaceable_by(c: Cell, item: ItemId) -> bool {
    let b = block_of(c);
    let d = bdef(b);
    if d.shape == Shape::Layers {
        let n = snow_layers(c);
        return if item == block_item(b) && n < 8 { true } else { n == 1 };
    }
    d.replaceable && (item == 0 || block_item(b) != item) // item 0 — пустой стек (приземление песка)
}
/// Падающий блок проходит сквозь клетку (FallingBlock.canFallThrough): воздух, жидкость, заменяемое — по флагу блока
/// (снежный слой любой высоты тоже: isReplaceable — флаг, а не canReplace; садится песок на него — уже нет).
pub(crate) fn falls_through(c: Cell) -> bool {
    bdef(block_of(c)).replaceable
}

/// Блок из тега #dirt (опора травы и цветов).
pub(crate) fn is_dirt_tag(b: BlockId) -> bool {
    bdef(b).dirt
}
/// Пашня (farmland) — флаг появится вместе с блоком (ASP-19).
pub(crate) fn is_farmland(b: BlockId) -> bool {
    bdef(b).farmland
}
/// Может ли растение plant стоять на блоке below (PlantBlock.canPlantOnTop: #dirt или пашня; мёртвый куст — свой тег;
/// факел — верх опоры закрывает центр 2×2: полный куб коллизии, кроме листвы — у неё «форма граней» пустая).
pub(crate) fn plant_supported(plant: BlockId, below: BlockId) -> bool {
    match bdef(plant).support {
        Support::Plant => is_dirt_tag(below) || is_farmland(below),
        Support::DeadBush => is_dirt_tag(below) || below == SAND,
        Support::Floor => is_full_cube(below) && !is_leaves(below),
        _ => true,
    }
}
/// Полный куб по форме коллизии (BlockState.isFullCube): выталкивание предметов, опора факела.
pub(crate) fn is_full_cube(b: BlockId) -> bool {
    let c = bdef(b).collision;
    c.len() == 1 && c[0] == CUBE[0]
}

// ---------------------------------------------------------------- смещение растений

/// MathHelper.hashCode(x, y, z) из Minecraft (long-арифметика с переполнением).
pub(crate) fn mc_hash(x: i32, y: i32, z: i32) -> i64 {
    let mut l = (x.wrapping_mul(3129871) as i64) ^ (z as i64).wrapping_mul(116129781) ^ (y as i64);
    l = l.wrapping_mul(l).wrapping_mul(42317861).wrapping_add(l.wrapping_mul(11));
    l >> 16
}

/// Смещение модели растения в клетке (AbstractBlock.getModelOffset): XZ ±0.25 шагом 1/30, XYZ — ещё вниз до 0.2.
/// Хеш — по (x, 0, z). Шейдер повторяет это в 32-битной арифметике (младшие биты хеша те же).
pub(crate) fn plant_offset(b: BlockId, x: i32, z: i32) -> [f64; 3] {
    let kind = bdef(b).offset;
    if kind == Offset::None {
        return [0.0; 3];
    }
    let l = mc_hash(x, 0, z);
    let h = |s: i64| ((((l >> s) & 15) as f32 / 15.0) as f64 - 0.5) * 0.5;
    let (dx, dz) = (h(0).clamp(-0.25, 0.25), h(8).clamp(-0.25, 0.25));
    let dy = if kind == Offset::Xyz { ((((l >> 4) & 15) as f32 / 15.0) as f64 - 1.0) * (0.2f32 as f64) } else { 0.0 };
    [dx, dy, dz]
}

/// Обводка ячейки c (для луча и рамки) в мировых координатах. offsets — смещать ли цветы (меш смещает растения).
pub(crate) fn outline_at(c: Cell, x: i32, y: i32, z: i32, offsets: bool, out: &mut Vec<Aabb>) {
    let b = block_of(c);
    let d = bdef(b);
    let o = if offsets && d.outline_offset { plant_offset(b, x, z) } else { [0.0; 3] };
    let p = [x as f64 + o[0], y as f64 + o[1], z as f64 + o[2]];
    for s in outline_of(c) {
        out.push(Aabb { lo: [p[0] + s.lo[0], p[1] + s.lo[1], p[2] + s.lo[2]], hi: [p[0] + s.hi[0], p[1] + s.hi[1], p[2] + s.hi[2]] });
    }
}

#[cfg(test)]
mod table_tests {
    use super::*;

    /// Прежние разбросанные правила (до Ф1) — эталон для таблицы: та же непрозрачность, крест, модель, оттенок и
    /// слои граней для каждого номера 0…255.
    fn old_is_plant(b: BlockId) -> bool {
        matches!(b, TALL_GRASS | DANDELION | POPPY | DEAD_BUSH | FERN | TORCH)
    }
    fn old_is_model(b: BlockId) -> bool {
        matches!(b, CHEST | LADDER | WALL_TORCH | SNOW_LAYER)
    }
    fn old_opaque(b: BlockId) -> bool {
        b != AIR && b != WATER && b != GLASS && !old_is_plant(b) && !old_is_model(b)
    }
    fn edge(b: BlockId) -> bool {
        (GRASS_EDGE..GRASS_EDGE + GRASS_EDGES).contains(&b)
    }
    fn old_tinted(b: BlockId, face: usize) -> bool {
        match b {
            GRASS_FULL => face != 3,
            GRASS => face == 2,
            TALL_GRASS | FERN | LEAVES | BIRCH_LEAVES | SPRUCE_LEAVES => true,
            _ => face != 3 && edge(b),
        }
    }
    fn old_layer(b: BlockId, face: usize) -> u32 {
        let (top, bottom) = (face == 2, face == 3);
        match b {
            GRASS => if top { L_GRASS_TOP } else if bottom { L_DIRT } else { L_GRASS_SIDE },
            DRY_GRASS => if top { L_DRY_TOP } else if bottom { L_DIRT } else { L_DRY_SIDE },
            DARK_GRASS => if top { L_DARK_TOP } else if bottom { L_DIRT } else { L_DARK_SIDE },
            SNOWY_GRASS => if top { L_SNOW } else if bottom { L_DIRT } else { L_SNOWY_SIDE },
            GRASS_FULL => if bottom { L_DIRT } else if top { L_GRASS_TOP } else { L_GRASS_STEP_T },
            b if edge(b) => if top { L_GRASS_TOP } else if bottom { L_DIRT } else { L_GRASS_SIDE_T },
            DIRT => L_DIRT,
            STONE => L_STONE,
            SAND => L_SAND,
            GRAVEL => L_GRAVEL,
            SNOW => L_SNOW,
            SANDSTONE => if top || bottom { L_SANDSTONE_TOP } else { L_SANDSTONE },
            CLAY => L_CLAY,
            LOG => if top || bottom { L_LOG_TOP } else { L_LOG },
            LEAVES => L_LEAVES,
            BIRCH_LOG => if top || bottom { L_BIRCH_TOP } else { L_BIRCH },
            BIRCH_LEAVES => L_BIRCH_LEAVES,
            SPRUCE_LOG => if top || bottom { L_SPRUCE_TOP } else { L_SPRUCE },
            SPRUCE_LEAVES => L_SPRUCE_LEAVES,
            WATER => L_WATER,
            CACTUS => L_CACTUS,
            TALL_GRASS => L_SHORT_GRASS,
            DANDELION => L_DANDELION,
            POPPY => L_POPPY,
            DEAD_BUSH => L_DEAD_BUSH,
            FERN => L_FERN,
            COBBLE => L_COBBLE,
            BEDROCK => L_BEDROCK,
            PLANKS => L_OAK_PLANKS,
            BIRCH_PLANKS => L_BIRCH_PLANKS,
            SPRUCE_PLANKS => L_SPRUCE_PLANKS,
            CRAFTING_TABLE => if top { L_TABLE_TOP } else if bottom { L_OAK_PLANKS } else if face == 5 || face == 1 { L_TABLE_FRONT } else { L_TABLE_SIDE },
            FURNACE => if top || bottom { L_FURNACE_TOP } else if face == 5 { L_FURNACE_FRONT } else { L_FURNACE_SIDE },
            COAL_ORE => L_COAL_ORE,
            IRON_ORE => L_IRON_ORE,
            TORCH | WALL_TORCH => L_TORCH,
            GLASS => L_GLASS,
            IRON_BLOCK => L_IRON_BLOCK,
            RAW_IRON_BLOCK => L_RAW_IRON_BLOCK,
            COAL_BLOCK => L_BEDROCK,
            CHEST => if top || bottom { L_CHEST_TOP } else if face == 5 { L_CHEST_FRONT } else { L_CHEST_SIDE },
            LADDER => L_LADDER,
            SNOW_LAYER => L_SNOW,
            _ => L_STONE,
        }
    }

    #[test]
    fn table_matches_old_rules() {
        // Прежние номера 0…55; дальше — лава, блоки Mekanism M1/M2 и команды items (свои проверки ниже).
        for i in 0..LAVA as usize {
            let b = i as BlockId;
            assert_eq!(opaque(b), old_opaque(b), "непрозрачность {b}");
            assert_eq!(is_plant(b), old_is_plant(b), "крест {b}");
            assert_eq!(is_model(b), old_is_model(b), "модель {b}");
            assert_eq!(is_leaves(b), matches!(b, LEAVES | BIRCH_LEAVES | SPRUCE_LEAVES), "листва {b}");
            assert_eq!(is_log(b), matches!(b, LOG | BIRCH_LOG | SPRUCE_LOG), "бревно {b}");
            assert_eq!(is_falling(b), matches!(b, SAND | GRAVEL), "падает {b}");
            assert_eq!(is_grass(b), matches!(b, GRASS | DRY_GRASS | DARK_GRASS | GRASS_FULL) || edge(b), "трава {b}");
            assert_eq!(is_dirt_tag(b), b == DIRT || bdef(b).base == GRASS, "#dirt {b}");
            assert_eq!(bdef(b).sinks, matches!(b, TALL_GRASS | FERN | DANDELION | POPPY), "утапливание {b}");
            assert_eq!(bdef(b).climbable, b == LADDER, "лазанье {b}");
            for f in 0..7 {
                assert_eq!(tinted(b, f), old_tinted(b, f), "оттенок {b} грань {f}");
            }
            for f in 0..6 {
                assert_eq!(layer(b, f), old_layer(b, f), "слой {b} грань {f}");
            }
        }
        assert_eq!((bdef(FURNACE).be, bdef(CHEST).be, bdef(STONE).be), (Some(BeKind::Furnace), Some(BeKind::Chest), None));
        assert_eq!((model_of(CHEST), model_of(LADDER), model_of(WALL_TORCH), model_of(SNOW_LAYER)), (Some(Model::Chest), Some(Model::Ladder), Some(Model::WallTorch), Some(Model::Snow)));
        assert_eq!((bdef(CRAFTING_TABLE).on_use, bdef(FURNACE).on_use, bdef(CHEST).on_use, bdef(LOG).on_use), (OnUse::Crafting, OnUse::Furnace, OnUse::Chest, OnUse::None));
        assert_eq!((bdef(WATER).fluid, bdef(LAVA).fluid, bdef(STONE).fluid), (FLUID_WATER, FLUID_LAVA, 0));
        assert!(bdef(LAVA).collision.is_empty() && bdef(LAVA).replaceable && !bdef(LAVA).suffocates);
        // Шерсть (G6): кубы своих слоёв, звук «шерсть», инструмент не нужен.
        for (k, b) in (WHITE_WOOL..=PINK_WOOL).enumerate() {
            assert!(opaque(b) && !bdef(b).requires_tool && bdef(b).sound == Snd::Wool && bdef(b).hardness == 0.8);
            assert_eq!(layer(b, 0), L_WOOL_WHITE + k as u32);
        }
        // Номер вне таблицы — как свободный номер.
        assert!(opaque(5000) && bdef(5000).collision.is_empty() && bdef(5000).replaceable);
        // Mekanism M1: руды — кубы с ярусом; машины и кабель — модели с блок-сущностью / формой по рукавам; лава.
        assert!(opaque(OSMIUM_ORE) && bdef(GOLD_ORE).needs == 2 && bdef(COPPER_ORE).needs == 1);
        assert_eq!((model_of(HEAT_GENERATOR), model_of(UNIVERSAL_CABLE)), (Some(Model::Mek), Some(Model::Cable)));
        assert_eq!(bdef(METALLURGIC_INFUSER).be, Some(BeKind::Mek(MekKind::Infuser)));
        assert_eq!(bdef(HEAT_GENERATOR).on_use, OnUse::Machine);
        assert!(bdef(LAVA).replaceable && bdef(LAVA).collision.is_empty() && opaque(LAVA) && bdef(LAVA).light == 15);
        // Кабель: центр + рукава по маске состояния.
        let c = cell_of(UNIVERSAL_CABLE, crate::world::grid::with_arms(0, crate::world::grid::arms_from_mask(0b110000)));
        assert_eq!(outline_of(c).len(), 3);
        assert_eq!(outline_of(UNIVERSAL_CABLE as Cell).len(), 1);
        // Mekanism M2: камеры — непрозрачные кубы с блок-сущностью и окном; лицо по FACING, у работающей — лицо «работа».
        assert!(opaque(ENRICHMENT_CHAMBER) && model_of(ENERGIZED_SMELTER).is_none() && bdef(ENRICHMENT_CHAMBER).suffocates);
        assert_eq!((bdef(ENRICHMENT_CHAMBER).be, bdef(ENERGIZED_SMELTER).be), (Some(BeKind::Mek(MekKind::Enrich)), Some(BeKind::Mek(MekKind::Smelter))));
        assert_eq!(bdef(ENERGIZED_SMELTER).on_use, OnUse::Machine);
        let f = |s: State, face: usize| layer_cell(ENRICHMENT_CHAMBER, s, face).0;
        assert_eq!((f(2, 5), f(2 | ST_LIT, 5), f(2, 4), f(2, 2), f(2, 3)), (L_ENRICH_FRONT, L_ENRICH_FRONT_ON, L_MEK_SIDE, L_MEK_TOP, L_MEK_BOTTOM));
        assert_eq!((light_of(cell_of(ENERGIZED_SMELTER, 2)), light_of(cell_of(ENERGIZED_SMELTER, 2 | ST_LIT))), (0, 8));
    }

    #[test]
    fn state_rules_from_table() {
        // Лицо печи по FACING и горение; ось бревна; формы лестницы и снега по состоянию.
        let f = |s: State, face: usize| layer_cell(FURNACE, s, face).0;
        assert_eq!((f(2, 5), f(3, 4), f(4, 1), f(5, 0)), (L_FURNACE_FRONT, L_FURNACE_FRONT, L_FURNACE_FRONT, L_FURNACE_FRONT));
        assert_eq!((f(2 | ST_LIT, 5), f(2, 4), f(2, 2), f(2, 3)), (L_FURNACE_FRONT_ON, L_FURNACE_SIDE, L_FURNACE_TOP, L_FURNACE_TOP));
        assert_eq!((layer_cell(LOG, 1, 0), layer_cell(LOG, 1, 4), layer_cell(LOG, 2, 0)), ((L_LOG_TOP, false), (L_LOG, true), (L_LOG, true)));
        assert_eq!(outline_of(cell_of(LADDER, 3)), LADDER_SHAPES[1]);
        assert_eq!((outline_of(cell_of(SNOW_LAYER, 3)), collision_of(cell_of(SNOW_LAYER, 3))), (SNOW_SHAPES[4], SNOW_SHAPES[3]));
        assert!(collision_of(cell_of(WALL_TORCH, 4)).is_empty() && outline_of(cell_of(WALL_TORCH, 4)) == WALL_TORCH_SHAPES[2]);
        assert!(sides_of(LEAVES as Cell).is_empty() && sides_of(CACTUS as Cell) == CACTUS_SIDES);
        assert!(replaceable_by(cell_of(SNOW_LAYER, 3), SNOW_LAYER as ItemId) && !replaceable_by(cell_of(SNOW_LAYER, 3), DIRT as ItemId));
        assert!(replaceable_by(cell_of(SNOW_LAYER, 0), DIRT as ItemId) && !replaceable_by(cell_of(SNOW_LAYER, 7), SNOW_LAYER as ItemId));
        // Светимость на будущее: факелы 14, печь 13 — только горящая.
        assert_eq!((light_of(TORCH as Cell), light_of(WALL_TORCH as Cell), light_of(cell_of(FURNACE, 2)), light_of(cell_of(FURNACE, 2 | ST_LIT))), (14, 14, 0, 13));
    }
}
