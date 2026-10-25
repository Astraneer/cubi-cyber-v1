//! Pure machine recipes from the SPEC-approved donor at
//! 3992583213e1f00d95a3beef2bde43ae970192d4 (world/mek/recipes.rs).

use crate::cell::BlockId;
use crate::ids::{
    CLAY, COAL_BLOCK, COBBLE, GRAVEL, RAW_IRON_BLOCK, REDSTONE_BLOCK, SAND, SANDSTONE, STONE,
};
use crate::items::*;

pub const SUB_NONE: u8 = 0;
pub const SUB_CARBON: u8 = 1;
pub const SUB_REDSTONE: u8 = 2;
pub const SUB_LAVA: u8 = 3;
pub const SUB_OXYGEN: u8 = 4;
pub const SUB_HYDROGEN: u8 = 5;
pub const SUB_WATER: u8 = 6;

pub fn sub_name(s: u8) -> &'static str {
    match s {
        SUB_CARBON => "Углерод",
        SUB_REDSTONE => "Редстоун",
        SUB_LAVA => "Лава",
        SUB_OXYGEN => "Кислород",
        SUB_HYDROGEN => "Водород",
        SUB_WATER => "Вода",
        _ => "Пусто",
    }
}
pub fn sub_color(s: u8) -> u32 {
    match s {
        SUB_CARBON => 0x2C2C2C,
        SUB_REDSTONE => 0xB30505,
        SUB_LAVA => 0xE8601C,
        SUB_OXYGEN => 0x6CE2FF,
        SUB_HYDROGEN => 0xFFFFFF,
        SUB_WATER => 0x3F76E4,
        _ => 0,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Infuse {
    pub input: ItemId,
    pub sub: u8,
    pub amount: u32,
    pub out: ItemId,
}

pub const INFUSE: [Infuse; 6] = [
    Infuse {
        input: I_IRON_INGOT,
        sub: SUB_CARBON,
        amount: 10,
        out: I_ENRICHED_IRON,
    },
    Infuse {
        input: I_DUST_IRON,
        sub: SUB_CARBON,
        amount: 10,
        out: I_ENRICHED_IRON,
    },
    Infuse {
        input: I_ENRICHED_IRON,
        sub: SUB_CARBON,
        amount: 10,
        out: I_DUST_STEEL,
    },
    Infuse {
        input: I_OSMIUM_INGOT,
        sub: SUB_REDSTONE,
        amount: 20,
        out: I_BASIC_CIRCUIT,
    },
    Infuse {
        input: I_COPPER_INGOT,
        sub: SUB_REDSTONE,
        amount: 10,
        out: I_ALLOY_INFUSED,
    },
    Infuse {
        input: I_BASIC_CIRCUIT,
        sub: SUB_REDSTONE,
        amount: 60,
        out: I_ADVANCED_CIRCUIT,
    },
];

pub fn infusable(item: ItemId) -> bool {
    INFUSE.iter().any(|r| r.input == item)
}
pub fn find_infuse(item: ItemId, sub: u8, amount: u32) -> Option<u8> {
    INFUSE
        .iter()
        .position(|r| r.input == item && r.sub == sub && amount >= r.amount)
        .map(|i| i as u8)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Enrich {
    pub input: ItemId,
    pub in_n: i32,
    pub out: ItemId,
    pub out_n: i32,
}

const fn en(input: ItemId, in_n: i32, out: ItemId, out_n: i32) -> Enrich {
    Enrich {
        input,
        in_n,
        out,
        out_n,
    }
}

pub const ENRICH: [Enrich; 17] = [
    en(I_RAW_IRON, 3, I_DUST_IRON, 4),
    en(I_RAW_OSMIUM, 3, I_DUST_OSMIUM, 4),
    en(I_RAW_COPPER, 3, I_DUST_COPPER, 4),
    en(I_RAW_GOLD, 3, I_DUST_GOLD, 4),
    en(I_RAW_TIN, 3, I_DUST_TIN, 4),
    en(RAW_IRON_BLOCK as ItemId, 1, I_DUST_IRON, 12),
    en(I_COAL, 1, I_ENRICHED_CARBON, 1),
    en(I_CHARCOAL, 1, I_ENRICHED_CARBON, 1),
    en(I_REDSTONE, 1, I_ENRICHED_REDSTONE, 1),
    en(GRAVEL as ItemId, 1, I_FLINT, 1),
    en(SAND as ItemId, 1, GRAVEL as ItemId, 1),
    en(CLAY as ItemId, 1, I_CLAY_BALL, 4),
    en(I_DIRTY_DUST_IRON, 1, I_DUST_IRON, 1),
    en(I_DIRTY_DUST_IRON + 1, 1, I_DUST_OSMIUM, 1),
    en(I_DIRTY_DUST_IRON + 2, 1, I_DUST_COPPER, 1),
    en(I_DIRTY_DUST_IRON + 3, 1, I_DUST_GOLD, 1),
    en(I_DIRTY_DUST_IRON + 4, 1, I_DUST_TIN, 1),
];

pub const METALS: [(ItemId, ItemId, ItemId); 5] = [
    (I_RAW_IRON, I_DUST_IRON, I_IRON_INGOT),
    (I_RAW_OSMIUM, I_DUST_OSMIUM, I_OSMIUM_INGOT),
    (I_RAW_COPPER, I_DUST_COPPER, I_COPPER_INGOT),
    (I_RAW_GOLD, I_DUST_GOLD, I_GOLD_INGOT),
    (I_RAW_TIN, I_DUST_TIN, I_TIN_INGOT),
];

pub const CRUSH: [Enrich; 15] = [
    en(STONE as ItemId, 1, COBBLE as ItemId, 1),
    en(COBBLE as ItemId, 1, GRAVEL as ItemId, 1),
    en(GRAVEL as ItemId, 1, SAND as ItemId, 1),
    en(SANDSTONE as ItemId, 1, SAND as ItemId, 2),
    en(I_IRON_INGOT, 1, I_DUST_IRON, 1),
    en(I_OSMIUM_INGOT, 1, I_DUST_OSMIUM, 1),
    en(I_COPPER_INGOT, 1, I_DUST_COPPER, 1),
    en(I_GOLD_INGOT, 1, I_DUST_GOLD, 1),
    en(I_TIN_INGOT, 1, I_DUST_TIN, 1),
    en(I_STEEL_INGOT, 1, I_DUST_STEEL, 1),
    en(I_CLUMP_IRON, 1, I_DIRTY_DUST_IRON, 1),
    en(I_CLUMP_IRON + 1, 1, I_DIRTY_DUST_IRON + 1, 1),
    en(I_CLUMP_IRON + 2, 1, I_DIRTY_DUST_IRON + 2, 1),
    en(I_CLUMP_IRON + 3, 1, I_DIRTY_DUST_IRON + 3, 1),
    en(I_CLUMP_IRON + 4, 1, I_DIRTY_DUST_IRON + 4, 1),
];

pub fn find_crush(item: ItemId) -> Option<u8> {
    CRUSH.iter().position(|r| r.input == item).map(|i| i as u8)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Purify {
    pub input: ItemId,
    pub rate: u32,
    pub out: ItemId,
    pub out_n: i32,
}

pub const PURIFY: [Purify; 6] = [
    Purify {
        input: I_RAW_IRON,
        rate: 1,
        out: I_CLUMP_IRON,
        out_n: 2,
    },
    Purify {
        input: I_RAW_OSMIUM,
        rate: 1,
        out: I_CLUMP_IRON + 1,
        out_n: 2,
    },
    Purify {
        input: I_RAW_COPPER,
        rate: 1,
        out: I_CLUMP_IRON + 2,
        out_n: 2,
    },
    Purify {
        input: I_RAW_GOLD,
        rate: 1,
        out: I_CLUMP_IRON + 3,
        out_n: 2,
    },
    Purify {
        input: I_RAW_TIN,
        rate: 1,
        out: I_CLUMP_IRON + 4,
        out_n: 2,
    },
    Purify {
        input: RAW_IRON_BLOCK as ItemId,
        rate: 2,
        out: I_CLUMP_IRON,
        out_n: 18,
    },
];

pub fn find_purify(item: ItemId) -> Option<u8> {
    PURIFY.iter().position(|r| r.input == item).map(|i| i as u8)
}

pub fn find_enrich(item: ItemId) -> Option<u8> {
    ENRICH.iter().position(|r| r.input == item).map(|i| i as u8)
}

pub fn oxygen_of(item: ItemId) -> u32 {
    if item == I_FLINT { 10 } else { 0 }
}

pub fn chem_of(item: ItemId) -> Option<(u8, u32)> {
    let b = |x: BlockId| x as ItemId;
    Some(match item {
        I_COAL => (SUB_CARBON, 10),
        I_CHARCOAL => (SUB_CARBON, 20),
        x if x == b(COAL_BLOCK) => (SUB_CARBON, 90),
        I_ENRICHED_CARBON => (SUB_CARBON, 80),
        I_REDSTONE => (SUB_REDSTONE, 10),
        x if x == b(REDSTONE_BLOCK) => (SUB_REDSTONE, 90),
        I_ENRICHED_REDSTONE => (SUB_REDSTONE, 80),
        _ => return None,
    })
}

pub fn energy_of(item: ItemId) -> u64 {
    if item == I_REDSTONE {
        10_000
    } else if item == REDSTONE_BLOCK as ItemId {
        90_000
    } else {
        0
    }
}

pub fn crafting() -> Vec<crate::crafting::RecipeDef> {
    use crate::crafting::{Ing::*, RShape::*, RecipeDef, TAG_PLANKS};
    use crate::ids::*;
    let i = |b: BlockId| b as ItemId;
    let mut v = vec![
        RecipeDef {
            name: "heat_generator",
            shape: Shaped(
                vec!["III", "WOW", "CFC"],
                vec![
                    ('I', Item(I_IRON_INGOT)),
                    ('W', Set(TAG_PLANKS)),
                    ('O', Item(I_OSMIUM_INGOT)),
                    ('C', Item(I_COPPER_INGOT)),
                    ('F', Item(i(FURNACE))),
                ],
            ),
            out: i(HEAT_GENERATOR),
            count: 1,
        },
        RecipeDef {
            name: "metallurgic_infuser",
            shape: Shaped(
                vec!["IFI", "ROR", "IFI"],
                vec![
                    ('I', Item(I_IRON_INGOT)),
                    ('F', Item(i(FURNACE))),
                    ('R', Item(I_REDSTONE)),
                    ('O', Item(I_OSMIUM_INGOT)),
                ],
            ),
            out: i(METALLURGIC_INFUSER),
            count: 1,
        },
        RecipeDef {
            name: "basic_universal_cable",
            shape: Shaped(
                vec!["SRS"],
                vec![('S', Item(I_STEEL_INGOT)), ('R', Item(I_REDSTONE))],
            ),
            out: i(UNIVERSAL_CABLE),
            count: 8,
        },
        RecipeDef {
            name: "steel_casing",
            shape: Shaped(
                vec!["SGS", "GOG", "SGS"],
                vec![
                    ('S', Item(I_STEEL_INGOT)),
                    ('G', Item(i(GLASS))),
                    ('O', Item(I_OSMIUM_INGOT)),
                ],
            ),
            out: i(STEEL_CASING),
            count: 1,
        },
        RecipeDef {
            name: "energy_tablet",
            shape: Shaped(
                vec!["RGR", "AGA", "RGR"],
                vec![
                    ('R', Item(I_REDSTONE)),
                    ('G', Item(I_GOLD_INGOT)),
                    ('A', Item(I_ALLOY_INFUSED)),
                ],
            ),
            out: I_ENERGY_TABLET,
            count: 1,
        },
        RecipeDef {
            name: "basic_energy_cube",
            shape: Shaped(
                vec!["RTR", "ICI", "RTR"],
                vec![
                    ('R', Item(I_REDSTONE)),
                    ('T', Item(I_ENERGY_TABLET)),
                    ('I', Item(I_IRON_INGOT)),
                    ('C', Item(i(STEEL_CASING))),
                ],
            ),
            out: i(ENERGY_CUBE),
            count: 1,
        },
        RecipeDef {
            name: "wind_generator",
            shape: Shaped(
                vec![" O ", "OAO", "TCT"],
                vec![
                    ('O', Item(I_OSMIUM_INGOT)),
                    ('A', Item(I_ALLOY_INFUSED)),
                    ('T', Item(I_ENERGY_TABLET)),
                    ('C', Item(I_BASIC_CIRCUIT)),
                ],
            ),
            out: i(WIND_GENERATOR),
            count: 1,
        },
        RecipeDef {
            name: "configurator",
            shape: Shaped(
                vec!["I O", " S ", " I "],
                vec![
                    ('I', Item(I_IRON_INGOT)),
                    ('O', Item(I_OSMIUM_INGOT)),
                    ('S', Item(I_STEEL_INGOT)),
                ],
            ),
            out: I_CONFIGURATOR,
            count: 1,
        },
        RecipeDef {
            name: "upgrade_speed",
            shape: Shaped(
                vec![" G ", "ADA", " G "],
                vec![
                    ('G', Item(i(GLASS))),
                    ('A', Item(I_ALLOY_INFUSED)),
                    ('D', Item(I_DUST_OSMIUM)),
                ],
            ),
            out: I_UPGRADE_SPEED,
            count: 1,
        },
        RecipeDef {
            name: "upgrade_energy",
            shape: Shaped(
                vec![" G ", "ADA", " G "],
                vec![
                    ('G', Item(i(GLASS))),
                    ('A', Item(I_ALLOY_INFUSED)),
                    ('D', Item(I_DUST_GOLD)),
                ],
            ),
            out: I_UPGRADE_ENERGY,
            count: 1,
        },
        RecipeDef {
            name: "enrichment_chamber",
            shape: Shaped(
                vec!["RCR", "ISI", "RCR"],
                vec![
                    ('R', Item(I_REDSTONE)),
                    ('C', Item(I_BASIC_CIRCUIT)),
                    ('I', Item(I_IRON_INGOT)),
                    ('S', Item(i(STEEL_CASING))),
                ],
            ),
            out: i(ENRICHMENT_CHAMBER),
            count: 1,
        },
        RecipeDef {
            name: "energized_smelter",
            shape: Shaped(
                vec!["RCR", "GSG", "RCR"],
                vec![
                    ('R', Item(I_REDSTONE)),
                    ('C', Item(I_BASIC_CIRCUIT)),
                    ('G', Item(i(GLASS))),
                    ('S', Item(i(STEEL_CASING))),
                ],
            ),
            out: i(ENERGIZED_SMELTER),
            count: 1,
        },
        RecipeDef {
            name: "crusher",
            shape: Shaped(
                vec!["RCR", "LSL", "RCR"],
                vec![
                    ('R', Item(I_REDSTONE)),
                    ('C', Item(I_BASIC_CIRCUIT)),
                    ('L', Item(I_LAVA_BUCKET)),
                    ('S', Item(i(STEEL_CASING))),
                ],
            ),
            out: i(CRUSHER),
            count: 1,
        },
        RecipeDef {
            name: "advanced_control_circuit",
            shape: Shaped(
                vec!["ACA"],
                vec![('A', Item(I_ALLOY_INFUSED)), ('C', Item(I_BASIC_CIRCUIT))],
            ),
            out: I_ADVANCED_CIRCUIT,
            count: 1,
        },
        RecipeDef {
            name: "purification_chamber",
            shape: Shaped(
                vec!["ACA", "OEO", "ACA"],
                vec![
                    ('A', Item(I_ALLOY_INFUSED)),
                    ('C', Item(I_ADVANCED_CIRCUIT)),
                    ('O', Item(I_OSMIUM_INGOT)),
                    ('E', Item(i(ENRICHMENT_CHAMBER))),
                ],
            ),
            out: i(PURIFICATION_CHAMBER),
            count: 1,
        },
        RecipeDef {
            name: "electrolytic_core",
            shape: Shaped(
                vec!["AOA", "IAG", "AOA"],
                vec![
                    ('A', Item(I_ALLOY_INFUSED)),
                    ('O', Item(I_DUST_OSMIUM)),
                    ('I', Item(I_DUST_IRON)),
                    ('G', Item(I_DUST_GOLD)),
                ],
            ),
            out: I_ELECTROLYTIC_CORE,
            count: 1,
        },
        RecipeDef {
            name: "electrolytic_separator",
            shape: Shaped(
                vec!["IRI", "ACA", "IRI"],
                vec![
                    ('I', Item(I_IRON_INGOT)),
                    ('R', Item(I_REDSTONE)),
                    ('A', Item(I_ALLOY_INFUSED)),
                    ('C', Item(I_ELECTROLYTIC_CORE)),
                ],
            ),
            out: i(ELECTROLYTIC_SEPARATOR),
            count: 1,
        },
        RecipeDef {
            name: "basic_logistical_transporter",
            shape: Shaped(
                vec!["SCS"],
                vec![('S', Item(I_STEEL_INGOT)), ('C', Item(I_BASIC_CIRCUIT))],
            ),
            out: i(LOGISTICAL_TRANSPORTER),
            count: 8,
        },
        RecipeDef {
            name: "basic_mechanical_pipe",
            shape: Shaped(
                vec!["SBS"],
                vec![('S', Item(I_STEEL_INGOT)), ('B', Item(I_BUCKET))],
            ),
            out: i(MECHANICAL_PIPE),
            count: 8,
        },
        RecipeDef {
            name: "basic_pressurized_tube",
            shape: Shaped(
                vec!["SGS"],
                vec![('S', Item(I_STEEL_INGOT)), ('G', Item(i(GLASS)))],
            ),
            out: i(PRESSURIZED_TUBE),
            count: 8,
        },
        RecipeDef {
            name: "electric_pump",
            shape: Shaped(
                vec![" B ", "ACA", "OOO"],
                vec![
                    ('B', Item(I_BUCKET)),
                    ('A', Item(I_ALLOY_INFUSED)),
                    ('C', Item(i(STEEL_CASING))),
                    ('O', Item(I_OSMIUM_INGOT)),
                ],
            ),
            out: i(ELECTRIC_PUMP),
            count: 1,
        },
        RecipeDef {
            name: "upgrade_gas",
            shape: Shaped(
                vec![" G ", "ADA", " G "],
                vec![
                    ('G', Item(i(GLASS))),
                    ('A', Item(I_ALLOY_INFUSED)),
                    ('D', Item(I_DUST_IRON)),
                ],
            ),
            out: I_UPGRADE_GAS,
            count: 1,
        },
        RecipeDef {
            name: "steel_nugget",
            shape: Shapeless(vec![Item(I_STEEL_INGOT)]),
            out: I_STEEL_NUGGET,
            count: 9,
        },
        RecipeDef {
            name: "steel_ingot_from_nuggets",
            shape: Shaped(vec!["###", "###", "###"], vec![('#', Item(I_STEEL_NUGGET))]),
            out: I_STEEL_INGOT,
            count: 1,
        },
        RecipeDef {
            name: "block_steel",
            shape: Shaped(vec!["###", "###", "###"], vec![('#', Item(I_STEEL_INGOT))]),
            out: i(STEEL_BLOCK),
            count: 1,
        },
        RecipeDef {
            name: "steel_ingot_from_block",
            shape: Shapeless(vec![Item(i(STEEL_BLOCK))]),
            out: I_STEEL_INGOT,
            count: 9,
        },
        RecipeDef {
            name: "block_osmium",
            shape: Shaped(vec!["###", "###", "###"], vec![('#', Item(I_OSMIUM_INGOT))]),
            out: i(OSMIUM_BLOCK),
            count: 1,
        },
        RecipeDef {
            name: "osmium_ingot_from_block",
            shape: Shapeless(vec![Item(i(OSMIUM_BLOCK))]),
            out: I_OSMIUM_INGOT,
            count: 9,
        },
        RecipeDef {
            name: "redstone_block",
            shape: Shaped(vec!["###", "###", "###"], vec![('#', Item(I_REDSTONE))]),
            out: i(REDSTONE_BLOCK),
            count: 1,
        },
        RecipeDef {
            name: "redstone",
            shape: Shapeless(vec![Item(i(REDSTONE_BLOCK))]),
            out: I_REDSTONE,
            count: 9,
        },
    ];
    let shapes: [(&'static str, Vec<&'static str>); 5] = [
        ("steel_pickaxe", vec!["XXX", " # ", " # "]),
        ("steel_axe", vec!["XX", "X#", " #"]),
        ("steel_shovel", vec!["X", "#", "#"]),
        ("steel_hoe", vec!["XX", " #", " #"]),
        ("steel_sword", vec!["X", "X", "#"]),
    ];
    for (k, (name, rows)) in shapes.into_iter().enumerate() {
        v.push(RecipeDef {
            name,
            shape: Shaped(
                rows,
                vec![('X', Item(I_STEEL_INGOT)), ('#', Item(I_IRON_INGOT))],
            ),
            out: I_STEEL_PICKAXE + k as ItemId,
            count: 1,
        });
    }
    let parts: [Vec<&'static str>; 4] = [
        vec!["XXX", "X X"],
        vec!["X X", "XXX", "XXX"],
        vec!["XXX", "X X", "X X"],
        vec!["X X", "X X"],
    ];
    for (p, rows) in parts.into_iter().enumerate() {
        v.push(RecipeDef {
            name: "steel_armor",
            shape: Shaped(rows, vec![('X', Item(I_STEEL_INGOT))]),
            out: I_STEEL_HELMET + p as ItemId,
            count: 1,
        });
    }
    v
}

pub fn smelt(item: ItemId) -> Option<ItemId> {
    Some(match item {
        I_RAW_OSMIUM => I_OSMIUM_INGOT,
        I_RAW_TIN => I_TIN_INGOT,
        I_RAW_COPPER => I_COPPER_INGOT,
        I_RAW_GOLD => I_GOLD_INGOT,
        I_DUST_IRON => I_IRON_INGOT,
        I_DUST_STEEL => I_STEEL_INGOT,
        I_DUST_OSMIUM => I_OSMIUM_INGOT,
        I_DUST_COPPER => I_COPPER_INGOT,
        I_DUST_GOLD => I_GOLD_INGOT,
        I_DUST_TIN => I_TIN_INGOT,
        x if (I_STEEL_PICKAXE..I_STEEL_HELMET + 4).contains(&x) => I_STEEL_NUGGET,
        _ => return None,
    })
}

/// Stable table categories for recipe displays; these are not wire discriminants.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum MachineKind {
    Infuse,
    Enrich,
    Crush,
    Purify,
    Smelt,
    ChemicalConversion,
    EnergyConversion,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Product {
    Item { item: ItemId, count: u16 },
    Substance { substance: u8, amount: u32 },
    Energy { joules: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProductKey {
    Item(ItemId),
    Substance(u8),
    Energy,
}

impl Product {
    pub const fn key(self) -> ProductKey {
        match self {
            Self::Item { item, .. } => ProductKey::Item(item),
            Self::Substance { substance, .. } => ProductKey::Substance(substance),
            Self::Energy { .. } => ProductKey::Energy,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReagentUsage {
    PerOperation,
    PerTick,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reagent {
    pub substance: u8,
    pub amount: u32,
    pub usage: ReagentUsage,
}

/// A view of the same executable recipe table for both forward and reverse NEI
/// queries. Energy/timing remains in the machine definition, not this table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MachineRecipe {
    pub kind: MachineKind,
    pub index: u16,
    pub input: ItemId,
    pub input_count: u16,
    pub product: Product,
    pub reagent: Option<Reagent>,
}

pub fn all() -> impl Iterator<Item = MachineRecipe> {
    let infuse = INFUSE
        .iter()
        .enumerate()
        .map(|(index, recipe)| MachineRecipe {
            kind: MachineKind::Infuse,
            index: index as u16,
            input: recipe.input,
            input_count: 1,
            product: Product::Item {
                item: recipe.out,
                count: 1,
            },
            reagent: Some(Reagent {
                substance: recipe.sub,
                amount: recipe.amount,
                usage: ReagentUsage::PerOperation,
            }),
        });
    let convert = |kind, index, recipe: &Enrich| MachineRecipe {
        kind,
        index: index as u16,
        input: recipe.input,
        input_count: recipe.in_n as u16,
        product: Product::Item {
            item: recipe.out,
            count: recipe.out_n as u16,
        },
        reagent: None,
    };
    let enrich = ENRICH
        .iter()
        .enumerate()
        .map(move |(index, recipe)| convert(MachineKind::Enrich, index, recipe));
    let crush = CRUSH
        .iter()
        .enumerate()
        .map(move |(index, recipe)| convert(MachineKind::Crush, index, recipe));
    let purify = PURIFY
        .iter()
        .enumerate()
        .map(|(index, recipe)| MachineRecipe {
            kind: MachineKind::Purify,
            index: index as u16,
            input: recipe.input,
            input_count: 1,
            product: Product::Item {
                item: recipe.out,
                count: recipe.out_n as u16,
            },
            reagent: Some(Reagent {
                substance: SUB_OXYGEN,
                amount: recipe.rate,
                usage: ReagentUsage::PerTick,
            }),
        });
    let smelt = crate::all_items().filter_map(|(input, _)| {
        smelt(input).map(|output| MachineRecipe {
            kind: MachineKind::Smelt,
            index: input,
            input,
            input_count: 1,
            product: Product::Item {
                item: output,
                count: 1,
            },
            reagent: None,
        })
    });
    let chemical = crate::all_items().filter_map(|(input, _)| {
        chem_of(input)
            .or_else(|| {
                let amount = oxygen_of(input);
                (amount != 0).then_some((SUB_OXYGEN, amount))
            })
            .map(|(substance, amount)| MachineRecipe {
                kind: MachineKind::ChemicalConversion,
                index: input,
                input,
                input_count: 1,
                product: Product::Substance { substance, amount },
                reagent: None,
            })
    });
    let energy = crate::all_items().filter_map(|(input, _)| {
        let joules = energy_of(input);
        (joules != 0).then_some(MachineRecipe {
            kind: MachineKind::EnergyConversion,
            index: input,
            input,
            input_count: 1,
            product: Product::Energy { joules },
            reagent: None,
        })
    });
    infuse
        .chain(enrich)
        .chain(crush)
        .chain(purify)
        .chain(smelt)
        .chain(chemical)
        .chain(energy)
}

pub fn recipes_using(input: ItemId) -> impl Iterator<Item = MachineRecipe> {
    all().filter(move |recipe| recipe.input == input)
}

pub fn recipes_producing(output: ProductKey) -> impl Iterator<Item = MachineRecipe> {
    all().filter(move |recipe| recipe.product.key() == output)
}
