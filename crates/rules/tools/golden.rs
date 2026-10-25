//! Rebuild initial registry references directly from SPEC-approved donor tables.
//! Run with the pinned rustc, never as part of tests/CI. Any subsequent update
//! requires an explicit reviewed explanation of the changed behavior.
#![allow(dead_code, unused_imports)]

#[path = "../src/cell.rs"]
mod cell;
#[path = "../src/arms.rs"]
mod arms;
#[path = "../../../donor/crates/voxel-core/src/registry/ids.rs"]
pub(crate) mod donor_ids;
#[path = "../../../donor/crates/voxel-core/src/registry/blocks.rs"]
pub(crate) mod donor_blocks;
#[path = "../../../donor/crates/voxel-core/src/registry/items.rs"]
pub(crate) mod donor_items;
#[path = "../../../donor/crates/voxel-core/src/registry/shapes.rs"]
pub(crate) mod donor_shapes;

mod registry {
    pub(crate) use crate::donor_blocks as blocks;
    pub(crate) use crate::donor_ids as ids;
    pub(crate) use crate::donor_items as items;
    pub(crate) use crate::donor_shapes as shapes;
}
mod world {
    pub(crate) mod grid {
        pub(crate) use crate::arms::*;
    }
}
mod abi {
    // Only these symbolic data categories occur in the allowed block table;
    // numeric protocol/ABI values are deliberately not inferred or assigned.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Model { Cable, Chest, Ladder, Mek, Snow, WallTorch }
}

fn main() {
    let root = std::env::args().nth(1).expect("output directory argument");
    let root = std::path::Path::new(&root);
    std::fs::create_dir_all(root).expect("create output");
    let mut blocks = String::new();
    for id in 0..256u16 {
        let definition = donor_blocks::bdef(id);
        if id == 0 || definition.mc != "air" {
            blocks.push_str(&format!("{id}\t{definition:?}\n"));
        }
    }
    let mut items = String::new();
    for id in (1..donor_items::ITEMS_END).chain(donor_items::G7_FIRST..donor_items::G7_END) {
        let definition = donor_items::idef(id);
        if definition.kind != donor_items::ItemKind::None {
            items.push_str(&format!("{id}\t{definition:?}\n"));
        }
    }
    std::fs::write(root.join("blocks.tsv"), blocks).expect("write block reference");
    std::fs::write(root.join("items.tsv"), items).expect("write item reference");
}
