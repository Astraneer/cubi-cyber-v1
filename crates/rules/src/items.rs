//! Stable registry data and pure formulas from the SPEC-approved donor.
//! Source: donor/crates/voxel-core/src/registry/items.rs at 3992583213e1f00d95a3beef2bde43ae970192d4.
//! Adapted to the shared rules layer; no donor world or runtime is included.

use crate::blocks::{AXE, BlockDef, HOE, PICKAXE, SHOVEL, bdef, block_def};
use crate::cell::BlockId;
use crate::ids::*;

pub type ItemId = u16;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tier {
    pub level: u8,
    pub speed: f32,
    pub durability: u16,
}
pub const T_WOOD: Tier = Tier {
    level: 0,
    speed: 2.0,
    durability: 59,
};
pub const T_STONE: Tier = Tier {
    level: 1,
    speed: 4.0,
    durability: 131,
};
pub const T_IRON: Tier = Tier {
    level: 2,
    speed: 6.0,
    durability: 250,
};
pub const T_STEEL: Tier = Tier {
    level: 3,
    speed: 8.0,
    durability: 500,
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ItemKind {
    None,
    Plain,
    Block(BlockId),
    Tool { mask: u8, tier: Tier },
    Sword { tier: Tier },
    Shears,
    Armor { slot: u8, points: u8 },
    Food { nutrition: u8 },
    Bucket { fluid: u8 },
    Seed { crop: BlockId, nutrition: u8 },
}

#[derive(Clone, Copy, Debug)]
pub struct ItemDef {
    pub mc: &'static str,
    pub name: &'static str,
    pub max_stack: u8,
    pub max_damage: u16,
    pub kind: ItemKind,
    pub remainder: ItemId,
}

pub const I_STICK: ItemId = 256;
pub const I_COAL: ItemId = 257;
pub const I_CHARCOAL: ItemId = 258;
pub const I_RAW_IRON: ItemId = 259;
pub const I_IRON_INGOT: ItemId = 260;
pub const I_IRON_NUGGET: ItemId = 261;
pub const I_FLINT: ItemId = 262;
pub const I_APPLE: ItemId = 263;
pub const I_WHEAT_SEEDS: ItemId = 264;
pub const I_SNOWBALL: ItemId = 265;
pub const I_CLAY_BALL: ItemId = 266;
pub const I_LEATHER: ItemId = 267;
pub const I_BUCKET: ItemId = 268;
pub const I_WATER_BUCKET: ItemId = 269;
pub const I_LAVA_BUCKET: ItemId = 270;
pub const I_WOODEN_PICKAXE: ItemId = 271; // дальше: топор, лопата, мотыга, меч; каменные; железные
pub const I_LEATHER_HELMET: ItemId = 286; // шлем, нагрудник, поножи, ботинки; кожа, затем железо
pub const I_SHEARS: ItemId = 294;
pub const I_RAW_OSMIUM: ItemId = 295;
pub const I_RAW_TIN: ItemId = 296;
pub const I_RAW_COPPER: ItemId = 297;
pub const I_RAW_GOLD: ItemId = 298;
pub const I_OSMIUM_INGOT: ItemId = 299;
pub const I_TIN_INGOT: ItemId = 300;
pub const I_COPPER_INGOT: ItemId = 301;
pub const I_GOLD_INGOT: ItemId = 302;
pub const I_STEEL_INGOT: ItemId = 303;
pub const I_STEEL_NUGGET: ItemId = 304;
pub const I_REDSTONE: ItemId = 305;
pub const I_DUST_IRON: ItemId = 306;
pub const I_DUST_STEEL: ItemId = 307;
pub const I_ENRICHED_CARBON: ItemId = 308;
pub const I_ENRICHED_REDSTONE: ItemId = 309;
pub const I_ENRICHED_IRON: ItemId = 310;
pub const I_BASIC_CIRCUIT: ItemId = 311;
pub const I_ALLOY_INFUSED: ItemId = 312;
pub const I_ENERGY_TABLET: ItemId = 313;
pub const I_STEEL_PICKAXE: ItemId = 314;
pub const I_STEEL_HELMET: ItemId = 319;
pub const I_DUST_OSMIUM: ItemId = 323;
pub const I_DUST_COPPER: ItemId = 324;
pub const I_DUST_GOLD: ItemId = 325;
pub const I_DUST_TIN: ItemId = 326;
pub const I_CONFIGURATOR: ItemId = 327;
pub const I_UPGRADE_SPEED: ItemId = 328;
pub const I_UPGRADE_ENERGY: ItemId = 329;
pub const I_CLUMP_IRON: ItemId = 340;
pub const I_DIRTY_DUST_IRON: ItemId = 345;
pub const I_ELECTROLYTIC_CORE: ItemId = 350;
pub const I_ADVANCED_CIRCUIT: ItemId = 351;
pub const I_UPGRADE_GAS: ItemId = 352;
pub const I_PORKCHOP: ItemId = 400;
pub const I_COOKED_PORKCHOP: ItemId = 401;
pub const I_BEEF: ItemId = 402;
pub const I_COOKED_BEEF: ItemId = 403;
pub const I_MUTTON: ItemId = 404;
pub const I_COOKED_MUTTON: ItemId = 405;
pub const I_CHICKEN: ItemId = 406;
pub const I_COOKED_CHICKEN: ItemId = 407;
pub const I_FEATHER: ItemId = 408;
pub const I_EGG: ItemId = 409;
pub const I_ROTTEN_FLESH: ItemId = 410;
pub const ITEMS_END: ItemId = 411;

pub const I_DIAMOND: ItemId = 420;
pub const I_GOLDEN_HELMET: ItemId = 421;
pub const I_DIAMOND_HELMET: ItemId = 425;
pub const I_WHEAT: ItemId = 429;
pub const I_BREAD: ItemId = 430;
pub const I_CARROT: ItemId = 431;
pub const I_POTATO: ItemId = 432;
pub const I_BAKED_POTATO: ItemId = 433;
pub const G7_FIRST: ItemId = 420;
pub const G7_END: ItemId = 434;

pub const fn plain(mc: &'static str, name: &'static str, max_stack: u8, kind: ItemKind) -> ItemDef {
    ItemDef {
        mc,
        name,
        max_stack,
        max_damage: 0,
        kind,
        remainder: 0,
    }
}
pub const fn tool(mc: &'static str, name: &'static str, kind: ItemKind, tier: Tier) -> ItemDef {
    ItemDef {
        mc,
        name,
        max_stack: 1,
        max_damage: tier.durability,
        kind,
        remainder: 0,
    }
}
pub const fn armor(
    mc: &'static str,
    name: &'static str,
    slot: u8,
    points: u8,
    durability: u16,
) -> ItemDef {
    ItemDef {
        mc,
        name,
        max_stack: 1,
        max_damage: durability,
        kind: ItemKind::Armor { slot, points },
        remainder: 0,
    }
}

pub const NO_ITEM: ItemDef = ItemDef {
    mc: "air",
    name: "",
    max_stack: 64,
    max_damage: 0,
    kind: ItemKind::None,
    remainder: 0,
};

pub const fn block_item_name(b: BlockId) -> &'static str {
    match b {
        GRASS => "Блок травы",
        DIRT => "Земля",
        STONE => "Камень",
        SAND => "Песок",
        GRAVEL => "Гравий",
        SNOW => "Снежный блок",
        SANDSTONE => "Песчаник",
        CLAY => "Глина",
        LOG => "Дубовое бревно",
        LEAVES => "Дубовая листва",
        BIRCH_LOG => "Берёзовое бревно",
        BIRCH_LEAVES => "Берёзовая листва",
        SPRUCE_LOG => "Еловое бревно",
        SPRUCE_LEAVES => "Еловая листва",
        CACTUS => "Кактус",
        TALL_GRASS => "Трава",
        DANDELION => "Одуванчик",
        POPPY => "Мак",
        DEAD_BUSH => "Мёртвый куст",
        COBBLE => "Булыжник",
        BEDROCK => "Бедрок",
        FERN => "Папоротник",
        PLANKS => "Дубовые доски",
        BIRCH_PLANKS => "Берёзовые доски",
        SPRUCE_PLANKS => "Еловые доски",
        CRAFTING_TABLE => "Верстак",
        FURNACE => "Печь",
        COAL_ORE => "Угольная руда",
        IRON_ORE => "Железная руда",
        TORCH => "Факел",
        GLASS => "Стекло",
        IRON_BLOCK => "Железный блок",
        RAW_IRON_BLOCK => "Блок сырого железа",
        COAL_BLOCK => "Угольный блок",
        CHEST => "Сундук",
        LADDER => "Лестница",
        SNOW_LAYER => "Снег",
        OSMIUM_ORE => "Осмиевая руда",
        TIN_ORE => "Оловянная руда",
        COPPER_ORE => "Медная руда",
        GOLD_ORE => "Золотая руда",
        REDSTONE_ORE => "Редстоуновая руда",
        STEEL_CASING => "Стальной корпус",
        HEAT_GENERATOR => "Тепловой генератор",
        METALLURGIC_INFUSER => "Металлургический инфузор",
        UNIVERSAL_CABLE => "Универсальный кабель",
        ENERGY_CUBE => "Базовый куб энергии",
        STEEL_BLOCK => "Стальной блок",
        OSMIUM_BLOCK => "Осмиевый блок",
        REDSTONE_BLOCK => "Блок редстоуна",
        OBSIDIAN => "Обсидиан",
        HAY_BLOCK => "Стог сена",
        ENRICHMENT_CHAMBER => "Обогатительная камера",
        ENERGIZED_SMELTER => "Плавильня с энергией",
        WIND_GENERATOR => "Ветряной генератор",
        CRUSHER => "Дробилка",
        PURIFICATION_CHAMBER => "Очистительная камера",
        ELECTROLYTIC_SEPARATOR => "Электролитический сепаратор",
        LOGISTICAL_TRANSPORTER => "Логистический транспортёр",
        MECHANICAL_PIPE => "Механическая труба",
        PRESSURIZED_TUBE => "Напорная трубка",
        ELECTRIC_PUMP => "Электрический насос",
        WHITE_WOOL => "Белая шерсть",
        BLACK_WOOL => "Чёрная шерсть",
        GRAY_WOOL => "Серая шерсть",
        LIGHT_GRAY_WOOL => "Светло-серая шерсть",
        BROWN_WOOL => "Коричневая шерсть",
        PINK_WOOL => "Розовая шерсть",
        _ => "",
    }
}

pub const fn item_def(id: ItemId) -> ItemDef {
    use ItemKind::*;
    if id >= 1 && id < 256 {
        let b = id as BlockId;
        let name = block_item_name(b);
        if name.is_empty() {
            return NO_ITEM;
        }
        return ItemDef {
            mc: block_def(b).mc,
            name,
            max_stack: 64,
            max_damage: 0,
            kind: Block(b),
            remainder: 0,
        };
    }
    let tools = [
        (PICKAXE, "pickaxe", "кирка"),
        (AXE, "axe", "топор"),
        (SHOVEL, "shovel", "лопата"),
        (HOE, "hoe", "мотыга"),
        (0, "sword", "меч"),
    ];
    match id {
        I_STICK => plain("stick", "Палка", 64, Plain),
        I_COAL => plain("coal", "Уголь", 64, Plain),
        I_CHARCOAL => plain("charcoal", "Древесный уголь", 64, Plain),
        I_RAW_IRON => plain("raw_iron", "Сырое железо", 64, Plain),
        I_IRON_INGOT => plain("iron_ingot", "Железный слиток", 64, Plain),
        I_IRON_NUGGET => plain("iron_nugget", "Железный самородок", 64, Plain),
        I_FLINT => plain("flint", "Кремень", 64, Plain),
        I_APPLE => plain("apple", "Яблоко", 64, Food { nutrition: 4 }),
        I_WHEAT_SEEDS => plain(
            "wheat_seeds",
            "Семена пшеницы",
            64,
            Seed {
                crop: WHEAT,
                nutrition: 0,
            },
        ),
        I_DIAMOND => plain("diamond", "Алмаз", 64, Plain),
        id if id >= I_GOLDEN_HELMET && id < I_GOLDEN_HELMET + 8 => {
            let k = (id - I_GOLDEN_HELMET) as usize;
            let diamond = k >= 4;
            let part = k % 4; // шлем, нагрудник, поножи, ботинки
            let base = [11u16, 16, 15, 13][part];
            let points = if diamond {
                [3u8, 8, 6, 3][part]
            } else {
                [2u8, 5, 3, 1][part]
            };
            let (mc, name) = G7_ARMOR_NAMES[k];
            armor(
                mc,
                name,
                3 - part as u8,
                points,
                base * if diamond { 33 } else { 7 },
            )
        }
        I_WHEAT => plain("wheat", "Пшеница", 64, Plain),
        I_BREAD => plain("bread", "Хлеб", 64, Food { nutrition: 5 }),
        I_CARROT => plain(
            "carrot",
            "Морковь",
            64,
            Seed {
                crop: CARROTS,
                nutrition: 3,
            },
        ),
        I_POTATO => plain(
            "potato",
            "Картофель",
            64,
            Seed {
                crop: POTATOES,
                nutrition: 1,
            },
        ),
        I_BAKED_POTATO => plain(
            "baked_potato",
            "Печёный картофель",
            64,
            Food { nutrition: 5 },
        ),
        I_SNOWBALL => plain("snowball", "Снежок", 16, Plain),
        I_CLAY_BALL => plain("clay_ball", "Комок глины", 64, Plain),
        I_LEATHER => plain("leather", "Кожа", 64, Plain),
        I_BUCKET => plain("bucket", "Ведро", 16, Bucket { fluid: 0 }),
        I_WATER_BUCKET => {
            let mut d = plain("water_bucket", "Ведро воды", 1, Bucket { fluid: 1 });
            d.remainder = I_BUCKET;
            d
        }
        I_LAVA_BUCKET => {
            let mut d = plain("lava_bucket", "Ведро лавы", 1, Bucket { fluid: 2 });
            d.remainder = I_BUCKET;
            d
        }
        id if id >= I_WOODEN_PICKAXE && id < I_WOODEN_PICKAXE + 15 => {
            let k = (id - I_WOODEN_PICKAXE) as usize;
            let (tier, mc_t) = match k / 5 {
                0 => (T_WOOD, 0),
                1 => (T_STONE, 1),
                _ => (T_IRON, 2),
            };
            let (mask, _, _) = tools[k % 5];
            let kind = if mask == 0 {
                Sword { tier }
            } else {
                Tool { mask, tier }
            };
            let (mc, name) = TOOL_NAMES[mc_t * 5 + k % 5];
            tool(mc, name, kind, tier)
        }
        I_SHEARS => ItemDef {
            mc: "shears",
            name: "Ножницы",
            max_stack: 1,
            max_damage: 238,
            kind: Shears,
            remainder: 0,
        },
        id if id >= I_PORKCHOP && id < I_PORKCHOP + MOB_ITEMS.len() as ItemId => {
            let (mc, name, stack, food) = MOB_ITEMS[(id - I_PORKCHOP) as usize];
            plain(
                mc,
                name,
                stack,
                if food > 0 {
                    Food { nutrition: food }
                } else {
                    Plain
                },
            )
        }
        I_ENERGY_TABLET => plain("energy_tablet", "Планшет энергии", 1, Plain),
        I_CONFIGURATOR => plain("configurator", "Конфигуратор", 1, Plain),
        I_UPGRADE_SPEED => plain("upgrade_speed", "Улучшение скорости", 64, Plain),
        I_UPGRADE_ENERGY => plain("upgrade_energy", "Улучшение энергии", 64, Plain),
        I_UPGRADE_GAS => plain("upgrade_gas", "Улучшение газа", 64, Plain),
        I_ELECTROLYTIC_CORE => plain("electrolytic_core", "Электролитическое ядро", 64, Plain),
        I_ADVANCED_CIRCUIT => plain("advanced_control_circuit", "Продвинутая схема", 64, Plain),
        id if id >= I_CLUMP_IRON && id < I_CLUMP_IRON + 10 => {
            let (mc, name) = MEK4_ITEMS[(id - I_CLUMP_IRON) as usize];
            plain(mc, name, 64, Plain)
        }
        id if id >= I_RAW_OSMIUM && id < I_STEEL_PICKAXE => {
            let (mc, name) = MEK_ITEMS[(id - I_RAW_OSMIUM) as usize];
            plain(mc, name, 64, Plain)
        }
        id if id >= I_STEEL_PICKAXE && id < I_STEEL_PICKAXE + 5 => {
            let k = (id - I_STEEL_PICKAXE) as usize;
            let (mask, _, _) = tools[k];
            let kind = if mask == 0 {
                Sword { tier: T_STEEL }
            } else {
                Tool {
                    mask,
                    tier: T_STEEL,
                }
            };
            let (mc, name) = STEEL_TOOL_NAMES[k];
            tool(mc, name, kind, T_STEEL)
        }
        id if id >= I_STEEL_HELMET && id < I_STEEL_HELMET + 4 => {
            let part = (id - I_STEEL_HELMET) as usize;
            let (mc, name) = STEEL_ARMOR_NAMES[part];
            armor(
                mc,
                name,
                3 - part as u8,
                [3u8, 8, 6, 3][part],
                [220u16, 320, 300, 260][part],
            )
        }
        id if id >= I_DUST_OSMIUM && id < I_DUST_OSMIUM + 4 => {
            let (mc, name) = MEK2_ITEMS[(id - I_DUST_OSMIUM) as usize];
            plain(mc, name, 64, Plain)
        }
        id if id >= I_LEATHER_HELMET && id < I_LEATHER_HELMET + 8 => {
            let k = (id - I_LEATHER_HELMET) as usize;
            let iron = k >= 4;
            let part = k % 4; // шлем, нагрудник, поножи, ботинки
            let base = [11u16, 16, 15, 13][part];
            let points = if iron {
                [2u8, 6, 5, 2][part]
            } else {
                [1u8, 3, 2, 1][part]
            };
            let (mc, name) = ARMOR_NAMES[k];
            armor(
                mc,
                name,
                3 - part as u8,
                points,
                base * if iron { 15 } else { 5 },
            )
        }
        _ => NO_ITEM,
    }
}

pub const TOOL_NAMES: [(&str, &str); 15] = [
    ("wooden_pickaxe", "Деревянная кирка"),
    ("wooden_axe", "Деревянный топор"),
    ("wooden_shovel", "Деревянная лопата"),
    ("wooden_hoe", "Деревянная мотыга"),
    ("wooden_sword", "Деревянный меч"),
    ("stone_pickaxe", "Каменная кирка"),
    ("stone_axe", "Каменный топор"),
    ("stone_shovel", "Каменная лопата"),
    ("stone_hoe", "Каменная мотыга"),
    ("stone_sword", "Каменный меч"),
    ("iron_pickaxe", "Железная кирка"),
    ("iron_axe", "Железный топор"),
    ("iron_shovel", "Железная лопата"),
    ("iron_hoe", "Железная мотыга"),
    ("iron_sword", "Железный меч"),
];
pub const G7_ARMOR_NAMES: [(&str, &str); 8] = [
    ("golden_helmet", "Золотой шлем"),
    ("golden_chestplate", "Золотой нагрудник"),
    ("golden_leggings", "Золотые поножи"),
    ("golden_boots", "Золотые ботинки"),
    ("diamond_helmet", "Алмазный шлем"),
    ("diamond_chestplate", "Алмазный нагрудник"),
    ("diamond_leggings", "Алмазные поножи"),
    ("diamond_boots", "Алмазные ботинки"),
];
pub const ARMOR_NAMES: [(&str, &str); 8] = [
    ("leather_helmet", "Кожаный шлем"),
    ("leather_chestplate", "Кожаная куртка"),
    ("leather_leggings", "Кожаные штаны"),
    ("leather_boots", "Кожаные ботинки"),
    ("iron_helmet", "Железный шлем"),
    ("iron_chestplate", "Железный нагрудник"),
    ("iron_leggings", "Железные поножи"),
    ("iron_boots", "Железные ботинки"),
];

pub const MOB_ITEMS: [(&str, &str, u8, u8); 11] = [
    ("porkchop", "Сырая свинина", 64, 3),
    ("cooked_porkchop", "Жареная свинина", 64, 8),
    ("beef", "Сырая говядина", 64, 3),
    ("cooked_beef", "Стейк", 64, 8),
    ("mutton", "Сырая баранина", 64, 2),
    ("cooked_mutton", "Жареная баранина", 64, 6),
    ("chicken", "Сырая курица", 64, 2),
    ("cooked_chicken", "Жареная курица", 64, 6),
    ("feather", "Перо", 64, 0),
    ("egg", "Яйцо", 16, 0),
    ("rotten_flesh", "Гнилая плоть", 64, 4),
];

pub const MEK_ITEMS: [(&str, &str); 19] = [
    ("raw_osmium", "Сырой осмий"),
    ("raw_tin", "Сырое олово"),
    ("raw_copper", "Сырая медь"),
    ("raw_gold", "Сырое золото"),
    ("osmium_ingot", "Слиток осмия"),
    ("tin_ingot", "Слиток олова"),
    ("copper_ingot", "Медный слиток"),
    ("gold_ingot", "Золотой слиток"),
    ("steel_ingot", "Стальной слиток"),
    ("steel_nugget", "Стальной самородок"),
    ("redstone", "Редстоун"),
    ("dust_iron", "Железная пыль"),
    ("dust_steel", "Стальная пыль"),
    ("enriched_carbon", "Обогащённый уголь"),
    ("enriched_redstone", "Обогащённый редстоун"),
    ("enriched_iron", "Обогащённое железо"),
    ("basic_control_circuit", "Базовая схема управления"),
    ("alloy_infused", "Пропитанный сплав"),
    ("energy_tablet", "Планшет энергии"),
];
pub const MEK2_ITEMS: [(&str, &str); 4] = [
    ("dust_osmium", "Осмиевая пыль"),
    ("dust_copper", "Медная пыль"),
    ("dust_gold", "Золотая пыль"),
    ("dust_tin", "Оловянная пыль"),
];
pub const MEK4_ITEMS: [(&str, &str); 10] = [
    ("clump_iron", "Железный комок"),
    ("clump_osmium", "Осмиевый комок"),
    ("clump_copper", "Медный комок"),
    ("clump_gold", "Золотой комок"),
    ("clump_tin", "Оловянный комок"),
    ("dirty_dust_iron", "Грязная железная пыль"),
    ("dirty_dust_osmium", "Грязная осмиевая пыль"),
    ("dirty_dust_copper", "Грязная медная пыль"),
    ("dirty_dust_gold", "Грязная золотая пыль"),
    ("dirty_dust_tin", "Грязная оловянная пыль"),
];
pub const STEEL_TOOL_NAMES: [(&str, &str); 5] = [
    ("steel_pickaxe", "Стальная кирка"),
    ("steel_axe", "Стальной топор"),
    ("steel_shovel", "Стальная лопата"),
    ("steel_hoe", "Стальная мотыга"),
    ("steel_sword", "Стальной меч"),
];
pub const STEEL_ARMOR_NAMES: [(&str, &str); 4] = [
    ("steel_helmet", "Стальной шлем"),
    ("steel_chestplate", "Стальной нагрудник"),
    ("steel_leggings", "Стальные поножи"),
    ("steel_boots", "Стальные ботинки"),
];

pub fn armor_toughness(id: ItemId) -> f32 {
    if (I_STEEL_HELMET..I_STEEL_HELMET + 4).contains(&id)
        || (I_DIAMOND_HELMET..I_DIAMOND_HELMET + 4).contains(&id)
    {
        2.0
    } else {
        0.0
    }
}

pub const fn build_items() -> [ItemDef; ITEMS_END as usize] {
    let mut t = [NO_ITEM; ITEMS_END as usize];
    let mut i = 0;
    while i < ITEMS_END as usize {
        t[i] = item_def(i as ItemId);
        i += 1;
    }
    t
}

pub static ITEMS: [ItemDef; ITEMS_END as usize] = build_items();

const fn build_g7_items() -> [ItemDef; (G7_END - G7_FIRST) as usize] {
    let mut t = [NO_ITEM; (G7_END - G7_FIRST) as usize];
    let mut i = 0;
    while i < t.len() {
        t[i] = item_def(G7_FIRST + i as ItemId);
        i += 1;
    }
    t
}

pub static G7_ITEMS: [ItemDef; (G7_END - G7_FIRST) as usize] = build_g7_items();

pub fn idef(id: ItemId) -> &'static ItemDef {
    if id >= G7_FIRST {
        return G7_ITEMS.get((id - G7_FIRST) as usize).unwrap_or(&NO_ITEM);
    }
    ITEMS.get(id as usize).unwrap_or(&NO_ITEM)
}

pub fn food_nutrition(id: ItemId) -> Option<u8> {
    match idef(id).kind {
        ItemKind::Food { nutrition } => Some(nutrition),
        ItemKind::Seed { nutrition, .. } if nutrition > 0 => Some(nutrition),
        _ => None,
    }
}

pub fn equip_slot(id: ItemId) -> Option<u8> {
    match idef(id).kind {
        ItemKind::Armor { slot, .. } => Some(slot),
        _ => None,
    }
}

pub fn tool_attack(id: ItemId) -> Option<(f32, f32)> {
    if (I_STEEL_PICKAXE..I_STEEL_PICKAXE + 5).contains(&id) {
        let k = (id - I_STEEL_PICKAXE) as usize;
        return Some(([5.0, 11.0, 5.5, 1.0, 7.0][k], [1.2, 1.0, 1.0, 4.0, 1.6][k]));
    }
    if !(I_WOODEN_PICKAXE..I_WOODEN_PICKAXE + 15).contains(&id) {
        return None;
    }
    let k = (id - I_WOODEN_PICKAXE) as usize;
    let (tier, kind) = (k / 5, k % 5); // ярус: дерево, камень, железо; вид: кирка, топор, лопата, мотыга, меч
    let dmg = [
        [2.0, 3.0, 4.0],
        [7.0, 9.0, 9.0],
        [2.5, 3.5, 4.5],
        [1.0, 1.0, 1.0],
        [4.0, 5.0, 6.0],
    ][kind][tier];
    let speed = [
        [1.2, 1.2, 1.2],
        [0.8, 0.8, 0.9],
        [1.0, 1.0, 1.0],
        [1.0, 2.0, 3.0],
        [1.6, 1.6, 1.6],
    ][kind][tier];
    Some((dmg, speed))
}

pub fn block_item(b: BlockId) -> ItemId {
    let base = bdef(b).base;
    if matches!(idef(base as ItemId).kind, ItemKind::Block(_)) {
        base as ItemId
    } else {
        0
    }
}

pub fn tool_speed(item: ItemId, b: &BlockDef) -> (f32, bool) {
    match idef(item).kind {
        ItemKind::Tool { mask, tier } if b.tool & mask != 0 => (tier.speed, b.needs <= tier.level),
        ItemKind::Sword { .. } if b.sword_fast => (1.5, false),
        ItemKind::Shears if b.leaves => (15.0, false),
        ItemKind::Shears if (WHITE_WOOL..=PINK_WOOL).contains(&b.base) => (5.0, false),
        _ => (1.0, false),
    }
}

pub fn can_harvest(item: ItemId, b: &BlockDef) -> bool {
    !b.requires_tool || tool_speed(item, b).1
}

pub fn break_delta(item: ItemId, b: &BlockDef, on_ground: bool, eyes_in_water: bool) -> f32 {
    if b.hardness == -1.0 {
        return 0.0;
    }
    let (mut speed, _) = tool_speed(item, b);
    if eyes_in_water {
        speed *= 0.2;
    }
    if !on_ground {
        speed /= 5.0;
    }
    let div: f32 = if can_harvest(item, b) { 30.0 } else { 100.0 };
    speed / b.hardness / div // твёрдость 0 → +inf → мгновенно
}

pub fn item_by_mc(name: &str) -> ItemId {
    (1..ITEMS_END)
        .chain(G7_FIRST..G7_END)
        .find(|&i| idef(i).kind != ItemKind::None && idef(i).mc == name)
        .unwrap_or(0)
}
