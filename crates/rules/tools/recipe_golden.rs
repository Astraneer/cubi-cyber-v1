//! Generate references from the original SPEC-approved donor implementation only.
//! No production rules implementation is linked. Never run this automatically.
#![allow(dead_code, unused_imports)]

#[path = "../src/cell.rs"] mod cell;
#[path = "../src/arms.rs"] mod arms;
#[path = "legacy_stack.rs"] pub(crate) mod donor_stack;
#[path = "../../../donor/crates/voxel-core/src/registry/ids.rs"] pub(crate) mod donor_ids;
#[path = "../../../donor/crates/voxel-core/src/registry/blocks.rs"] pub(crate) mod donor_blocks;
#[path = "../../../donor/crates/voxel-core/src/registry/items.rs"] pub(crate) mod donor_items;
#[path = "../../../donor/crates/voxel-core/src/registry/shapes.rs"] pub(crate) mod donor_shapes;
#[path = "../../../donor/crates/voxel-core/src/game/crafting.rs"] pub(crate) mod donor_crafting;
#[path = "../../../donor/crates/voxel-core/src/world/mek/recipes.rs"] pub(crate) mod donor_machine_recipes;

mod registry {
    pub(crate) use crate::donor_blocks as blocks;
    pub(crate) use crate::donor_ids as ids;
    pub(crate) use crate::donor_items as items;
    pub(crate) use crate::donor_shapes as shapes;
}
mod world {
    pub(crate) use crate::donor_stack as stack;
    pub(crate) mod grid { pub(crate) use crate::arms::*; }
    pub(crate) mod mek { pub(crate) use crate::donor_machine_recipes as recipes; }
}
mod game { pub(crate) use crate::donor_crafting as crafting; }
mod abi {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Model { Cable, Chest, Ladder, Mek, Snow, WallTorch }
}

use donor_crafting::{Grid, Ing, RecipeKind};
use donor_stack::ItemStack;

fn ingredient(ingredient: &Ing) -> String {
    match ingredient {
        Ing::Item(id) => format!("i:{id}"),
        Ing::Set(ids) => format!("s:{}", ids.iter().map(u16::to_string).collect::<Vec<_>>().join("|")),
    }
}
fn ids(ingredient: &Ing) -> &[u16] {
    match ingredient { Ing::Item(id) => std::slice::from_ref(id), Ing::Set(ids) => ids }
}
fn stack(stack: ItemStack) -> String { format!("{},{},{}", stack.item, stack.count, stack.damage) }

fn main() {
    let directory = std::env::args().nth(1).expect("output directory argument");
    let directory = std::path::Path::new(&directory);
    std::fs::create_dir_all(directory).expect("create output");
    let book = donor_crafting::RecipeBook::standard();
    assert_eq!(book.list.len(), 97);
    let mut recipes = String::new();
    let mut cases = String::new();
    for (index, recipe) in book.list.iter().enumerate() {
        let mut grid = Grid::new(3, 3);
        let shape = match &recipe.kind {
            RecipeKind::Shaped(pattern) => {
                for y in 0..pattern.h {
                    for x in 0..pattern.w {
                        if let Some(value) = pattern.cells[x + y * pattern.w] {
                            grid.set(x, y, ItemStack::of(ids(&value)[0], 1));
                        }
                    }
                }
                format!("S:{}:{}:{}", pattern.w, pattern.h, pattern.cells.iter().map(|cell|
                    cell.as_ref().map_or("-".into(), ingredient)).collect::<Vec<_>>().join(";"))
            }
            RecipeKind::Shapeless(values) => {
                for (slot, value) in values.iter().enumerate() {
                    grid.set(slot % 3, slot / 3, ItemStack::of(ids(value)[0], 1));
                }
                format!("L:{}", values.iter().map(ingredient).collect::<Vec<_>>().join(";"))
            }
            RecipeKind::Repair => {
                grid.set(0, 0, ItemStack { item: donor_items::I_WOODEN_PICKAXE, count: 1, damage: 35 });
                grid.set(2, 2, ItemStack { item: donor_items::I_WOODEN_PICKAXE, count: 1, damage: 35 });
                "R".into()
            }
        };
        recipes.push_str(&format!("{index}\t{}\t{}\t{}\t{shape}\n", recipe.name, recipe.out, recipe.count));
        let (matched, _) = book.find(&grid).expect("canonical donor recipe matches");
        assert_eq!(matched, index, "canonical recipe overlap");
        cases.push_str(&format!("{index}\t{}\t{}\n", grid.cells.iter().map(|&value| stack(value)).collect::<Vec<_>>().join(";"), stack(book.result(&grid))));
    }
    let mut nei = String::new();
    for item in (1..donor_items::ITEMS_END).chain(donor_items::G7_FIRST..donor_items::G7_END) {
        let definition = donor_items::idef(item);
        if definition.kind == donor_items::ItemKind::None { continue; }
        let mut producers = Vec::new(); let mut users = Vec::new();
        for (index, recipe) in book.list.iter().enumerate() {
            match &recipe.kind {
                RecipeKind::Repair if definition.max_damage > 0 => { producers.push(index); users.push(index); }
                RecipeKind::Repair => {}
                kind => {
                    if recipe.out == item { producers.push(index); }
                    let uses = match kind {
                        RecipeKind::Shaped(pattern) => pattern.cells.iter().flatten().any(|value| ids(value).contains(&item)),
                        RecipeKind::Shapeless(values) => values.iter().any(|value| ids(value).contains(&item)),
                        _ => false,
                    };
                    if uses { users.push(index); }
                }
            }
        }
        let list = |values: Vec<usize>| values.iter().map(usize::to_string).collect::<Vec<_>>().join(",");
        nei.push_str(&format!("{item}\t{}\t{}\n", list(producers), list(users)));
    }
    std::fs::write(directory.join("crafting.tsv"), recipes).unwrap();
    std::fs::write(directory.join("crafting_cases.tsv"), cases).unwrap();
    std::fs::write(directory.join("crafting_nei.tsv"), nei).unwrap();

    use donor_machine_recipes as m;
    let mut machines = String::new();
    for (index, value) in m::INFUSE.iter().enumerate() {
        machines.push_str(&format!("Infuse\t{index}\t{}\t1\titem:{}:1\t{}:{}:operation\n", value.input, value.out, value.sub, value.amount));
    }
    for (kind, values) in [("Enrich", &m::ENRICH[..]), ("Crush", &m::CRUSH[..])] {
        for (index, value) in values.iter().enumerate() {
            machines.push_str(&format!("{kind}\t{index}\t{}\t{}\titem:{}:{}\t-\n", value.input, value.in_n, value.out, value.out_n));
        }
    }
    for (index, value) in m::PURIFY.iter().enumerate() {
        machines.push_str(&format!("Purify\t{index}\t{}\t1\titem:{}:{}\t{}:{}:tick\n", value.input, value.out, value.out_n, m::SUB_OXYGEN, value.rate));
    }
    let known = || (1..donor_items::ITEMS_END).chain(donor_items::G7_FIRST..donor_items::G7_END)
        .filter(|&id| donor_items::idef(id).kind != donor_items::ItemKind::None);
    for input in known() {
        if let Some(output) = m::smelt(input) {
            machines.push_str(&format!("Smelt\t{input}\t{input}\t1\titem:{output}:1\t-\n"));
        }
    }
    for input in known() {
        let converted = m::chem_of(input).or_else(|| { let amount = m::oxygen_of(input); (amount != 0).then_some((m::SUB_OXYGEN, amount)) });
        if let Some((substance, amount)) = converted {
            machines.push_str(&format!("ChemicalConversion\t{input}\t{input}\t1\tsub:{substance}:{amount}\t-\n"));
        }
    }
    for input in known() {
        let energy = m::energy_of(input);
        if energy != 0 { machines.push_str(&format!("EnergyConversion\t{input}\t{input}\t1\tenergy:{energy}\t-\n")); }
    }
    std::fs::write(directory.join("machines.tsv"), machines).unwrap();
}
