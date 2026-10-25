use core_types::{BlockPos, Cell, ChannelMask, ComponentId, SpaceId, Tick};
use rules::{
    blocks::MekKind,
    ids::*,
    machines::{Machine, Slot},
    stack::ItemStack,
    survival::furnace::{Chest, Furnace},
};
use server::{block_loot, survival};
use world::{Transaction, World};
fn at() -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x: 8,
        y: 1,
        z: 8,
    }
}
fn cell(id: u16) -> Cell {
    Cell::from_parts(id, 0)
}
fn fixture(id: u16, data: Option<world::ComponentData>) -> World {
    let mut w = World::new(rules::properties, 1024);
    let mut cells = vec![Cell::AIR; 4096];
    cells[at().local_index().get() as usize] = cell(id);
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    tx.create_page(at().page(), cells);
    if let Some(data) = data {
        tx.set_component(ComponentId(500), data, ChannelMask::CONTENTS);
    }
    w.commit(tx).unwrap();
    w
}
#[test]
fn chest_and_furnace_contents_leave_one_explicit_removal_without_mutating_world() {
    let mut chest = Chest::default();
    chest
        .set_trusted(0, ItemStack::new(STONE, 64).unwrap())
        .unwrap();
    chest
        .set_trusted(
            26,
            ItemStack::from_parts(rules::items::I_WOODEN_PICKAXE + 10, 1, 7).unwrap(),
        )
        .unwrap();
    let w = fixture(CHEST, Some(survival::chest_component(at(), &chest)));
    let before = w.canonical_hash();
    let plan = block_loot::prepare_contents(&w, at()).unwrap();
    assert_eq!(plan.remove, Some(ComponentId(500)));
    assert_eq!(plan.anchor, at());
    assert_eq!(plan.cell, cell(CHEST));
    assert_eq!(plan.stacks, &[chest.slots()[0], chest.slots()[26]]);
    assert_eq!(w.canonical_hash(), before);
    let mut furnace = Furnace::default();
    furnace
        .set_trusted(0, ItemStack::new(rules::items::I_RAW_IRON, 5).unwrap())
        .unwrap();
    furnace
        .set_trusted(1, ItemStack::new(rules::items::I_COAL, 2).unwrap())
        .unwrap();
    furnace
        .set_trusted(2, ItemStack::new(rules::items::I_IRON_INGOT, 3).unwrap())
        .unwrap();
    let w = fixture(FURNACE, Some(survival::furnace_component(at(), &furnace)));
    assert_eq!(
        block_loot::prepare_contents(&w, at()).unwrap().stacks,
        furnace.slots()
    );
}
#[test]
fn placement_birth_is_atomic_and_does_not_overwrite_an_existing_anchor_component() {
    for id in [CHEST, FURNACE, WIND_GENERATOR, HEAT_GENERATOR] {
        let mut w = fixture(AIR, None);
        let (component, data) = block_loot::initial_component(&w, at(), cell(id))
            .unwrap()
            .unwrap();
        assert_eq!(w.read(at()), world::CellRead::Known(Cell::AIR));
        assert!(w.component(component).is_none());
        let mut tx = Transaction::new(Tick(2), SpaceId(0));
        tx.set_cell(at(), cell(id));
        tx.set_component(component, data, ChannelMask::CONTENTS);
        w.commit(tx).unwrap();
        assert!(matches!(
            block_loot::initial_component(&w, at(), cell(id)),
            Err(block_loot::Error::Component)
        ));
        assert!(
            block_loot::prepare_contents(&w, at())
                .unwrap()
                .stacks
                .is_empty()
        );
    }
    let w = fixture(STONE, None);
    assert!(
        block_loot::initial_component(&w, at(), cell(STONE))
            .unwrap()
            .is_none()
    );
    let mut unknown = at();
    unknown.x = 1000;
    assert!(matches!(
        block_loot::initial_component(&w, unknown, cell(CHEST)),
        Err(block_loot::Error::Unknown)
    ));
}
#[test]
fn stateful_machine_never_scatters_or_loses_charge_and_corrupt_components_fail_closed() {
    let mut machine = Machine::new(MekKind::Cube);
    machine.slots[0] = Slot::tablet(4000);
    machine.energy = 1234;
    let mut bytes = [0; rules::machines::MACHINE_BYTES];
    let used = machine.encode(&mut bytes);
    let data = world::ComponentData {
        anchor: at(),
        kind: 0x8201,
        payload: bytes[..used].to_vec(),
    };
    let w = fixture(ENERGY_CUBE, Some(data));
    assert!(matches!(
        block_loot::prepare_contents(&w, at()),
        Err(block_loot::Error::UnsupportedTypedItem)
    ));
    let mut data = survival::chest_component(at(), &Chest::default());
    data.payload.push(1);
    let w = fixture(CHEST, Some(data));
    assert!(matches!(
        block_loot::prepare_contents(&w, at()),
        Err(block_loot::Error::Encoding)
    ));
    let data = world::ComponentData {
        anchor: at(),
        kind: 999,
        payload: Vec::new(),
    };
    let w = fixture(CHEST, Some(data));
    assert!(matches!(
        block_loot::prepare_contents(&w, at()),
        Err(block_loot::Error::Component)
    ));
}
