//! Shared block/item definitions, stable IDs, and state-dependent world properties.
//!
//! The pinned donor contributes only numeric data and pure formulas permitted by
//! SPEC §2. The registry has no dependency on world storage, simulation, or render.
#![forbid(unsafe_code)]

pub mod arms;
pub mod bindings;
pub mod blocks;
pub mod cell;
pub mod crafting;
mod crafting_data;
pub mod crossroads;
pub mod entities;
pub mod ids;
pub mod item_payload;
pub mod items;
pub mod machine_recipes;
pub mod machines;
pub mod shapes;
pub mod stack;
pub mod survival;

pub use blocks::BlockDef;
use core_types::{Cell, CellProperties};
pub use items::{ItemDef, ItemId};

/// Rendering categories, not legacy ABI numbers or network discriminants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Model {
    Cable,
    Chest,
    Ladder,
    Mek,
    Snow,
    WallTorch,
}

/// Defined IDs only: reserved and unsupported values never alias air.
pub fn block(id: u16) -> Option<&'static BlockDef> {
    let definition = blocks::bdef(id);
    (definition.mc != "unknown").then_some(definition)
}

pub fn item(id: ItemId) -> Option<&'static ItemDef> {
    let definition = items::idef(id);
    (definition.kind != items::ItemKind::None).then_some(definition)
}

pub fn all_blocks() -> impl Iterator<Item = (u16, &'static BlockDef)> {
    (0..blocks::BLOCK_COUNT as u16).filter_map(|id| block(id).map(|definition| (id, definition)))
}

pub fn all_items() -> impl Iterator<Item = (ItemId, &'static ItemDef)> {
    (1..items::ITEMS_END)
        .chain(items::G7_FIRST..items::G7_END)
        .filter_map(|id| item(id).map(|definition| (id, definition)))
}

/// Name lookup preserves the distinction between a missing name and ID zero.
pub fn block_by_name(name: &str) -> Option<(u16, &'static BlockDef)> {
    all_blocks().find(|(_, definition)| definition.mc == name)
}

pub fn item_by_name(name: &str) -> Option<(ItemId, &'static ItemDef)> {
    all_items().find(|(_, definition)| definition.mc == name)
}

/// Classify a known registry cell without allocation or generation. Unknown IDs
/// fail closed for collision/opacity instead of becoming passable air.
pub fn properties(cell: Cell) -> CellProperties {
    let Some(definition) = block(cell.id()) else {
        return CellProperties {
            shape: cell.raw(),
            ..CellProperties::SOLID
        };
    };
    if cell.id() == ids::AIR {
        return CellProperties::AIR;
    }
    let collidable = !blocks::collision_of(cell.raw()).is_empty();
    let full_opaque = definition.suffocates
        && !definition.leaves
        && definition.fluid == 0
        && matches!(definition.render, blocks::Render::Cube);
    let emitted = blocks::light_of(cell.raw());
    CellProperties {
        shape: shape_key(cell, definition),
        occupied: true,
        opaque: full_opaque,
        collidable,
        blocks_projectile: collidable,
        fluid: definition.fluid != 0,
        emission: [emitted; 3],
        light_opacity: if full_opaque {
            15
        } else if definition.leaves || definition.fluid == blocks::FLUID_WATER {
            1
        } else {
            0
        },
    }
}

/// Geometry/material state excludes emission-only LIT bits. Animated machine
/// faces and emission are updated by material/light channels, not terrain scans.
fn shape_key(cell: Cell, definition: &BlockDef) -> u32 {
    use blocks::{Shape, TexRule};
    let state = match definition.shape {
        Shape::Cable | Shape::Large => cell.state() & 0x1f7f,
        Shape::Ladder | Shape::WallTorch | Shape::Layers | Shape::Wheat | Shape::Root => {
            cell.state() & 7
        }
        Shape::Fixed => match definition.tex_rule {
            TexRule::Axis => cell.state() & 3,
            TexRule::Facing { .. } | TexRule::Crop { .. } | TexRule::Farmland => cell.state() & 7,
            TexRule::Fixed if definition.fluid != 0 => cell.state() & 15,
            TexRule::Fixed if matches!(definition.place, blocks::Place::FacePlayer) => {
                cell.state() & 7
            }
            TexRule::Fixed => 0,
        },
    };
    cell.id() as u32 | (state as u32) << 16
}
