//! Pure recipe definitions from the SPEC-approved donor at
//! 3992583213e1f00d95a3beef2bde43ae970192d4 (game/crafting.rs).

use crate::crafting::{
    Ing, LIST_COAL_OR_CHARCOAL, RShape, RecipeDef, TAG_BIRCH_LOGS, TAG_OAK_LOGS, TAG_PLANKS,
    TAG_SPRUCE_LOGS, TAG_STONE_CRAFTING_MATERIALS, TAG_STONE_TOOL_MATERIALS,
};
use crate::ids::*;
use crate::items::*;

pub fn path_recipes() -> Vec<RecipeDef> {
    use Ing::*;
    use RShape::*;
    let i = |b: crate::cell::BlockId| b as ItemId;
    let mut v = vec![
        RecipeDef {
            name: "oak_planks",
            shape: Shapeless(vec![Set(TAG_OAK_LOGS)]),
            out: i(PLANKS),
            count: 4,
        },
        RecipeDef {
            name: "birch_planks",
            shape: Shapeless(vec![Set(TAG_BIRCH_LOGS)]),
            out: i(BIRCH_PLANKS),
            count: 4,
        },
        RecipeDef {
            name: "spruce_planks",
            shape: Shapeless(vec![Set(TAG_SPRUCE_LOGS)]),
            out: i(SPRUCE_PLANKS),
            count: 4,
        },
        RecipeDef {
            name: "stick",
            shape: Shaped(vec!["#", "#"], vec![('#', Set(TAG_PLANKS))]),
            out: I_STICK,
            count: 4,
        },
        RecipeDef {
            name: "crafting_table",
            shape: Shaped(vec!["##", "##"], vec![('#', Set(TAG_PLANKS))]),
            out: i(CRAFTING_TABLE),
            count: 1,
        },
        RecipeDef {
            name: "furnace",
            shape: Shaped(
                vec!["###", "# #", "###"],
                vec![('#', Set(TAG_STONE_CRAFTING_MATERIALS))],
            ),
            out: i(FURNACE),
            count: 1,
        },
        RecipeDef {
            name: "torch",
            shape: Shaped(
                vec!["X", "#"],
                vec![('X', Set(LIST_COAL_OR_CHARCOAL)), ('#', Item(I_STICK))],
            ),
            out: i(TORCH),
            count: 4,
        },
        RecipeDef {
            name: "chest",
            shape: Shaped(vec!["###", "# #", "###"], vec![('#', Set(TAG_PLANKS))]),
            out: i(CHEST),
            count: 1,
        },
        RecipeDef {
            name: "ladder",
            shape: Shaped(vec!["# #", "###", "# #"], vec![('#', Item(I_STICK))]),
            out: i(LADDER),
            count: 3,
        },
        RecipeDef {
            name: "bucket",
            shape: Shaped(vec!["# #", " # "], vec![('#', Item(I_IRON_INGOT))]),
            out: I_BUCKET,
            count: 1,
        },
        RecipeDef {
            name: "shears",
            shape: Shaped(vec![" #", "# "], vec![('#', Item(I_IRON_INGOT))]),
            out: I_SHEARS,
            count: 1,
        },
        RecipeDef {
            name: "iron_block",
            shape: Shaped(vec!["###", "###", "###"], vec![('#', Item(I_IRON_INGOT))]),
            out: i(IRON_BLOCK),
            count: 1,
        },
        RecipeDef {
            name: "iron_ingot_from_iron_block",
            shape: Shapeless(vec![Item(i(IRON_BLOCK))]),
            out: I_IRON_INGOT,
            count: 9,
        },
        RecipeDef {
            name: "iron_nugget",
            shape: Shapeless(vec![Item(I_IRON_INGOT)]),
            out: I_IRON_NUGGET,
            count: 9,
        },
        RecipeDef {
            name: "iron_ingot_from_nuggets",
            shape: Shaped(vec!["###", "###", "###"], vec![('#', Item(I_IRON_NUGGET))]),
            out: I_IRON_INGOT,
            count: 1,
        },
        RecipeDef {
            name: "raw_iron_block",
            shape: Shaped(vec!["###", "###", "###"], vec![('#', Item(I_RAW_IRON))]),
            out: i(RAW_IRON_BLOCK),
            count: 1,
        },
        RecipeDef {
            name: "raw_iron",
            shape: Shapeless(vec![Item(i(RAW_IRON_BLOCK))]),
            out: I_RAW_IRON,
            count: 9,
        },
        RecipeDef {
            name: "coal_block",
            shape: Shaped(vec!["###", "###", "###"], vec![('#', Item(I_COAL))]),
            out: i(COAL_BLOCK),
            count: 1,
        },
        RecipeDef {
            name: "coal",
            shape: Shapeless(vec![Item(i(COAL_BLOCK))]),
            out: I_COAL,
            count: 9,
        },
        RecipeDef {
            name: "snow_block",
            shape: Shaped(vec!["##", "##"], vec![('#', Item(I_SNOWBALL))]),
            out: i(SNOW),
            count: 1,
        },
        RecipeDef {
            name: "snow",
            shape: Shaped(vec!["###"], vec![('#', Item(i(SNOW)))]),
            out: i(SNOW_LAYER),
            count: 6,
        },
        RecipeDef {
            name: "clay",
            shape: Shaped(vec!["##", "##"], vec![('#', Item(I_CLAY_BALL))]),
            out: i(CLAY),
            count: 1,
        },
        RecipeDef {
            name: "sandstone",
            shape: Shaped(vec!["##", "##"], vec![('#', Item(i(SAND)))]),
            out: i(SANDSTONE),
            count: 1,
        },
    ];
    let tiers: [Ing; 3] = [
        Set(TAG_PLANKS),
        Set(TAG_STONE_TOOL_MATERIALS),
        Item(I_IRON_INGOT),
    ];
    let shapes: [(&'static str, Vec<&'static str>); 5] = [
        ("pickaxe", vec!["XXX", " # ", " # "]),
        ("axe", vec!["XX", "X#", " #"]),
        ("shovel", vec!["X", "#", "#"]),
        ("hoe", vec!["XX", " #", " #"]),
        ("sword", vec!["X", "X", "#"]),
    ];
    for (t, mat) in tiers.iter().enumerate() {
        for (k, (name, rows)) in shapes.iter().enumerate() {
            let out = I_WOODEN_PICKAXE + (t * 5 + k) as ItemId;
            v.push(RecipeDef {
                name,
                shape: Shaped(rows.clone(), vec![('X', *mat), ('#', Item(I_STICK))]),
                out,
                count: 1,
            });
        }
    }
    let parts: [Vec<&'static str>; 4] = [
        vec!["XXX", "X X"],
        vec!["X X", "XXX", "XXX"],
        vec!["XXX", "X X", "X X"],
        vec!["X X", "X X"],
    ];
    for (m, mat) in [I_LEATHER, I_IRON_INGOT].iter().enumerate() {
        for (p, rows) in parts.iter().enumerate() {
            let out = I_LEATHER_HELMET + (m * 4 + p) as ItemId;
            v.push(RecipeDef {
                name: "armor",
                shape: Shaped(rows.clone(), vec![('X', Item(*mat))]),
                out,
                count: 1,
            });
        }
    }
    v.extend(crate::machine_recipes::crafting()); // Mekanism M1
    v.extend(g7_recipes()); // команда items: золотая и алмазная броня, хлеб, стог
    v
}

pub fn g7_recipes() -> Vec<RecipeDef> {
    use Ing::*;
    use RShape::*;
    let mut v = Vec::new();
    let parts: [Vec<&'static str>; 4] = [
        vec!["XXX", "X X"],
        vec!["X X", "XXX", "XXX"],
        vec!["XXX", "X X", "X X"],
        vec!["X X", "X X"],
    ];
    for (m, mat) in [I_GOLD_INGOT, I_DIAMOND].iter().enumerate() {
        for (p, rows) in parts.iter().enumerate() {
            let out = I_GOLDEN_HELMET + (m * 4 + p) as ItemId;
            v.push(RecipeDef {
                name: "armor",
                shape: Shaped(rows.clone(), vec![('X', Item(*mat))]),
                out,
                count: 1,
            });
        }
    }
    v.push(RecipeDef {
        name: "bread",
        shape: Shaped(vec!["###"], vec![('#', Item(I_WHEAT))]),
        out: I_BREAD,
        count: 1,
    });
    v.push(RecipeDef {
        name: "hay_block",
        shape: Shapeless(vec![Item(I_WHEAT); 9]),
        out: HAY_BLOCK as ItemId,
        count: 1,
    });
    v.push(RecipeDef {
        name: "wheat",
        shape: Shapeless(vec![Item(HAY_BLOCK as ItemId)]),
        out: I_WHEAT,
        count: 9,
    });
    v
}
