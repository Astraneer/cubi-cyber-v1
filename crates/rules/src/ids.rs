//! Stable registry data and pure formulas from the SPEC-approved donor.
//! Source: donor/crates/voxel-core/src/registry/ids.rs at 3992583213e1f00d95a3beef2bde43ae970192d4.
//! Adapted to the shared rules layer; no donor world or runtime is included.

use crate::cell::BlockId;

pub const AIR: BlockId = 0;
pub const GRASS: BlockId = 1;
pub const DIRT: BlockId = 2;
pub const STONE: BlockId = 3;
pub const SAND: BlockId = 4;
pub const GRAVEL: BlockId = 5;
pub const SNOW: BlockId = 6;
pub const SANDSTONE: BlockId = 7;
pub const CLAY: BlockId = 8;
pub const LOG: BlockId = 9;
pub const LEAVES: BlockId = 10;
pub const BIRCH_LOG: BlockId = 11;
pub const BIRCH_LEAVES: BlockId = 12;
pub const SPRUCE_LOG: BlockId = 13;
pub const SPRUCE_LEAVES: BlockId = 14;
pub const WATER: BlockId = 15;
pub const CACTUS: BlockId = 16;
pub const TALL_GRASS: BlockId = 17;
pub const DANDELION: BlockId = 18;
pub const POPPY: BlockId = 19;
pub const DEAD_BUSH: BlockId = 20;
pub const COBBLE: BlockId = 21;
pub const BEDROCK: BlockId = 22;
pub const DRY_GRASS: BlockId = 23;
pub const DARK_GRASS: BlockId = 24;
pub const SNOWY_GRASS: BlockId = 25;
pub const FERN: BlockId = 26;
pub const GRASS_FULL: BlockId = 27;
pub const GRASS_EDGE: BlockId = 28;
pub const GRASS_EDGES: BlockId = 6;

pub const PLANKS: BlockId = 40;
pub const BIRCH_PLANKS: BlockId = 41;
pub const SPRUCE_PLANKS: BlockId = 42;
pub const CRAFTING_TABLE: BlockId = 43;
pub const FURNACE: BlockId = 44;
pub const COAL_ORE: BlockId = 45;
pub const IRON_ORE: BlockId = 46;
pub const TORCH: BlockId = 47;
pub const GLASS: BlockId = 48;
pub const IRON_BLOCK: BlockId = 49;
pub const RAW_IRON_BLOCK: BlockId = 50;
pub const COAL_BLOCK: BlockId = 51;
pub const CHEST: BlockId = 52;
pub const LADDER: BlockId = 53;
pub const SNOW_LAYER: BlockId = 54;
pub const WALL_TORCH: BlockId = 55;
pub const LAVA: BlockId = 56;

pub const OBSIDIAN: BlockId = 110;
pub const FARMLAND: BlockId = 111;
pub const WHEAT: BlockId = 112;
pub const CARROTS: BlockId = 113;
pub const POTATOES: BlockId = 114;
pub const HAY_BLOCK: BlockId = 115;

pub const OSMIUM_ORE: BlockId = 57;
pub const TIN_ORE: BlockId = 58;
pub const COPPER_ORE: BlockId = 59;
pub const GOLD_ORE: BlockId = 60;
pub const REDSTONE_ORE: BlockId = 61;
pub const STEEL_CASING: BlockId = 62;
pub const HEAT_GENERATOR: BlockId = 63;
pub const METALLURGIC_INFUSER: BlockId = 64;
pub const UNIVERSAL_CABLE: BlockId = 65;
pub const ENERGY_CUBE: BlockId = 66;
pub const STEEL_BLOCK: BlockId = 67;
pub const OSMIUM_BLOCK: BlockId = 68;
pub const REDSTONE_BLOCK: BlockId = 69;
pub const ENRICHMENT_CHAMBER: BlockId = 70;
pub const ENERGIZED_SMELTER: BlockId = 71;
pub const WIND_GENERATOR: BlockId = 72;
pub const BOUNDING: BlockId = 73;
pub const CRUSHER: BlockId = 74;
pub const PURIFICATION_CHAMBER: BlockId = 75;
pub const ELECTROLYTIC_SEPARATOR: BlockId = 76;
pub const LOGISTICAL_TRANSPORTER: BlockId = 77;
pub const MECHANICAL_PIPE: BlockId = 78;
pub const PRESSURIZED_TUBE: BlockId = 79;
pub const ELECTRIC_PUMP: BlockId = 80;
pub const WHITE_WOOL: BlockId = 100;
pub const BLACK_WOOL: BlockId = 101;
pub const GRAY_WOOL: BlockId = 102;
pub const LIGHT_GRAY_WOOL: BlockId = 103;
pub const BROWN_WOOL: BlockId = 104;
pub const PINK_WOOL: BlockId = 105;

pub const L_GRASS_TOP: u32 = 0;
pub const L_GRASS_SIDE: u32 = 1;
pub const L_DIRT: u32 = 2;
pub const L_STONE: u32 = 3;
pub const L_SAND: u32 = 4;
pub const L_GRAVEL: u32 = 5;
pub const L_SNOW: u32 = 6;
pub const L_SANDSTONE: u32 = 7;
pub const L_SANDSTONE_TOP: u32 = 8;
pub const L_CLAY: u32 = 9;
pub const L_LOG: u32 = 10;
pub const L_LOG_TOP: u32 = 11;
pub const L_LEAVES: u32 = 12;
pub const L_BIRCH: u32 = 13;
pub const L_BIRCH_TOP: u32 = 14;
pub const L_BIRCH_LEAVES: u32 = 15;
pub const L_SPRUCE: u32 = 16;
pub const L_SPRUCE_TOP: u32 = 17;
pub const L_SPRUCE_LEAVES: u32 = 18;
pub const L_WATER: u32 = 19;
pub const L_CACTUS: u32 = 20;
pub const L_SHORT_GRASS: u32 = 21;
pub const L_DANDELION: u32 = 22;
pub const L_POPPY: u32 = 23;
pub const L_DEAD_BUSH: u32 = 24;
pub const L_COBBLE: u32 = 25;
pub const L_BEDROCK: u32 = 26;
pub const L_DRY_TOP: u32 = 27;
pub const L_DRY_SIDE: u32 = 28;
pub const L_DARK_TOP: u32 = 29;
pub const L_DARK_SIDE: u32 = 30;
pub const L_SNOWY_SIDE: u32 = 31;
pub const L_FERN: u32 = 32;
pub const L_GRASS_FULL: u32 = 33;
#[allow(dead_code)]
pub const L_GRASS_EDGE1: u32 = 34;
pub const L_MTN_TOP: u32 = 39;
pub const L_GRASS_SIDE_T: u32 = 40;
pub const L_GRASS_STEP_T: u32 = 41;
pub const L_OAK_PLANKS: u32 = 42;
pub const L_BIRCH_PLANKS: u32 = 43;
pub const L_SPRUCE_PLANKS: u32 = 44;
pub const L_TABLE_TOP: u32 = 45;
pub const L_TABLE_SIDE: u32 = 46;
pub const L_TABLE_FRONT: u32 = 47;
pub const L_FURNACE_FRONT: u32 = 48;
pub const L_FURNACE_FRONT_ON: u32 = 49;
pub const L_FURNACE_SIDE: u32 = 50;
pub const L_FURNACE_TOP: u32 = 51;
pub const L_COAL_ORE: u32 = 52;
pub const L_IRON_ORE: u32 = 53;
pub const L_GLASS: u32 = 54;
pub const L_TORCH: u32 = 55;
pub const L_CHEST_TOP: u32 = 56;
pub const L_CHEST_SIDE: u32 = 57;
pub const L_CHEST_FRONT: u32 = 58;
pub const L_LADDER: u32 = 59;
pub const L_FARMLAND_DRY: u32 = 60;
pub const L_FARMLAND_WET: u32 = 61;
pub const L_WHEAT: u32 = 62;
pub const L_CARROTS: u32 = 70;
pub const L_POTATOES: u32 = 74;
pub const L_OAK_SAPLING: u32 = 78;
pub const L_BIRCH_SAPLING: u32 = 79;
pub const L_SPRUCE_SAPLING: u32 = 80;
pub const L_WOOL_WHITE: u32 = 81;
pub const L_WOOL_BLACK: u32 = 82;
pub const L_WOOL_GRAY: u32 = 83;
pub const L_WOOL_LIGHT_GRAY: u32 = 84;
pub const L_WOOL_BROWN: u32 = 85;
pub const L_WOOL_PINK: u32 = 86;
pub const L_HAY_TOP: u32 = 87;
pub const L_HAY_SIDE: u32 = 88;
pub const L_OSMIUM_ORE: u32 = 89;
pub const L_TIN_ORE: u32 = 90;
pub const L_COPPER_ORE: u32 = 91;
pub const L_GOLD_ORE: u32 = 92;
pub const L_REDSTONE_ORE: u32 = 93;
pub const L_STEEL_CASING: u32 = 94;
pub const L_MEK_SIDE: u32 = 95;
pub const L_MEK_TOP: u32 = 96;
pub const L_MEK_BOTTOM: u32 = 97;
pub const L_MEK_BACK: u32 = 98;
pub const L_ENRICH_FRONT: u32 = 99;
pub const L_ENRICH_FRONT_ON: u32 = 100;
pub const L_CRUSHER_FRONT: u32 = 106;
pub const L_CRUSHER_FRONT_ON: u32 = 107;
pub const L_SMELTER_FRONT: u32 = 117;
pub const L_SMELTER_FRONT_ON: u32 = 118;
pub const L_PURIFIER_FRONT: u32 = 125;
pub const L_PURIFIER_FRONT_ON: u32 = 126;
pub const L_INFUSER_BODY: u32 = 128;
pub const L_SEPARATOR_BODY: u32 = 130;
pub const L_HEATGEN_BODY: u32 = 133;
pub const L_HEATGEN_DRUM: u32 = 134;
pub const L_CUBE_FRAME: u32 = 139;
pub const L_CABLE_SIDE: u32 = 151;
pub const L_PUMP_SIDE: u32 = 145;
pub const L_PUMP_TOP: u32 = 146;
pub const L_PUMP_BOTTOM: u32 = 147;
pub const L_TUBE_SIDE: u32 = 161;
pub const L_PIPE_SIDE: u32 = 164;
pub const L_TRANSPORTER_SIDE: u32 = 166;
pub const L_OSMIUM_BLOCK: u32 = 173;
pub const L_STEEL_BLOCK: u32 = 175;
pub const L_IRON_BLOCK: u32 = 178;
pub const L_REDSTONE_BLOCK: u32 = 179;
pub const L_RAW_IRON_BLOCK: u32 = 184;
pub const L_LAVA: u32 = 185;
pub const ATLAS_LAYERS: u32 = 186;

const _: [u32; 23] = [
    L_FARMLAND_DRY,
    L_FARMLAND_WET,
    L_WHEAT,
    L_CARROTS,
    L_POTATOES,
    L_OAK_SAPLING,
    L_BIRCH_SAPLING,
    L_SPRUCE_SAPLING,
    L_WOOL_WHITE,
    L_WOOL_BLACK,
    L_WOOL_GRAY,
    L_WOOL_LIGHT_GRAY,
    L_WOOL_BROWN,
    L_WOOL_PINK,
    L_HAY_TOP,
    L_HAY_SIDE,
    L_COAL_ORE,
    L_IRON_ORE,
    L_GLASS,
    L_TORCH,
    L_CHEST_TOP,
    L_CHEST_SIDE,
    L_CHEST_FRONT,
];
