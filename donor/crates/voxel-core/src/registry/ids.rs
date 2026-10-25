//! Номера блоков и слои атласа (было blocks.rs и номера в registry.rs). Номера не переставляются, новые — только в
//! конец (mc/decisions.md): 0–33 — природа (варианты каймы травы 28–33), 34–39 — резерв, с 40 — блоки срезов 2–3.
//! Поведение блока — строка таблицы registry/blocks.rs.

use crate::cell::BlockId;

// Блоки.
pub(crate) const AIR: BlockId = 0;
pub(crate) const GRASS: BlockId = 1;
pub(crate) const DIRT: BlockId = 2;
pub(crate) const STONE: BlockId = 3;
pub(crate) const SAND: BlockId = 4;
pub(crate) const GRAVEL: BlockId = 5;
pub(crate) const SNOW: BlockId = 6;
pub(crate) const SANDSTONE: BlockId = 7;
pub(crate) const CLAY: BlockId = 8;
pub(crate) const LOG: BlockId = 9;
pub(crate) const LEAVES: BlockId = 10;
pub(crate) const BIRCH_LOG: BlockId = 11;
pub(crate) const BIRCH_LEAVES: BlockId = 12;
pub(crate) const SPRUCE_LOG: BlockId = 13;
pub(crate) const SPRUCE_LEAVES: BlockId = 14;
pub(crate) const WATER: BlockId = 15;
pub(crate) const CACTUS: BlockId = 16;
pub(crate) const TALL_GRASS: BlockId = 17;
pub(crate) const DANDELION: BlockId = 18;
pub(crate) const POPPY: BlockId = 19;
pub(crate) const DEAD_BUSH: BlockId = 20;
pub(crate) const COBBLE: BlockId = 21;
pub(crate) const BEDROCK: BlockId = 22;
pub(crate) const DRY_GRASS: BlockId = 23;
pub(crate) const DARK_GRASS: BlockId = 24;
pub(crate) const SNOWY_GRASS: BlockId = 25;
pub(crate) const FERN: BlockId = 26;
/// Трава ступеньки в 1 блок: бок — глубокая кайма (земля только узкой полосой в углу ступени).
pub(crate) const GRASS_FULL: BlockId = 27;
/// Трава с каймой бока, запечённой под оттенок биома: GRASS_EDGE + вариант (0…5, FRINGE_TINTS в gen).
pub(crate) const GRASS_EDGE: BlockId = 28;
pub(crate) const GRASS_EDGES: BlockId = 6;

// Блоки срезов 2–3 (порядок пород — как у брёвен: дуб, берёза, ель).
/// Дубовые доски.
pub(crate) const PLANKS: BlockId = 40;
pub(crate) const BIRCH_PLANKS: BlockId = 41;
pub(crate) const SPRUCE_PLANKS: BlockId = 42;
pub(crate) const CRAFTING_TABLE: BlockId = 43;
/// Состояние: FACING | LIT; блок-сущность.
pub(crate) const FURNACE: BlockId = 44;
pub(crate) const COAL_ORE: BlockId = 45;
pub(crate) const IRON_ORE: BlockId = 46;
/// Стоячий; настенный — WALL_TORCH.
pub(crate) const TORCH: BlockId = 47;
pub(crate) const GLASS: BlockId = 48;
pub(crate) const IRON_BLOCK: BlockId = 49;
pub(crate) const RAW_IRON_BLOCK: BlockId = 50;
pub(crate) const COAL_BLOCK: BlockId = 51;
/// Одинарный: FACING; блок-сущность на 27 ячеек.
pub(crate) const CHEST: BlockId = 52;
/// FACING — от стены; держится только на полной грани сзади.
pub(crate) const LADDER: BlockId = 53;
/// Слоёв 1…8 (биты 0–2 состояния: слоёв − 1).
pub(crate) const SNOW_LAYER: BlockId = 54;
/// Настенный факел (wall_torch): FACING — от стены; предмета своего нет, дроп — факел.
pub(crate) const WALL_TORCH: BlockId = 55;
/// Лава — пока только неподвижный источник (течение — ASP-15, урон — срез выживания).
pub(crate) const LAVA: BlockId = 56;

// Команда items (G7, docs/ids.md): блоки 110–119 — обсидиан, пашня, посевы, стог; предметы 420–449 (registry/items.rs).
/// Источник лавы под водой (FluidBlock.receiveNeighborFluids); текучая лава рядом с водой — булыжник.
pub(crate) const OBSIDIAN: BlockId = 110;
/// Пашня: биты 0–2 — влажность 0…7 (ASP-19). Предмета нет (дропает землю).
pub(crate) const FARMLAND: BlockId = 111;
/// Пшеница: биты 0–2 — возраст 0…7.
pub(crate) const WHEAT: BlockId = 112;
/// Морковь и картофель: возраст 0…7 (текстура — по стадиям 0–1, 2–3, 4–6, 7).
pub(crate) const CARROTS: BlockId = 113;
pub(crate) const POTATOES: BlockId = 114;
/// Стог сена: ось как у бревна (0 Y, 1 X, 2 Z); урон падения на него ×0.2.
pub(crate) const HAY_BLOCK: BlockId = 115;

// Mekanism M1 (mekanism/decisions.md): руды (осмий, олово — Mekanism; медь, золото, редстоун — ваниль), машины, кабель.
pub(crate) const OSMIUM_ORE: BlockId = 57;
pub(crate) const TIN_ORE: BlockId = 58;
pub(crate) const COPPER_ORE: BlockId = 59;
pub(crate) const GOLD_ORE: BlockId = 60;
/// Состояние: бит 3 — LIT (горит после касания, свет 9).
pub(crate) const REDSTONE_ORE: BlockId = 61;
pub(crate) const STEEL_CASING: BlockId = 62;
/// FACING | ACTIVE (бит 3); блок-сущность Mek.
pub(crate) const HEAT_GENERATOR: BlockId = 63;
pub(crate) const METALLURGIC_INFUSER: BlockId = 64;
/// Рукава: 12 бит состояния мимо бита 7 (world/grid.rs arms_of).
pub(crate) const UNIVERSAL_CABLE: BlockId = 65;
pub(crate) const ENERGY_CUBE: BlockId = 66;
pub(crate) const STEEL_BLOCK: BlockId = 67;
pub(crate) const OSMIUM_BLOCK: BlockId = 68;
pub(crate) const REDSTONE_BLOCK: BlockId = 69;
// Mekanism M2 «Обогащение» (G9): камеры — куб с лицом к игроку, «активна» — бит LIT (лицо работы), блок-сущность Mek.
pub(crate) const ENRICHMENT_CHAMBER: BlockId = 70;
pub(crate) const ENERGIZED_SMELTER: BlockId = 71;
// Mekanism M3 «Ветер и склад» (G11): ветряк — многоблок 1 × 5 (MEK-03 §3.5, E19): основание с блок-сущностью и 4
// ограничивающих блока над ним (состояние — высота над основанием 1…4: основание находится без блок-сущности).
pub(crate) const WIND_GENERATOR: BlockId = 72;
pub(crate) const BOUNDING: BlockId = 73;
// Mekanism M4 «Удвоение» (G12): дробилка и очистительная камера — кубы с лицом (как камеры M2), сепаратор — модель.
pub(crate) const CRUSHER: BlockId = 74;
pub(crate) const PURIFICATION_CHAMBER: BlockId = 75;
pub(crate) const ELECTROLYTIC_SEPARATOR: BlockId = 76;
// Mekanism M5 «Логистика» (G14): передатчики — рукава в состоянии, как у кабеля (world/grid.rs); насос — куб с блок-сущностью.
pub(crate) const LOGISTICAL_TRANSPORTER: BlockId = 77;
pub(crate) const MECHANICAL_PIPE: BlockId = 78;
pub(crate) const PRESSURIZED_TUBE: BlockId = 79;
/// FACING | ACTIVE (бит 3); блок-сущность Mek (MekKind::Pump).
pub(crate) const ELECTRIC_PUMP: BlockId = 80;
/// Шерсть 6 природных цветов (G6, ASP-17 §5.1): свой диапазон с 100 (соседи дописывают с 70; номера не пересекаются).
pub(crate) const WHITE_WOOL: BlockId = 100;
pub(crate) const BLACK_WOOL: BlockId = 101;
pub(crate) const GRAY_WOOL: BlockId = 102;
pub(crate) const LIGHT_GRAY_WOOL: BlockId = 103;
pub(crate) const BROWN_WOOL: BlockId = 104;
pub(crate) const PINK_WOOL: BlockId = 105;

// Слои атласа (tools/atlas.py — тот же порядок).
pub(crate) const L_GRASS_TOP: u32 = 0;
pub(crate) const L_GRASS_SIDE: u32 = 1;
pub(crate) const L_DIRT: u32 = 2;
pub(crate) const L_STONE: u32 = 3;
pub(crate) const L_SAND: u32 = 4;
pub(crate) const L_GRAVEL: u32 = 5;
pub(crate) const L_SNOW: u32 = 6;
pub(crate) const L_SANDSTONE: u32 = 7;
pub(crate) const L_SANDSTONE_TOP: u32 = 8;
pub(crate) const L_CLAY: u32 = 9;
pub(crate) const L_LOG: u32 = 10;
pub(crate) const L_LOG_TOP: u32 = 11;
pub(crate) const L_LEAVES: u32 = 12;
pub(crate) const L_BIRCH: u32 = 13;
pub(crate) const L_BIRCH_TOP: u32 = 14;
pub(crate) const L_BIRCH_LEAVES: u32 = 15;
pub(crate) const L_SPRUCE: u32 = 16;
pub(crate) const L_SPRUCE_TOP: u32 = 17;
pub(crate) const L_SPRUCE_LEAVES: u32 = 18;
pub(crate) const L_WATER: u32 = 19;
pub(crate) const L_CACTUS: u32 = 20;
pub(crate) const L_SHORT_GRASS: u32 = 21;
pub(crate) const L_DANDELION: u32 = 22;
pub(crate) const L_POPPY: u32 = 23;
pub(crate) const L_DEAD_BUSH: u32 = 24;
pub(crate) const L_COBBLE: u32 = 25;
pub(crate) const L_BEDROCK: u32 = 26;
pub(crate) const L_DRY_TOP: u32 = 27;
pub(crate) const L_DRY_SIDE: u32 = 28;
pub(crate) const L_DARK_TOP: u32 = 29;
pub(crate) const L_DARK_SIDE: u32 = 30;
pub(crate) const L_SNOWY_SIDE: u32 = 31;
pub(crate) const L_FERN: u32 = 32;
pub(crate) const L_GRASS_FULL: u32 = 33;
/// Запечённые варианты каймы 1…5 (в мире заменены слоем с маской, номера заняты).
#[allow(dead_code)]
pub(crate) const L_GRASS_EDGE1: u32 = 34;
/// Горная трава с запечённым цветом (дальние плитки).
pub(crate) const L_MTN_TOP: u32 = 39;
// Бок травы в мире — земля + накладка травы с маской оттенка в альфе (как grass_block_side_overlay Minecraft):
// трава красится оттенком столбца, земля — нет (шейдер блоков). Обычная кайма и глубокая — у ступени в 1 блок.
pub(crate) const L_GRASS_SIDE_T: u32 = 40;
pub(crate) const L_GRASS_STEP_T: u32 = 41;
// Блоки срезов 2–5 (tools/art_blocks.py, тот же порядок). Только дописывать.
// Посевы — слой стадии 0 + номер стадии (пшеница 0…7, морковь и картофель 0…3).
pub(crate) const L_OAK_PLANKS: u32 = 42;
pub(crate) const L_BIRCH_PLANKS: u32 = 43;
pub(crate) const L_SPRUCE_PLANKS: u32 = 44;
pub(crate) const L_TABLE_TOP: u32 = 45;
pub(crate) const L_TABLE_SIDE: u32 = 46;
pub(crate) const L_TABLE_FRONT: u32 = 47;
pub(crate) const L_FURNACE_FRONT: u32 = 48;
pub(crate) const L_FURNACE_FRONT_ON: u32 = 49;
pub(crate) const L_FURNACE_SIDE: u32 = 50;
pub(crate) const L_FURNACE_TOP: u32 = 51;
pub(crate) const L_COAL_ORE: u32 = 52;
pub(crate) const L_IRON_ORE: u32 = 53;
pub(crate) const L_GLASS: u32 = 54;
pub(crate) const L_TORCH: u32 = 55;
pub(crate) const L_CHEST_TOP: u32 = 56;
pub(crate) const L_CHEST_SIDE: u32 = 57;
pub(crate) const L_CHEST_FRONT: u32 = 58;
pub(crate) const L_LADDER: u32 = 59;
pub(crate) const L_FARMLAND_DRY: u32 = 60;
pub(crate) const L_FARMLAND_WET: u32 = 61;
pub(crate) const L_WHEAT: u32 = 62;
pub(crate) const L_CARROTS: u32 = 70;
pub(crate) const L_POTATOES: u32 = 74;
pub(crate) const L_OAK_SAPLING: u32 = 78;
pub(crate) const L_BIRCH_SAPLING: u32 = 79;
pub(crate) const L_SPRUCE_SAPLING: u32 = 80;
pub(crate) const L_WOOL_WHITE: u32 = 81;
pub(crate) const L_WOOL_BLACK: u32 = 82;
pub(crate) const L_WOOL_GRAY: u32 = 83;
pub(crate) const L_WOOL_LIGHT_GRAY: u32 = 84;
pub(crate) const L_WOOL_BROWN: u32 = 85;
pub(crate) const L_WOOL_PINK: u32 = 86;
pub(crate) const L_HAY_TOP: u32 = 87;
pub(crate) const L_HAY_SIDE: u32 = 88;
// Mekanism (art/mekanism/manifest.json: слои дописаны подряд с 89 — tools/mek_embed.py; кадры анимации — подряд).
pub(crate) const L_OSMIUM_ORE: u32 = 89;
pub(crate) const L_TIN_ORE: u32 = 90;
pub(crate) const L_COPPER_ORE: u32 = 91;
pub(crate) const L_GOLD_ORE: u32 = 92;
pub(crate) const L_REDSTONE_ORE: u32 = 93;
pub(crate) const L_STEEL_CASING: u32 = 94;
pub(crate) const L_MEK_SIDE: u32 = 95;
pub(crate) const L_MEK_TOP: u32 = 96;
pub(crate) const L_MEK_BOTTOM: u32 = 97;
pub(crate) const L_MEK_BACK: u32 = 98;
pub(crate) const L_ENRICH_FRONT: u32 = 99;
/// Лицо работающей камеры — первый из 6 кадров (анимация лиц — со срезом вида).
pub(crate) const L_ENRICH_FRONT_ON: u32 = 100;
pub(crate) const L_CRUSHER_FRONT: u32 = 106;
pub(crate) const L_CRUSHER_FRONT_ON: u32 = 107;
pub(crate) const L_SMELTER_FRONT: u32 = 117;
pub(crate) const L_SMELTER_FRONT_ON: u32 = 118;
pub(crate) const L_PURIFIER_FRONT: u32 = 125;
pub(crate) const L_PURIFIER_FRONT_ON: u32 = 126;
pub(crate) const L_INFUSER_BODY: u32 = 128;
pub(crate) const L_SEPARATOR_BODY: u32 = 130;
pub(crate) const L_HEATGEN_BODY: u32 = 133;
pub(crate) const L_HEATGEN_DRUM: u32 = 134;
pub(crate) const L_CUBE_FRAME: u32 = 139;
pub(crate) const L_CABLE_SIDE: u32 = 151;
pub(crate) const L_PUMP_SIDE: u32 = 145;
pub(crate) const L_PUMP_TOP: u32 = 146;
pub(crate) const L_PUMP_BOTTOM: u32 = 147;
pub(crate) const L_TUBE_SIDE: u32 = 161;
pub(crate) const L_PIPE_SIDE: u32 = 164;
pub(crate) const L_TRANSPORTER_SIDE: u32 = 166;
pub(crate) const L_OSMIUM_BLOCK: u32 = 173;
pub(crate) const L_STEEL_BLOCK: u32 = 175;
pub(crate) const L_IRON_BLOCK: u32 = 178;
pub(crate) const L_REDSTONE_BLOCK: u32 = 179;
pub(crate) const L_RAW_IRON_BLOCK: u32 = 184;
/// Лава (свой рисунок, tools/mek_embed.py) — после слоёв Mekanism.
pub(crate) const L_LAVA: u32 = 185;
/// Слоёв в атласе (web/atlas.png): проверка tools/mek_embed.py и тест atlas_layers.
pub(crate) const ATLAS_LAYERS: u32 = 186;

// Пока блоков нет — константы помечены используемыми (иначе предупреждения dead_code).
const _: [u32; 23] = [
    L_FARMLAND_DRY, L_FARMLAND_WET, L_WHEAT, L_CARROTS, L_POTATOES, L_OAK_SAPLING, L_BIRCH_SAPLING, L_SPRUCE_SAPLING,
    L_WOOL_WHITE, L_WOOL_BLACK, L_WOOL_GRAY, L_WOOL_LIGHT_GRAY, L_WOOL_BROWN, L_WOOL_PINK, L_HAY_TOP, L_HAY_SIDE,
    L_COAL_ORE, L_IRON_ORE, L_GLASS, L_TORCH, L_CHEST_TOP, L_CHEST_SIDE, L_CHEST_FRONT,
];
