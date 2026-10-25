//! Рецепты машин таблицей (E21, MEK-04 §2.1, §1.6; MEK-01 §3.3): инфузия, превращение предмета в вещество, энергия
//! редстоуна. Расход энергии и время — в описании машины (MekDef), не в рецепте (решение после MEK-01).

use crate::cell::BlockId;
use crate::registry::ids::{CLAY, COAL_BLOCK, COBBLE, GRAVEL, RAW_IRON_BLOCK, REDSTONE_BLOCK, SAND, SANDSTONE, STONE};
use crate::registry::items::*;

/// Вещество бака: нет, углерод, редстоун (инфузия, MEK-04 §1.1), лава (бак теплового генератора, MEK-03 §2.3).
pub(crate) const SUB_NONE: u8 = 0;
pub(crate) const SUB_CARBON: u8 = 1;
pub(crate) const SUB_REDSTONE: u8 = 2;
pub(crate) const SUB_LAVA: u8 = 3;
/// M4 (MEK-11 §1.1): газы сепаратора и очистителя, вода в баке сепаратора.
pub(crate) const SUB_OXYGEN: u8 = 4;
pub(crate) const SUB_HYDROGEN: u8 = 5;
pub(crate) const SUB_WATER: u8 = 6;

/// Имя вещества для подсказки бака.
pub(crate) fn sub_name(s: u8) -> &'static str {
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
/// Цвет вещества в окне (MEK-04 §1.1; лава — своя).
pub(crate) fn sub_color(s: u8) -> u32 {
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

/// Рецепт инфузора: вход ×1 + вещество «на операцию» (тратится в конце, per_tick_usage: false) → выход ×1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Infuse {
    pub(crate) input: ItemId,
    pub(crate) sub: u8,
    pub(crate) amount: u32,
    pub(crate) out: ItemId,
}

/// Рецепты инфузора объёма M (MEK-04 §2.1; продвинутая схема — вместе с крафтом схем, M2).
pub(crate) const INFUSE: [Infuse; 6] = [
    Infuse { input: I_IRON_INGOT, sub: SUB_CARBON, amount: 10, out: I_ENRICHED_IRON },
    Infuse { input: I_DUST_IRON, sub: SUB_CARBON, amount: 10, out: I_ENRICHED_IRON },
    Infuse { input: I_ENRICHED_IRON, sub: SUB_CARBON, amount: 10, out: I_DUST_STEEL },
    Infuse { input: I_OSMIUM_INGOT, sub: SUB_REDSTONE, amount: 20, out: I_BASIC_CIRCUIT },
    Infuse { input: I_COPPER_INGOT, sub: SUB_REDSTONE, amount: 10, out: I_ALLOY_INFUSED },
    // M4: вторая дорога к продвинутой схеме (control_circuit/infused_advanced), первая — крафт.
    Infuse { input: I_BASIC_CIRCUIT, sub: SUB_REDSTONE, amount: 60, out: I_ADVANCED_CIRCUIT },
];

/// Есть ли у предмета рецепт инфузии (слот входа).
pub(crate) fn infusable(item: ItemId) -> bool {
    INFUSE.iter().any(|r| r.input == item)
}
/// Первый рецепт для входа и вещества в баке (с количеством).
pub(crate) fn find_infuse(item: ItemId, sub: u8, amount: u32) -> Option<u8> {
    INFUSE.iter().position(|r| r.input == item && r.sub == sub && amount >= r.amount).map(|i| i as u8)
}

/// Рецепт «предмет → предмет» камеры (MEK-06 §3.1): вход ×in_n → выход ×out_n; время и энергия — в описании машины.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Enrich {
    pub(crate) input: ItemId,
    pub(crate) in_n: i32,
    pub(crate) out: ItemId,
    pub(crate) out_n: i32,
}

const fn en(input: ItemId, in_n: i32, out: ItemId, out_n: i32) -> Enrich {
    Enrich { input, in_n, out, out_n }
}

/// Рецепты обогатительной камеры объёма M (MEK-06 §3.1, решения после MEK-04/06/10/11: блок сырья и удобные рецепты
/// берём; грязная пыль — с очистителем M4; блоки сырого осмия, меди, золота — когда появятся в мире).
pub(crate) const ENRICH: [Enrich; 17] = [
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
    // M4: грязная пыль → пыль (processing/*/dust/from_dirty_dust).
    en(I_DIRTY_DUST_IRON, 1, I_DUST_IRON, 1),
    en(I_DIRTY_DUST_IRON + 1, 1, I_DUST_OSMIUM, 1),
    en(I_DIRTY_DUST_IRON + 2, 1, I_DUST_COPPER, 1),
    en(I_DIRTY_DUST_IRON + 3, 1, I_DUST_GOLD, 1),
    en(I_DIRTY_DUST_IRON + 4, 1, I_DUST_TIN, 1),
];

/// Металлы удвоения по порядку комков и грязной пыли (железо, осмий, медь, золото, олово): сырьё, пыль, слиток.
pub(crate) const METALS: [(ItemId, ItemId, ItemId); 5] = [
    (I_RAW_IRON, I_DUST_IRON, I_IRON_INGOT),
    (I_RAW_OSMIUM, I_DUST_OSMIUM, I_OSMIUM_INGOT),
    (I_RAW_COPPER, I_DUST_COPPER, I_COPPER_INGOT),
    (I_RAW_GOLD, I_DUST_GOLD, I_GOLD_INGOT),
    (I_RAW_TIN, I_DUST_TIN, I_TIN_INGOT),
];

/// Рецепты дробилки объёма M (MEK-06 §3.2): камень → булыжник → гравий → песок, песчаник → 2 песка, слитки → пыль,
/// комки → грязная пыль (×2). Та же строка «вход ×n → выход ×m», что у камеры.
pub(crate) const CRUSH: [Enrich; 15] = [
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

/// Рецепт дробилки для предмета (без количества): номер в CRUSH.
pub(crate) fn find_crush(item: ItemId) -> Option<u8> {
    CRUSH.iter().position(|r| r.input == item).map(|i| i as u8)
}

/// Рецепт очистителя (MEK-11 §4.1.1): вход ×1 + кислород rate мБ на тик (per_tick_usage: true) → комок ×out_n.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Purify {
    pub(crate) input: ItemId,
    pub(crate) rate: u32,
    pub(crate) out: ItemId,
    pub(crate) out_n: i32,
}

/// Рецепты очистителя объёма M: сырьё → 2 комка (1 мБ/т), блок сырого железа → 18 комков (2 мБ/т).
pub(crate) const PURIFY: [Purify; 6] = [
    Purify { input: I_RAW_IRON, rate: 1, out: I_CLUMP_IRON, out_n: 2 },
    Purify { input: I_RAW_OSMIUM, rate: 1, out: I_CLUMP_IRON + 1, out_n: 2 },
    Purify { input: I_RAW_COPPER, rate: 1, out: I_CLUMP_IRON + 2, out_n: 2 },
    Purify { input: I_RAW_GOLD, rate: 1, out: I_CLUMP_IRON + 3, out_n: 2 },
    Purify { input: I_RAW_TIN, rate: 1, out: I_CLUMP_IRON + 4, out_n: 2 },
    Purify { input: RAW_IRON_BLOCK as ItemId, rate: 2, out: I_CLUMP_IRON, out_n: 18 },
];

/// Рецепт очистителя для предмета: номер в PURIFY.
pub(crate) fn find_purify(item: ItemId) -> Option<u8> {
    PURIFY.iter().position(|r| r.input == item).map(|i| i as u8)
}

/// Рецепт камеры для предмета (без количества): номер в ENRICH.
pub(crate) fn find_enrich(item: ItemId) -> Option<u8> {
    ENRICH.iter().position(|r| r.input == item).map(|i| i as u8)
}

/// Кислород из предмета в газ-слоте очистителя (chemical_conversion/flint_to_oxygen): кремень — 10 мБ.
pub(crate) fn oxygen_of(item: ItemId) -> u32 {
    if item == I_FLINT {
        10
    } else {
        0
    }
}

/// Предмет → вещество (chemical_conversion, MEK-04 §1.6): (вещество, мБ); None — не превращается.
pub(crate) fn chem_of(item: ItemId) -> Option<(u8, u32)> {
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

/// Энергия предмета в слоте энергии (energy_conversion, MEK-01 §3.3): пыль 10 000 Дж, блок 90 000; 0 — нет.
pub(crate) fn energy_of(item: ItemId) -> u64 {
    if item == I_REDSTONE {
        10_000
    } else if item == REDSTONE_BLOCK as ItemId {
        90_000
    } else {
        0
    }
}

/// Крафт объёма M1 (MEK-05 §3.4, MEK-07 §4.2, catalog EN-02, MCH-01, RES-06, EN-08, EN-06): узоры как в данных модов.
/// «Базовый сплав» в рецептах Mekanism — это редстоун (RES-05). Ручки стальных инструментов — железные слитки.
pub(crate) fn crafting() -> Vec<crate::game::crafting::RecipeDef> {
    use crate::game::crafting::{Ing::*, RShape::*, RecipeDef, TAG_PLANKS};
    use crate::registry::ids::*;
    let i = |b: BlockId| b as ItemId;
    let mut v = vec![
        RecipeDef { name: "heat_generator", shape: Shaped(vec!["III", "WOW", "CFC"], vec![('I', Item(I_IRON_INGOT)), ('W', Set(TAG_PLANKS)), ('O', Item(I_OSMIUM_INGOT)), ('C', Item(I_COPPER_INGOT)), ('F', Item(i(FURNACE)))]), out: i(HEAT_GENERATOR), count: 1 },
        RecipeDef { name: "metallurgic_infuser", shape: Shaped(vec!["IFI", "ROR", "IFI"], vec![('I', Item(I_IRON_INGOT)), ('F', Item(i(FURNACE))), ('R', Item(I_REDSTONE)), ('O', Item(I_OSMIUM_INGOT))]), out: i(METALLURGIC_INFUSER), count: 1 },
        RecipeDef { name: "basic_universal_cable", shape: Shaped(vec!["SRS"], vec![('S', Item(I_STEEL_INGOT)), ('R', Item(I_REDSTONE))]), out: i(UNIVERSAL_CABLE), count: 8 },
        RecipeDef { name: "steel_casing", shape: Shaped(vec!["SGS", "GOG", "SGS"], vec![('S', Item(I_STEEL_INGOT)), ('G', Item(i(GLASS))), ('O', Item(I_OSMIUM_INGOT))]), out: i(STEEL_CASING), count: 1 },
        RecipeDef { name: "energy_tablet", shape: Shaped(vec!["RGR", "AGA", "RGR"], vec![('R', Item(I_REDSTONE)), ('G', Item(I_GOLD_INGOT)), ('A', Item(I_ALLOY_INFUSED))]), out: I_ENERGY_TABLET, count: 1 },
        RecipeDef { name: "basic_energy_cube", shape: Shaped(vec!["RTR", "ICI", "RTR"], vec![('R', Item(I_REDSTONE)), ('T', Item(I_ENERGY_TABLET)), ('I', Item(I_IRON_INGOT)), ('C', Item(i(STEEL_CASING)))]), out: i(ENERGY_CUBE), count: 1 },
        // EN-04 (M3): осмий ×3, пропитанный сплав, планшет энергии ×2, базовая схема.
        RecipeDef { name: "wind_generator", shape: Shaped(vec![" O ", "OAO", "TCT"], vec![('O', Item(I_OSMIUM_INGOT)), ('A', Item(I_ALLOY_INFUSED)), ('T', Item(I_ENERGY_TABLET)), ('C', Item(I_BASIC_CIRCUIT))]), out: i(WIND_GENERATOR), count: 1 },
        // M3: конфигуратор — железо ×2, осмий, сталь (MEK-09 §0 п. 13; раскладка наша); улучшения (MEK-10 §5.4) — стекло
        // сверху и снизу, пропитанный сплав по бокам, в центре пыль осмия (скорость) или золота (энергия).
        RecipeDef { name: "configurator", shape: Shaped(vec!["I O", " S ", " I "], vec![('I', Item(I_IRON_INGOT)), ('O', Item(I_OSMIUM_INGOT)), ('S', Item(I_STEEL_INGOT))]), out: I_CONFIGURATOR, count: 1 },
        RecipeDef { name: "upgrade_speed", shape: Shaped(vec![" G ", "ADA", " G "], vec![('G', Item(i(GLASS))), ('A', Item(I_ALLOY_INFUSED)), ('D', Item(I_DUST_OSMIUM))]), out: I_UPGRADE_SPEED, count: 1 },
        RecipeDef { name: "upgrade_energy", shape: Shaped(vec![" G ", "ADA", " G "], vec![('G', Item(i(GLASS))), ('A', Item(I_ALLOY_INFUSED)), ('D', Item(I_DUST_GOLD))]), out: I_UPGRADE_ENERGY, count: 1 },
        // MCH-02, MCH-03 (M2): базовый сплав (редстоун) ×4, базовая схема ×2, корпус в центре; камера — железо по бокам,
        // плавильня — стекло.
        RecipeDef { name: "enrichment_chamber", shape: Shaped(vec!["RCR", "ISI", "RCR"], vec![('R', Item(I_REDSTONE)), ('C', Item(I_BASIC_CIRCUIT)), ('I', Item(I_IRON_INGOT)), ('S', Item(i(STEEL_CASING)))]), out: i(ENRICHMENT_CHAMBER), count: 1 },
        RecipeDef { name: "energized_smelter", shape: Shaped(vec!["RCR", "GSG", "RCR"], vec![('R', Item(I_REDSTONE)), ('C', Item(I_BASIC_CIRCUIT)), ('G', Item(i(GLASS))), ('S', Item(i(STEEL_CASING)))]), out: i(ENERGIZED_SMELTER), count: 1 },
        // M4 (MCH-04…06, MEK-10 §5.4; раскладки — наши, состав — как в данных мода): дробилка — 2 ведра лавы (вёдра
        // возвращаются по ванильному остатку); очиститель — вокруг обогатительной камеры (её улучшения не переносим);
        // сепаратор — вокруг электролитического ядра; продвинутая схема — схема между двумя сплавами.
        RecipeDef { name: "crusher", shape: Shaped(vec!["RCR", "LSL", "RCR"], vec![('R', Item(I_REDSTONE)), ('C', Item(I_BASIC_CIRCUIT)), ('L', Item(I_LAVA_BUCKET)), ('S', Item(i(STEEL_CASING)))]), out: i(CRUSHER), count: 1 },
        RecipeDef { name: "advanced_control_circuit", shape: Shaped(vec!["ACA"], vec![('A', Item(I_ALLOY_INFUSED)), ('C', Item(I_BASIC_CIRCUIT))]), out: I_ADVANCED_CIRCUIT, count: 1 },
        RecipeDef { name: "purification_chamber", shape: Shaped(vec!["ACA", "OEO", "ACA"], vec![('A', Item(I_ALLOY_INFUSED)), ('C', Item(I_ADVANCED_CIRCUIT)), ('O', Item(I_OSMIUM_INGOT)), ('E', Item(i(ENRICHMENT_CHAMBER)))]), out: i(PURIFICATION_CHAMBER), count: 1 },
        RecipeDef { name: "electrolytic_core", shape: Shaped(vec!["AOA", "IAG", "AOA"], vec![('A', Item(I_ALLOY_INFUSED)), ('O', Item(I_DUST_OSMIUM)), ('I', Item(I_DUST_IRON)), ('G', Item(I_DUST_GOLD))]), out: I_ELECTROLYTIC_CORE, count: 1 },
        RecipeDef { name: "electrolytic_separator", shape: Shaped(vec!["IRI", "ACA", "IRI"], vec![('I', Item(I_IRON_INGOT)), ('R', Item(I_REDSTONE)), ('A', Item(I_ALLOY_INFUSED)), ('C', Item(I_ELECTROLYTIC_CORE))]), out: i(ELECTROLYTIC_SEPARATOR), count: 1 },
        // M5 (MEK-14 §2, §11.4; data recipe/transmitter/*/basic.json, electric_pump.json): передатчики — сталь, центр,
        // сталь → 8 (транспортёр — базовая схема, труба — ведро, трубка — стекло); насос — ведро сверху, сплав, корпус,
        // сплав, осмий ×3.
        RecipeDef { name: "basic_logistical_transporter", shape: Shaped(vec!["SCS"], vec![('S', Item(I_STEEL_INGOT)), ('C', Item(I_BASIC_CIRCUIT))]), out: i(LOGISTICAL_TRANSPORTER), count: 8 },
        RecipeDef { name: "basic_mechanical_pipe", shape: Shaped(vec!["SBS"], vec![('S', Item(I_STEEL_INGOT)), ('B', Item(I_BUCKET))]), out: i(MECHANICAL_PIPE), count: 8 },
        RecipeDef { name: "basic_pressurized_tube", shape: Shaped(vec!["SGS"], vec![('S', Item(I_STEEL_INGOT)), ('G', Item(i(GLASS)))]), out: i(PRESSURIZED_TUBE), count: 8 },
        RecipeDef { name: "electric_pump", shape: Shaped(vec![" B ", "ACA", "OOO"], vec![('B', Item(I_BUCKET)), ('A', Item(I_ALLOY_INFUSED)), ('C', Item(i(STEEL_CASING))), ('O', Item(I_OSMIUM_INGOT))]), out: i(ELECTRIC_PUMP), count: 1 },
        RecipeDef { name: "upgrade_gas", shape: Shaped(vec![" G ", "ADA", " G "], vec![('G', Item(i(GLASS))), ('A', Item(I_ALLOY_INFUSED)), ('D', Item(I_DUST_IRON))]), out: I_UPGRADE_GAS, count: 1 },
        RecipeDef { name: "steel_nugget",shape: Shapeless(vec![Item(I_STEEL_INGOT)]), out: I_STEEL_NUGGET, count: 9 },
        RecipeDef { name: "steel_ingot_from_nuggets", shape: Shaped(vec!["###", "###", "###"], vec![('#', Item(I_STEEL_NUGGET))]), out: I_STEEL_INGOT, count: 1 },
        RecipeDef { name: "block_steel", shape: Shaped(vec!["###", "###", "###"], vec![('#', Item(I_STEEL_INGOT))]), out: i(STEEL_BLOCK), count: 1 },
        RecipeDef { name: "steel_ingot_from_block", shape: Shapeless(vec![Item(i(STEEL_BLOCK))]), out: I_STEEL_INGOT, count: 9 },
        RecipeDef { name: "block_osmium", shape: Shaped(vec!["###", "###", "###"], vec![('#', Item(I_OSMIUM_INGOT))]), out: i(OSMIUM_BLOCK), count: 1 },
        RecipeDef { name: "osmium_ingot_from_block", shape: Shapeless(vec![Item(i(OSMIUM_BLOCK))]), out: I_OSMIUM_INGOT, count: 9 },
        RecipeDef { name: "redstone_block", shape: Shaped(vec!["###", "###", "###"], vec![('#', Item(I_REDSTONE))]), out: i(REDSTONE_BLOCK), count: 1 },
        RecipeDef { name: "redstone", shape: Shapeless(vec![Item(i(REDSTONE_BLOCK))]), out: I_REDSTONE, count: 9 },
    ];
    let shapes: [(&'static str, Vec<&'static str>); 5] = [
        ("steel_pickaxe", vec!["XXX", " # ", " # "]),
        ("steel_axe", vec!["XX", "X#", " #"]),
        ("steel_shovel", vec!["X", "#", "#"]),
        ("steel_hoe", vec!["XX", " #", " #"]),
        ("steel_sword", vec!["X", "X", "#"]),
    ];
    for (k, (name, rows)) in shapes.into_iter().enumerate() {
        v.push(RecipeDef { name, shape: Shaped(rows, vec![('X', Item(I_STEEL_INGOT)), ('#', Item(I_IRON_INGOT))]), out: I_STEEL_PICKAXE + k as ItemId, count: 1 });
    }
    let parts: [Vec<&'static str>; 4] = [vec!["XXX", "X X"], vec!["X X", "XXX", "XXX"], vec!["XXX", "X X", "X X"], vec!["X X", "X X"]];
    for (p, rows) in parts.into_iter().enumerate() {
        v.push(RecipeDef { name: "steel_armor", shape: Shaped(rows, vec![('X', Item(I_STEEL_INGOT))]), out: I_STEEL_HELMET + p as ItemId, count: 1 });
    }
    v
}

/// Плавка в печи (MEK-05 §3.4): сырьё и пыль металлов → слиток, стальное снаряжение → стальной самородок.
pub(crate) fn smelt(item: ItemId) -> Option<ItemId> {
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
