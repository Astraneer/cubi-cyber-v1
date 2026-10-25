//! Stable registry data and pure formulas from the SPEC-approved donor.
//! Source: donor/crates/voxel-core/src/registry/blocks.rs at 3992583213e1f00d95a3beef2bde43ae970192d4.
//! Adapted to the shared rules layer; no donor world or runtime is included.

use crate::Model;
use crate::cell::{BlockId, Cell, DIRS, State, block_of, cell_of, state_of};
use crate::ids::*;
use crate::items::{
    I_APPLE, I_CLAY_BALL, I_COAL, I_FLINT, I_RAW_COPPER, I_RAW_GOLD, I_RAW_IRON, I_RAW_OSMIUM,
    I_RAW_TIN, I_REDSTONE, I_SNOWBALL, I_STICK, I_WHEAT_SEEDS, ItemId, block_item,
};
use crate::shapes::*;

pub const ST_LIT: State = 8;
pub const ST_DECAY: State = 1;
pub const ST_PERSIST: State = 2;

pub const BLOCK_COUNT: usize = 256;
pub const FLUID_WATER: u8 = 1;
pub const FLUID_LAVA: u8 = 2;

#[inline]
pub fn facing_of(c: Cell) -> u8 {
    let f = (state_of(c) & 7) as u8;
    if (2..=5).contains(&f) { f } else { 2 }
}
#[inline]
pub fn hidx(f: u8) -> usize {
    (f.clamp(2, 5) - 2) as usize
}
#[inline]
pub fn snow_layers(c: Cell) -> u8 {
    (state_of(c) & 7) as u8 + 1
}
pub fn mesh_face(f: u8) -> usize {
    [3, 2, 5, 4, 1, 0][f.min(5) as usize]
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Snd {
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Offset {
    None,
    Xz,
    Xyz,
}

pub const PICKAXE: u8 = 1;
pub const AXE: u8 = 2;
pub const SHOVEL: u8 = 4;
pub const HOE: u8 = 8;

#[derive(Clone, Copy, Debug)]
pub struct Drop {
    pub item: ItemId,
    pub min: u8,
    pub max: u8,
    pub chance: f32,
    pub alt: u8,
}

#[derive(Clone, Copy, Debug)]
pub enum Loot {
    None,
    Itself,
    Table(&'static [Drop]),
    SnowLayers,
    Crop,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Render {
    None,
    Cube,
    Cross,
    Leaves,
    Glass,
    Water,
    Model(Model),
}

impl Render {
    pub const fn opaque(self) -> bool {
        matches!(self, Render::Cube | Render::Leaves)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TexRule {
    Fixed,
    Axis,
    Facing { lit: u32 },
    Crop { base: u32, wheat: bool },
    Farmland,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shape {
    Fixed,
    Ladder,
    WallTorch,
    Layers,
    Cable,
    Large,
    Wheat,
    Root,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Sides {
    Collision,
    Outline,
    Empty,
    Box(&'static [Aabb]),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Support {
    None,
    Plant,
    DeadBush,
    Floor,
    Wall,
    Layers,
    Cactus,
    Crop,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Place {
    Plain,
    Axis,
    FacePlayer,
    Torch { wall: BlockId },
    Ladder,
    Layers,
    Persistent,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OnUse {
    None,
    Crafting,
    Furnace,
    Chest,
    Machine,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MekKind {
    #[default]
    Heat,
    Infuser,
    Cube,
    Enrich,
    Smelter,
    Wind,
    Crusher,
    Purifier,
    Separator,
    Pump,
}

#[inline]
pub fn bounding_dy(c: Cell) -> i32 {
    (state_of(c) & 7) as i32
}
pub fn main_of(c: Cell, p: [i32; 3]) -> [i32; 3] {
    if block_of(c) == BOUNDING {
        [p[0], p[1] - bounding_dy(c).max(1), p[2]]
    } else {
        p
    }
}
pub fn multiblock_parts(b: BlockId) -> std::ops::RangeInclusive<i32> {
    if b == WIND_GENERATOR {
        1..=4
    } else {
        std::ops::RangeInclusive::new(1, 0)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BeKind {
    Furnace,
    Chest,
    Mek(MekKind),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RandomTick {
    None,
    LeafDecay,
    Farmland,
    Crop,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Display {
    None,
    Furnace,
}

#[derive(Clone, Copy, Debug)]
pub struct BlockDef {
    pub mc: &'static str,
    pub base: BlockId,
    pub hardness: f32,
    pub requires_tool: bool,
    pub tool: u8,
    pub needs: u8,
    pub sword_fast: bool,
    pub sound: Snd,
    pub replaceable: bool,
    pub outline: &'static [Aabb],
    pub collision: &'static [Aabb],
    pub offset: Offset,
    pub outline_offset: bool,
    pub suffocates: bool,
    pub slip: f32,
    pub loot: Loot,
    pub render: Render,
    pub tex: [u32; 6],
    pub tex_rule: TexRule,
    pub tint: u8,
    pub shape: Shape,
    pub sides: Sides,
    pub support: Support,
    pub place: Place,
    pub on_use: OnUse,
    pub be: Option<BeKind>,
    pub falling: bool,
    pub random: RandomTick,
    pub display: Display,
    pub log: bool,
    pub leaves: bool,
    pub grass: bool,
    pub dirt: bool,
    pub farmland: bool,
    pub climbable: bool,
    pub never_solid: bool,
    pub sinks: bool,
    pub flat_item: bool,
    pub light: u8,
    pub lit_only: bool,
    pub fluid: u8,
}

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

const UNKNOWN: BlockDef = {
    let mut d = EMPTY_BLOCK;
    d.mc = "unknown";
    d.hardness = -1.0;
    d.replaceable = false;
    d.render = Render::Cube;
    d.outline = CUBE;
    d.collision = CUBE;
    d.suffocates = true;
    d
};

#[allow(clippy::too_many_arguments)]
const fn solid(
    mc: &'static str,
    base: BlockId,
    hardness: f32,
    req: bool,
    tool: u8,
    sound: Snd,
    loot: Loot,
    tex: [u32; 6],
) -> BlockDef {
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
const fn plant(
    mc: &'static str,
    base: BlockId,
    tool: u8,
    replaceable: bool,
    outline: &'static [Aabb],
    offset: Offset,
    loot: Loot,
    on: Support,
    l: u32,
) -> BlockDef {
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

const fn all(l: u32) -> [u32; 6] {
    [l; 6]
}
const fn cap(top: u32, bottom: u32, side: u32) -> [u32; 6] {
    [side, side, top, bottom, side, side]
}
const fn faced(top: u32, side: u32, front: u32) -> [u32; 6] {
    [side, side, top, top, side, front]
}
const TINT_ALL: u8 = 0x7F;
const TINT_NOT_BOTTOM: u8 = 0x7F & !(1 << 3);

const DROP_DIRT: &[Drop] = &[Drop {
    item: DIRT as ItemId,
    min: 1,
    max: 1,
    chance: 1.0,
    alt: 0,
}];
const DROP_COBBLE: &[Drop] = &[Drop {
    item: COBBLE as ItemId,
    min: 1,
    max: 1,
    chance: 1.0,
    alt: 0,
}];
const DROP_GRAVEL: &[Drop] = &[
    Drop {
        item: I_FLINT,
        min: 1,
        max: 1,
        chance: 0.1,
        alt: 1,
    },
    Drop {
        item: GRAVEL as ItemId,
        min: 1,
        max: 1,
        chance: 1.0,
        alt: 1,
    },
];
const DROP_OAK_LEAVES: &[Drop] = &[
    Drop {
        item: I_STICK,
        min: 1,
        max: 2,
        chance: 0.02,
        alt: 0,
    },
    Drop {
        item: I_APPLE,
        min: 1,
        max: 1,
        chance: 0.005,
        alt: 0,
    },
];
const DROP_LEAVES: &[Drop] = &[Drop {
    item: I_STICK,
    min: 1,
    max: 2,
    chance: 0.02,
    alt: 0,
}];
const DROP_SNOW_BLOCK: &[Drop] = &[Drop {
    item: I_SNOWBALL,
    min: 4,
    max: 4,
    chance: 1.0,
    alt: 0,
}];
const DROP_CLAY: &[Drop] = &[Drop {
    item: I_CLAY_BALL,
    min: 4,
    max: 4,
    chance: 1.0,
    alt: 0,
}];
const DROP_SHORT_GRASS: &[Drop] = &[Drop {
    item: I_WHEAT_SEEDS,
    min: 1,
    max: 1,
    chance: 0.125,
    alt: 0,
}];
const DROP_DEAD_BUSH: &[Drop] = &[Drop {
    item: I_STICK,
    min: 0,
    max: 2,
    chance: 1.0,
    alt: 0,
}];
const DROP_COAL: &[Drop] = &[Drop {
    item: I_COAL,
    min: 1,
    max: 1,
    chance: 1.0,
    alt: 0,
}];
const DROP_RAW_IRON: &[Drop] = &[Drop {
    item: I_RAW_IRON,
    min: 1,
    max: 1,
    chance: 1.0,
    alt: 0,
}];
const DROP_RAW_OSMIUM: &[Drop] = &[Drop {
    item: I_RAW_OSMIUM,
    min: 1,
    max: 1,
    chance: 1.0,
    alt: 0,
}];
const DROP_RAW_TIN: &[Drop] = &[Drop {
    item: I_RAW_TIN,
    min: 1,
    max: 1,
    chance: 1.0,
    alt: 0,
}];
const DROP_RAW_COPPER: &[Drop] = &[Drop {
    item: I_RAW_COPPER,
    min: 2,
    max: 5,
    chance: 1.0,
    alt: 0,
}];
const DROP_RAW_GOLD: &[Drop] = &[Drop {
    item: I_RAW_GOLD,
    min: 1,
    max: 1,
    chance: 1.0,
    alt: 0,
}];
const DROP_REDSTONE: &[Drop] = &[Drop {
    item: I_REDSTONE,
    min: 4,
    max: 5,
    chance: 1.0,
    alt: 0,
}];

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
const fn log(mc: &'static str, b: BlockId, top: u32, side: u32) -> BlockDef {
    let mut d = solid(
        mc,
        b,
        2.0,
        false,
        AXE,
        Snd::Wood,
        Loot::Itself,
        cap(top, top, side),
    );
    d.tex_rule = TexRule::Axis;
    d.place = Place::Axis;
    d.log = true;
    d
}

const fn crop(mc: &'static str, b: BlockId, wheat: bool, base: u32) -> BlockDef {
    let mut d = plant(
        mc,
        b,
        0,
        false,
        &[],
        Offset::None,
        Loot::Crop,
        Support::Crop,
        base,
    );
    d.sword_fast = false;
    d.shape = if wheat { Shape::Wheat } else { Shape::Root };
    d.tex_rule = TexRule::Crop { base, wheat };
    d.random = RandomTick::Crop;
    d.flat_item = false;
    d
}

const fn ore(mc: &'static str, b: BlockId, needs: u8, loot: &'static [Drop], l: u32) -> BlockDef {
    let mut d = solid(
        mc,
        b,
        3.0,
        true,
        PICKAXE,
        Snd::Stone,
        Loot::Table(loot),
        all(l),
    );
    d.needs = needs;
    d
}
const fn machine(
    mc: &'static str,
    b: BlockId,
    k: MekKind,
    hardness: f32,
    tex: [u32; 6],
    light: u8,
) -> BlockDef {
    let mut d = solid(
        mc,
        b,
        hardness,
        true,
        PICKAXE,
        Snd::Metal,
        Loot::Itself,
        tex,
    );
    d.render = Render::Model(Model::Mek);
    d.suffocates = false;
    d.place = Place::FacePlayer;
    d.on_use = OnUse::Machine;
    d.be = Some(BeKind::Mek(k));
    d.light = light;
    d.lit_only = true;
    d
}
const fn chamber(mc: &'static str, b: BlockId, k: MekKind, front: u32, front_on: u32) -> BlockDef {
    let mut d = machine(
        mc,
        b,
        k,
        3.5,
        [
            L_MEK_SIDE,
            L_MEK_SIDE,
            L_MEK_TOP,
            L_MEK_BOTTOM,
            L_MEK_SIDE,
            front,
        ],
        8,
    );
    d.render = Render::Cube;
    d.suffocates = true;
    d.tex_rule = TexRule::Facing { lit: front_on };
    d
}

pub const fn block_def(b: BlockId) -> BlockDef {
    use Snd::*;
    let grass = {
        let mut d = solid(
            "grass_block",
            GRASS,
            0.6,
            false,
            SHOVEL,
            Grass,
            Loot::Table(DROP_DIRT),
            cap(L_GRASS_TOP, L_DIRT, L_GRASS_SIDE),
        );
        d.dirt = true;
        d.grass = true;
        d
    };
    match b {
        AIR => EMPTY_BLOCK,
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
        GRASS_FULL => {
            let mut d = grass;
            d.tex = cap(L_GRASS_TOP, L_DIRT, L_GRASS_STEP_T);
            d.tint = TINT_NOT_BOTTOM;
            d
        }
        b if b >= GRASS_EDGE && b < GRASS_EDGE + GRASS_EDGES => {
            let mut d = grass;
            d.tex = cap(L_GRASS_TOP, L_DIRT, L_GRASS_SIDE_T);
            d.tint = TINT_NOT_BOTTOM;
            d
        }
        DIRT => {
            let mut d = solid(
                "dirt",
                DIRT,
                0.5,
                false,
                SHOVEL,
                Gravel,
                Loot::Itself,
                all(L_DIRT),
            );
            d.dirt = true;
            d
        }
        STONE => solid(
            "stone",
            STONE,
            1.5,
            true,
            PICKAXE,
            Stone,
            Loot::Table(DROP_COBBLE),
            all(L_STONE),
        ),
        SAND => {
            let mut d = solid(
                "sand",
                SAND,
                0.5,
                false,
                SHOVEL,
                Sand,
                Loot::Itself,
                all(L_SAND),
            );
            d.falling = true;
            d
        }
        GRAVEL => {
            let mut d = solid(
                "gravel",
                GRAVEL,
                0.6,
                false,
                SHOVEL,
                Gravel,
                Loot::Table(DROP_GRAVEL),
                all(L_GRAVEL),
            );
            d.falling = true;
            d
        }
        SNOW => solid(
            "snow_block",
            SNOW,
            0.2,
            true,
            SHOVEL,
            Snow,
            Loot::Table(DROP_SNOW_BLOCK),
            all(L_SNOW),
        ),
        SANDSTONE => solid(
            "sandstone",
            SANDSTONE,
            0.8,
            true,
            PICKAXE,
            Stone,
            Loot::Itself,
            cap(L_SANDSTONE_TOP, L_SANDSTONE_TOP, L_SANDSTONE),
        ),
        CLAY => solid(
            "clay",
            CLAY,
            0.6,
            false,
            SHOVEL,
            Gravel,
            Loot::Table(DROP_CLAY),
            all(L_CLAY),
        ),
        LOG => log("oak_log", LOG, L_LOG_TOP, L_LOG),
        BIRCH_LOG => log("birch_log", BIRCH_LOG, L_BIRCH_TOP, L_BIRCH),
        SPRUCE_LOG => log("spruce_log", SPRUCE_LOG, L_SPRUCE_TOP, L_SPRUCE),
        LEAVES => leaves("oak_leaves", LEAVES, Loot::Table(DROP_OAK_LEAVES), L_LEAVES),
        BIRCH_LEAVES => leaves(
            "birch_leaves",
            BIRCH_LEAVES,
            Loot::Table(DROP_LEAVES),
            L_BIRCH_LEAVES,
        ),
        SPRUCE_LEAVES => leaves(
            "spruce_leaves",
            SPRUCE_LEAVES,
            Loot::Table(DROP_LEAVES),
            L_SPRUCE_LEAVES,
        ),
        COBBLE => solid(
            "cobblestone",
            COBBLE,
            2.0,
            true,
            PICKAXE,
            Stone,
            Loot::Itself,
            all(L_COBBLE),
        ),
        BEDROCK => solid(
            "bedrock",
            BEDROCK,
            -1.0,
            false,
            0,
            Stone,
            Loot::None,
            all(L_BEDROCK),
        ),
        CACTUS => {
            let mut d = solid(
                "cactus",
                CACTUS,
                0.4,
                false,
                0,
                Wool,
                Loot::Itself,
                all(L_CACTUS),
            );
            d.suffocates = false;
            d.collision = CACTUS_SIDES;
            d.outline = CACTUS_OUTLINE;
            d.sides = Sides::Box(CACTUS_SIDES);
            d.support = Support::Cactus;
            d
        }
        TALL_GRASS => {
            let mut d = plant(
                "short_grass",
                TALL_GRASS,
                AXE | HOE,
                true,
                SHORT_PLANT,
                Offset::Xyz,
                Loot::Table(DROP_SHORT_GRASS),
                Support::Plant,
                L_SHORT_GRASS,
            );
            d.tint = TINT_ALL;
            d.sinks = true;
            d
        }
        FERN => {
            let mut d = plant(
                "fern",
                FERN,
                AXE | HOE,
                true,
                SHORT_PLANT,
                Offset::Xyz,
                Loot::Table(DROP_SHORT_GRASS),
                Support::Plant,
                L_FERN,
            );
            d.tint = TINT_ALL;
            d.sinks = true;
            d
        }
        DEAD_BUSH => plant(
            "dead_bush",
            DEAD_BUSH,
            AXE | HOE,
            true,
            SHORT_PLANT,
            Offset::None,
            Loot::Table(DROP_DEAD_BUSH),
            Support::DeadBush,
            L_DEAD_BUSH,
        ),
        DANDELION => {
            let mut d = plant(
                "dandelion",
                DANDELION,
                0,
                false,
                FLOWER,
                Offset::Xz,
                Loot::Itself,
                Support::Plant,
                L_DANDELION,
            );
            d.sinks = true;
            d
        }
        POPPY => {
            let mut d = plant(
                "poppy",
                POPPY,
                0,
                false,
                FLOWER,
                Offset::Xz,
                Loot::Itself,
                Support::Plant,
                L_POPPY,
            );
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
        OBSIDIAN => {
            let mut d = solid(
                "obsidian",
                OBSIDIAN,
                50.0,
                true,
                PICKAXE,
                Stone,
                Loot::Itself,
                all(L_BEDROCK),
            );
            d.needs = 3;
            d
        }
        FARMLAND => {
            let mut d = solid(
                "farmland",
                FARMLAND,
                0.6,
                false,
                SHOVEL,
                Gravel,
                Loot::Table(DROP_DIRT),
                all(L_DIRT),
            );
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
            let mut d = solid(
                "hay_block",
                HAY_BLOCK,
                0.5,
                false,
                HOE,
                Grass,
                Loot::Itself,
                cap(L_HAY_TOP, L_HAY_TOP, L_HAY_SIDE),
            );
            d.tex_rule = TexRule::Axis;
            d.place = Place::Axis;
            d
        }
        PLANKS => solid(
            "oak_planks",
            PLANKS,
            2.0,
            false,
            AXE,
            Wood,
            Loot::Itself,
            all(L_OAK_PLANKS),
        ),
        BIRCH_PLANKS => solid(
            "birch_planks",
            BIRCH_PLANKS,
            2.0,
            false,
            AXE,
            Wood,
            Loot::Itself,
            all(L_BIRCH_PLANKS),
        ),
        SPRUCE_PLANKS => solid(
            "spruce_planks",
            SPRUCE_PLANKS,
            2.0,
            false,
            AXE,
            Wood,
            Loot::Itself,
            all(L_SPRUCE_PLANKS),
        ),
        CRAFTING_TABLE => {
            let tex = [
                L_TABLE_SIDE,
                L_TABLE_FRONT,
                L_TABLE_TOP,
                L_OAK_PLANKS,
                L_TABLE_SIDE,
                L_TABLE_FRONT,
            ];
            let mut d = solid(
                "crafting_table",
                CRAFTING_TABLE,
                2.5,
                false,
                AXE,
                Wood,
                Loot::Itself,
                tex,
            );
            d.on_use = OnUse::Crafting;
            d
        }
        FURNACE => {
            let mut d = solid(
                "furnace",
                FURNACE,
                3.5,
                true,
                PICKAXE,
                Stone,
                Loot::Itself,
                faced(L_FURNACE_TOP, L_FURNACE_SIDE, L_FURNACE_FRONT),
            );
            d.tex_rule = TexRule::Facing {
                lit: L_FURNACE_FRONT_ON,
            };
            d.place = Place::FacePlayer;
            d.on_use = OnUse::Furnace;
            d.be = Some(BeKind::Furnace);
            d.display = Display::Furnace;
            d.light = 13;
            d.lit_only = true;
            d
        }
        COAL_ORE => solid(
            "coal_ore",
            COAL_ORE,
            3.0,
            true,
            PICKAXE,
            Stone,
            Loot::Table(DROP_COAL),
            all(L_COAL_ORE),
        ),
        IRON_ORE => {
            let mut d = solid(
                "iron_ore",
                IRON_ORE,
                3.0,
                true,
                PICKAXE,
                Stone,
                Loot::Table(DROP_RAW_IRON),
                all(L_IRON_ORE),
            );
            d.needs = 1;
            d
        }
        IRON_BLOCK => {
            let mut d = solid(
                "iron_block",
                IRON_BLOCK,
                5.0,
                true,
                PICKAXE,
                Metal,
                Loot::Itself,
                all(L_IRON_BLOCK),
            );
            d.needs = 1;
            d
        }
        RAW_IRON_BLOCK => {
            let mut d = solid(
                "raw_iron_block",
                RAW_IRON_BLOCK,
                5.0,
                true,
                PICKAXE,
                Stone,
                Loot::Itself,
                all(L_RAW_IRON_BLOCK),
            );
            d.needs = 1;
            d
        }
        COAL_BLOCK => solid(
            "coal_block",
            COAL_BLOCK,
            5.0,
            true,
            PICKAXE,
            Stone,
            Loot::Itself,
            all(L_BEDROCK),
        ),
        GLASS => {
            let mut d = solid(
                "glass",
                GLASS,
                0.3,
                false,
                0,
                Glass,
                Loot::None,
                all(L_GLASS),
            );
            d.suffocates = false;
            d.render = Render::Glass;
            d.never_solid = true;
            d
        }
        TORCH => {
            let mut d = plant(
                "torch",
                TORCH,
                0,
                false,
                TORCH_SHAPE,
                Offset::None,
                Loot::Itself,
                Support::Floor,
                L_TORCH,
            );
            d.sword_fast = false;
            d.sound = Wood;
            d.place = Place::Torch { wall: WALL_TORCH };
            d.light = 14;
            d
        }
        WALL_TORCH => {
            let mut d = plant(
                "wall_torch",
                TORCH,
                0,
                false,
                WALL_TORCH_SHAPES[0],
                Offset::None,
                Loot::Itself,
                Support::Wall,
                L_TORCH,
            );
            d.sword_fast = false;
            d.sound = Wood;
            d.render = Render::Model(Model::WallTorch);
            d.shape = Shape::WallTorch;
            d.flat_item = false;
            d.light = 14;
            d
        }
        CHEST => {
            let mut d = solid(
                "chest",
                CHEST,
                2.5,
                false,
                AXE,
                Wood,
                Loot::Itself,
                faced(L_CHEST_TOP, L_CHEST_SIDE, L_CHEST_FRONT),
            );
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
            let mut d = solid(
                "ladder",
                LADDER,
                0.4,
                false,
                AXE,
                Ladder,
                Loot::Itself,
                all(L_LADDER),
            );
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
            let mut d = solid(
                "snow",
                SNOW_LAYER,
                0.1,
                true,
                SHOVEL,
                Snow,
                Loot::SnowLayers,
                all(L_SNOW),
            );
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
            let mut d = ore(
                "redstone_ore",
                REDSTONE_ORE,
                2,
                DROP_REDSTONE,
                L_REDSTONE_ORE,
            );
            d.light = 9;
            d.lit_only = true;
            d
        }
        STEEL_CASING => solid(
            "steel_casing",
            STEEL_CASING,
            3.5,
            true,
            PICKAXE,
            Metal,
            Loot::Itself,
            all(L_STEEL_CASING),
        ),
        STEEL_BLOCK => solid(
            "block_steel",
            STEEL_BLOCK,
            5.0,
            true,
            PICKAXE,
            Metal,
            Loot::Itself,
            all(L_STEEL_BLOCK),
        ),
        OSMIUM_BLOCK => {
            let mut d = solid(
                "block_osmium",
                OSMIUM_BLOCK,
                7.5,
                true,
                PICKAXE,
                Metal,
                Loot::Itself,
                all(L_OSMIUM_BLOCK),
            );
            d.needs = 1;
            d
        }
        REDSTONE_BLOCK => solid(
            "redstone_block",
            REDSTONE_BLOCK,
            5.0,
            true,
            PICKAXE,
            Metal,
            Loot::Itself,
            all(L_REDSTONE_BLOCK),
        ),
        WHITE_WOOL => solid(
            "white_wool",
            WHITE_WOOL,
            0.8,
            false,
            0,
            Wool,
            Loot::Itself,
            all(L_WOOL_WHITE),
        ),
        BLACK_WOOL => solid(
            "black_wool",
            BLACK_WOOL,
            0.8,
            false,
            0,
            Wool,
            Loot::Itself,
            all(L_WOOL_BLACK),
        ),
        GRAY_WOOL => solid(
            "gray_wool",
            GRAY_WOOL,
            0.8,
            false,
            0,
            Wool,
            Loot::Itself,
            all(L_WOOL_GRAY),
        ),
        LIGHT_GRAY_WOOL => solid(
            "light_gray_wool",
            LIGHT_GRAY_WOOL,
            0.8,
            false,
            0,
            Wool,
            Loot::Itself,
            all(L_WOOL_LIGHT_GRAY),
        ),
        BROWN_WOOL => solid(
            "brown_wool",
            BROWN_WOOL,
            0.8,
            false,
            0,
            Wool,
            Loot::Itself,
            all(L_WOOL_BROWN),
        ),
        PINK_WOOL => solid(
            "pink_wool",
            PINK_WOOL,
            0.8,
            false,
            0,
            Wool,
            Loot::Itself,
            all(L_WOOL_PINK),
        ),
        HEAT_GENERATOR => machine(
            "heat_generator",
            HEAT_GENERATOR,
            MekKind::Heat,
            3.5,
            faced(L_HEATGEN_DRUM, L_HEATGEN_BODY, L_MEK_BACK),
            13,
        ),
        METALLURGIC_INFUSER => machine(
            "metallurgic_infuser",
            METALLURGIC_INFUSER,
            MekKind::Infuser,
            3.5,
            faced(L_MEK_TOP, L_INFUSER_BODY, L_INFUSER_BODY),
            8,
        ),
        ENRICHMENT_CHAMBER => chamber(
            "enrichment_chamber",
            ENRICHMENT_CHAMBER,
            MekKind::Enrich,
            L_ENRICH_FRONT,
            L_ENRICH_FRONT_ON,
        ),
        ENERGIZED_SMELTER => chamber(
            "energized_smelter",
            ENERGIZED_SMELTER,
            MekKind::Smelter,
            L_SMELTER_FRONT,
            L_SMELTER_FRONT_ON,
        ),
        ENERGY_CUBE => {
            let mut d = machine(
                "basic_energy_cube",
                ENERGY_CUBE,
                MekKind::Cube,
                2.0,
                all(L_CUBE_FRAME),
                0,
            );
            d.lit_only = false;
            d
        }
        CRUSHER => chamber(
            "crusher",
            CRUSHER,
            MekKind::Crusher,
            L_CRUSHER_FRONT,
            L_CRUSHER_FRONT_ON,
        ),
        PURIFICATION_CHAMBER => chamber(
            "purification_chamber",
            PURIFICATION_CHAMBER,
            MekKind::Purifier,
            L_PURIFIER_FRONT,
            L_PURIFIER_FRONT_ON,
        ),
        ELECTROLYTIC_SEPARATOR => machine(
            "electrolytic_separator",
            ELECTROLYTIC_SEPARATOR,
            MekKind::Separator,
            3.5,
            faced(L_MEK_TOP, L_SEPARATOR_BODY, L_SEPARATOR_BODY),
            8,
        ),
        WIND_GENERATOR => machine(
            "wind_generator",
            WIND_GENERATOR,
            MekKind::Wind,
            3.5,
            faced(L_MEK_TOP, L_MEK_SIDE, L_MEK_BACK),
            8,
        ),
        BOUNDING => {
            let mut d = solid(
                "bounding_block",
                WIND_GENERATOR,
                3.5,
                true,
                PICKAXE,
                Metal,
                Loot::None,
                all(L_MEK_SIDE),
            );
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
            let mut d = solid(
                "basic_universal_cable",
                UNIVERSAL_CABLE,
                1.0,
                false,
                PICKAXE,
                Metal,
                Loot::Itself,
                all(L_CABLE_SIDE),
            );
            d.outline = CABLE_CORE;
            d.collision = CABLE_CORE;
            d.suffocates = false;
            d.render = Render::Model(Model::Cable);
            d.shape = Shape::Cable;
            d.sides = Sides::Empty;
            d
        }
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
        ELECTRIC_PUMP => {
            let mut d = machine(
                "electric_pump",
                ELECTRIC_PUMP,
                MekKind::Pump,
                3.5,
                cap(L_PUMP_TOP, L_PUMP_BOTTOM, L_PUMP_SIDE),
                8,
            );
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

const K_OPAQUE: u8 = 1;
const K_CROSS: u8 = 2;
const K_LEAVES: u8 = 4;
const K_MODEL: u8 = 8;
const K_GRASS: u8 = 16;
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
        if matches!(
            d.render,
            Render::None | Render::Glass | Render::Cross | Render::Model(_)
        ) {
            k |= K_WATER_FACE;
        }
        t[i] = k;
        i += 1;
    }
    t
};

#[inline]
fn kind(b: BlockId) -> u8 {
    match KIND.get(b as usize) {
        Some(&k) => k,
        None => K_OPAQUE,
    }
}

#[inline]
pub fn bdef(b: BlockId) -> &'static BlockDef {
    BLOCKS.get(b as usize).unwrap_or(&UNKNOWN)
}

pub fn block_by_mc(name: &str) -> BlockId {
    (0..BLOCK_COUNT)
        .find(|&i| BLOCKS[i].mc == name)
        .unwrap_or(0) as BlockId
}

#[inline]
pub fn opaque(b: BlockId) -> bool {
    kind(b) & K_OPAQUE != 0
}
#[inline]
pub fn is_plant(b: BlockId) -> bool {
    kind(b) & K_CROSS != 0
}
#[inline]
pub fn is_grass(b: BlockId) -> bool {
    kind(b) & K_GRASS != 0
}
#[inline]
pub fn is_leaves(b: BlockId) -> bool {
    kind(b) & K_LEAVES != 0
}
pub fn is_log(b: BlockId) -> bool {
    bdef(b).log
}
#[inline]
pub fn is_model(b: BlockId) -> bool {
    kind(b) & K_MODEL != 0
}
#[inline]
pub fn water_faces(nb: BlockId) -> bool {
    kind(nb) & K_WATER_FACE != 0
}
pub fn model_of(b: BlockId) -> Option<Model> {
    match bdef(b).render {
        Render::Model(m) => Some(m),
        _ => None,
    }
}
pub fn is_falling(b: BlockId) -> bool {
    bdef(b).falling
}
#[inline]
pub fn layer(b: BlockId, face: usize) -> u32 {
    bdef(b).tex[face]
}
#[inline]
pub fn tinted(b: BlockId, face: usize) -> bool {
    bdef(b).tint >> face & 1 != 0
}
#[allow(dead_code)]
pub fn light_of(c: Cell) -> u8 {
    let d = bdef(block_of(c));
    if d.lit_only && state_of(c) & ST_LIT == 0 {
        0
    } else {
        d.light
    }
}

pub fn layer_cell(b: BlockId, s: State, face: usize) -> (u32, bool) {
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
            (
                base + if wheat {
                    age
                } else {
                    [0, 0, 1, 1, 2, 2, 2, 3][age as usize]
                },
                false,
            )
        }
        TexRule::Farmland => (
            if face != 2 {
                L_DIRT
            } else if s & 7 == 7 {
                L_FARMLAND_WET
            } else {
                L_FARMLAND_DRY
            },
            false,
        ),
    }
}

fn cable_shape(c: Cell) -> &'static [Aabb] {
    let m = crate::arms::arm_mask(state_of(c)) as usize;
    &CABLE_SHAPES.0[m][..CABLE_SHAPES.1[m] as usize]
}
fn large_shape(c: Cell) -> &'static [Aabb] {
    let m = crate::arms::arm_mask(state_of(c)) as usize;
    &LARGE_SHAPES.0[m][..LARGE_SHAPES.1[m] as usize]
}
static CABLE_SHAPES: ([[Aabb; 7]; 64], [u8; 64]) = transmitter_shapes(5.0, 11.0);
static LARGE_SHAPES: ([[Aabb; 7]; 64], [u8; 64]) = transmitter_shapes(4.0, 12.0);
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

pub fn outline_of(c: Cell) -> &'static [Aabb] {
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
#[inline]
pub fn collision_of(c: Cell) -> &'static [Aabb] {
    let d = bdef(block_of(c));
    match d.shape {
        Shape::Ladder => LADDER_SHAPES[hidx(facing_of(c))],
        Shape::Layers => SNOW_SHAPES[snow_layers(c) as usize - 1],
        Shape::Cable => cable_shape(c),
        Shape::Large => large_shape(c),
        Shape::Fixed | Shape::WallTorch | Shape::Wheat | Shape::Root => d.collision,
    }
}
pub fn sides_of(c: Cell) -> &'static [Aabb] {
    match bdef(block_of(c)).sides {
        Sides::Empty => &[],
        Sides::Box(s) => s,
        Sides::Outline => outline_of(c),
        Sides::Collision => collision_of(c),
    }
}
pub fn face_full(c: Cell, dir: u8) -> bool {
    let d = DIRS[dir as usize];
    let a = d.iter().position(|&v| v != 0).unwrap_or(1);
    sides_of(c).iter().any(|s| {
        let touches = if d[a] > 0 {
            s.hi[a] >= 1.0
        } else {
            s.lo[a] <= 0.0
        };
        touches
            && (0..3)
                .filter(|&k| k != a)
                .all(|k| s.lo[k] <= 0.0 && s.hi[k] >= 1.0)
    })
}
pub fn top_center(c: Cell) -> bool {
    let (lo, hi) = (7.0 / 16.0, 9.0 / 16.0);
    sides_of(c)
        .iter()
        .any(|s| s.hi[1] >= 1.0 && s.lo[0] <= lo && s.hi[0] >= hi && s.lo[2] <= lo && s.hi[2] >= hi)
}
pub fn solid_block(c: Cell) -> bool {
    let s = collision_of(c);
    !bdef(block_of(c)).never_solid && s.len() == 1 && s[0] == CUBE[0]
}
pub fn replaceable_by(c: Cell, item: ItemId) -> bool {
    let b = block_of(c);
    let d = bdef(b);
    if d.shape == Shape::Layers {
        let n = snow_layers(c);
        return if item == block_item(b) && n < 8 {
            true
        } else {
            n == 1
        };
    }
    d.replaceable && (item == 0 || block_item(b) != item) // item 0 — пустой стек (приземление песка)
}
pub fn falls_through(c: Cell) -> bool {
    bdef(block_of(c)).replaceable
}

pub fn is_dirt_tag(b: BlockId) -> bool {
    bdef(b).dirt
}
pub fn is_farmland(b: BlockId) -> bool {
    bdef(b).farmland
}
pub fn plant_supported(plant: BlockId, below: BlockId) -> bool {
    match bdef(plant).support {
        Support::Plant => is_dirt_tag(below) || is_farmland(below),
        Support::DeadBush => is_dirt_tag(below) || below == SAND,
        Support::Floor => is_full_cube(below) && !is_leaves(below),
        _ => true,
    }
}
pub fn is_full_cube(b: BlockId) -> bool {
    let c = bdef(b).collision;
    c.len() == 1 && c[0] == CUBE[0]
}

pub fn mc_hash(x: i32, y: i32, z: i32) -> i64 {
    let mut l = (x.wrapping_mul(3129871) as i64) ^ (z as i64).wrapping_mul(116129781) ^ (y as i64);
    l = l
        .wrapping_mul(l)
        .wrapping_mul(42317861)
        .wrapping_add(l.wrapping_mul(11));
    l >> 16
}

pub fn plant_offset(b: BlockId, x: i32, z: i32) -> [f64; 3] {
    let kind = bdef(b).offset;
    if kind == Offset::None {
        return [0.0; 3];
    }
    let l = mc_hash(x, 0, z);
    let h = |s: i64| ((((l >> s) & 15) as f32 / 15.0) as f64 - 0.5) * 0.5;
    let (dx, dz) = (h(0).clamp(-0.25, 0.25), h(8).clamp(-0.25, 0.25));
    let dy = if kind == Offset::Xyz {
        ((((l >> 4) & 15) as f32 / 15.0) as f64 - 1.0) * (0.2f32 as f64)
    } else {
        0.0
    };
    [dx, dy, dz]
}

pub fn outline_at(c: Cell, x: i32, y: i32, z: i32, offsets: bool, out: &mut Vec<Aabb>) {
    let b = block_of(c);
    let d = bdef(b);
    let o = if offsets && d.outline_offset {
        plant_offset(b, x, z)
    } else {
        [0.0; 3]
    };
    let p = [x as f64 + o[0], y as f64 + o[1], z as f64 + o[2]];
    for s in outline_of(c) {
        out.push(Aabb {
            lo: [p[0] + s.lo[0], p[1] + s.lo[1], p[2] + s.lo[2]],
            hi: [p[0] + s.hi[0], p[1] + s.hi[1], p[2] + s.hi[2]],
        });
    }
}
