//! Local resource names map to existing registry layers; bytes never enter net DTOs.
//! Call only while explicitly loading an imported pack.
use audio::resources::LocalResources;
use render::material_pack::{LayerSource, Limits, MaterialError, PreparedMaterials};
use render::terrain_gpu::AtlasImage;
use rules::ids::*;

const BLOCKS: &[(u32, &str)] = &[
    (L_GRASS_TOP, "grass_block_top"),
    (L_GRASS_SIDE, "grass_block_side"),
    (L_GRASS_SIDE_T, "grass_block_side"),
    (L_GRASS_STEP_T, "grass_block_side"),
    (L_DRY_TOP, "grass_block_top"),
    (L_DRY_SIDE, "grass_block_side"),
    (L_DARK_TOP, "grass_block_top"),
    (L_DARK_SIDE, "grass_block_side"),
    (L_SNOWY_SIDE, "grass_block_snow"),
    (L_DIRT, "dirt"),
    (L_STONE, "stone"),
    (L_SAND, "sand"),
    (L_GRAVEL, "gravel"),
    (L_SNOW, "snow"),
    (L_SANDSTONE, "sandstone"),
    (L_SANDSTONE_TOP, "sandstone_top"),
    (L_CLAY, "clay"),
    (L_LOG, "oak_log"),
    (L_LOG_TOP, "oak_log_top"),
    (L_LEAVES, "oak_leaves"),
    (L_BIRCH, "birch_log"),
    (L_BIRCH_TOP, "birch_log_top"),
    (L_BIRCH_LEAVES, "birch_leaves"),
    (L_SPRUCE, "spruce_log"),
    (L_SPRUCE_TOP, "spruce_log_top"),
    (L_SPRUCE_LEAVES, "spruce_leaves"),
    (L_WATER, "water_still"),
    (L_CACTUS, "cactus_side"),
    (L_SHORT_GRASS, "short_grass"),
    (L_DANDELION, "dandelion"),
    (L_POPPY, "poppy"),
    (L_DEAD_BUSH, "dead_bush"),
    (L_COBBLE, "cobblestone"),
    (L_BEDROCK, "bedrock"),
    (L_FERN, "fern"),
    (L_GRASS_FULL, "grass_block_top"),
    (L_OAK_PLANKS, "oak_planks"),
    (L_BIRCH_PLANKS, "birch_planks"),
    (L_SPRUCE_PLANKS, "spruce_planks"),
    (L_TABLE_TOP, "crafting_table_top"),
    (L_TABLE_SIDE, "crafting_table_side"),
    (L_TABLE_FRONT, "crafting_table_front"),
    (L_FURNACE_FRONT, "furnace_front"),
    (L_FURNACE_FRONT_ON, "furnace_front_on"),
    (L_FURNACE_SIDE, "furnace_side"),
    (L_FURNACE_TOP, "furnace_top"),
    (L_COAL_ORE, "coal_ore"),
    (L_IRON_ORE, "iron_ore"),
    (L_GLASS, "glass"),
    (L_TORCH, "torch"),
    (L_LADDER, "ladder"),
    (L_FARMLAND_DRY, "farmland"),
    (L_FARMLAND_WET, "farmland_moist"),
    (L_OAK_SAPLING, "oak_sapling"),
    (L_BIRCH_SAPLING, "birch_sapling"),
    (L_SPRUCE_SAPLING, "spruce_sapling"),
    (L_WOOL_WHITE, "white_wool"),
    (L_WOOL_BLACK, "black_wool"),
    (L_WOOL_GRAY, "gray_wool"),
    (L_WOOL_LIGHT_GRAY, "light_gray_wool"),
    (L_WOOL_BROWN, "brown_wool"),
    (L_WOOL_PINK, "pink_wool"),
    (L_HAY_TOP, "hay_block_top"),
    (L_HAY_SIDE, "hay_block_side"),
    (L_COPPER_ORE, "copper_ore"),
    (L_GOLD_ORE, "gold_ore"),
    (L_REDSTONE_ORE, "redstone_ore"),
    (L_IRON_BLOCK, "iron_block"),
    (L_REDSTONE_BLOCK, "redstone_block"),
    (L_RAW_IRON_BLOCK, "raw_iron_block"),
    (L_LAVA, "lava_still"),
];

pub fn sources(local: &LocalResources) -> Result<Vec<LayerSource<'_>>, MaterialError> {
    let mut result = Vec::with_capacity(BLOCKS.len() + 16);
    let mut insert = |layer, name: &str| -> Result<(), MaterialError> {
        let maps = local
            .labpbr("minecraft", &format!("block/{name}"))
            .map_err(|_| MaterialError::Layer)?;
        if maps.albedo.is_some() || maps.normal.is_some() || maps.specular.is_some() {
            result.push(LayerSource {
                layer,
                albedo_png: maps.albedo,
                normal_png: maps.normal,
                specular_png: maps.specular,
            });
        }
        Ok(())
    };
    for &(layer, name) in BLOCKS {
        insert(layer, name)?;
    }
    for stage in 0..8 {
        insert(L_WHEAT + stage, &format!("wheat_stage{stage}"))?;
    }
    for stage in 0..4 {
        insert(L_CARROTS + stage, &format!("carrots_stage{stage}"))?;
        insert(L_POTATOES + stage, &format!("potatoes_stage{stage}"))?;
    }
    Ok(result)
}

pub fn prepare(
    local: &LocalResources,
    limits: Limits,
) -> Result<Option<PreparedMaterials>, MaterialError> {
    let sources = sources(local)?;
    if sources.is_empty() {
        return Ok(None);
    }
    let base = AtlasImage::donor().map_err(|_| MaterialError::Atlas)?;
    PreparedMaterials::prepare(&base, &sources, local.revision(), limits).map(Some)
}
