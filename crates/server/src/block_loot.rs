//! Bounded block-entity birth and destruction plans. No commits, world scans or
//! invented item metadata. Stateful Mek block-items need the typed payload ledger.
use crate::{survival, windows};
use core_types::{BlockPos, Cell, ComponentId};
use rules::{
    blocks::BeKind,
    machines::Machine,
    stack::ItemStack,
    survival::furnace::{Chest, Furnace},
};
use world::{CellRead, ComponentData, World};
const MACHINE_COMPONENT: u16 = 0x8201;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Unknown,
    Component,
    Encoding,
    UnsupportedTypedItem,
    Capacity,
}
pub struct Contents {
    pub anchor: BlockPos,
    pub cell: Cell,
    pub remove: Option<ComponentId>,
    pub stacks: Vec<ItemStack>,
}
fn known(world: &World, at: BlockPos) -> Result<Cell, Error> {
    match world.read(at) {
        CellRead::Known(c) => Ok(c),
        CellRead::Unknown => Err(Error::Unknown),
    }
}
fn main(world: &World, position: BlockPos) -> Result<(BlockPos, Cell), Error> {
    let raw = known(world, position)?;
    let p = rules::blocks::main_of(raw.raw(), [position.x, position.y, position.z]);
    let anchor = BlockPos {
        space: position.space,
        x: p[0],
        y: p[1],
        z: p[2],
    };
    Ok((anchor, known(world, anchor)?))
}
pub fn prepare_contents(world: &World, position: BlockPos) -> Result<Contents, Error> {
    let (anchor, cell) = main(world, position)?;
    let id = world
        .page(anchor.page())
        .ok_or(Error::Unknown)?
        .component(anchor.local_index());
    let mut result = Contents {
        anchor,
        cell,
        remove: None,
        stacks: Vec::new(),
    };
    let Some(id) = id else {
        return Ok(result);
    };
    let record = world.component(id).ok_or(Error::Component)?;
    if record.data.anchor != anchor {
        return Err(Error::Component);
    }
    if record.data.kind == survival::DROP_COMPONENT {
        survival::DropBundle::decode(&record.data.payload).map_err(|_| Error::Encoding)?;
        return Ok(result);
    }
    let definition = rules::block(cell.id()).ok_or(Error::Component)?;
    match (definition.be, record.data.kind) {
        (Some(BeKind::Chest), survival::CHEST_COMPONENT) => {
            let value = Chest::decode(&record.data.payload).map_err(|_| Error::Encoding)?;
            result
                .stacks
                .extend(value.slots().iter().copied().filter(|s| !s.is_empty()));
        }
        (Some(BeKind::Furnace), survival::FURNACE_COMPONENT) => {
            let value = Furnace::decode(&record.data.payload).map_err(|_| Error::Encoding)?;
            result
                .stacks
                .extend(value.slots().iter().copied().filter(|s| !s.is_empty()));
        }
        (Some(BeKind::Mek(kind)), MACHINE_COMPONENT) => {
            let value = Machine::decode(&record.data.payload).map_err(|_| Error::Encoding)?;
            if value.kind != kind {
                return Err(Error::Component);
            }
            // The approved donor preserves the complete machine inside ONE
            // block item (mek_tests). Scattering slots would change that rule.
            if value != Machine::new(kind) {
                return Err(Error::UnsupportedTypedItem);
            }
        }
        _ => return Err(Error::Component),
    }
    result.remove = Some(id);
    Ok(result)
}
/// Use the actual prepared placement cell (including orientation). The caller
/// appends this component to the same placement/inventory WAL transaction.
pub fn initial_component(
    world: &World,
    anchor: BlockPos,
    actual: Cell,
) -> Result<Option<(ComponentId, ComponentData)>, Error> {
    let page = world.page(anchor.page()).ok_or(Error::Unknown)?;
    let definition = rules::block(actual.id()).ok_or(Error::Component)?;
    let Some(kind) = definition.be else {
        return Ok(None);
    };
    if page.component(anchor.local_index()).is_some() {
        return Err(Error::Component);
    }
    let data = match kind {
        BeKind::Chest => survival::chest_component(anchor, &Chest::default()),
        BeKind::Furnace => survival::furnace_component(anchor, &Furnace::default()),
        BeKind::Mek(kind) => {
            let mut bytes = [0; rules::machines::MACHINE_BYTES];
            let used = Machine::new(kind).encode(&mut bytes);
            ComponentData {
                anchor,
                kind: MACHINE_COMPONENT,
                payload: bytes[..used].to_vec(),
            }
        }
    };
    let id = windows::fresh_component(world, anchor, data.kind).map_err(|e| match e {
        windows::Error::Capacity => Error::Capacity,
        _ => Error::Component,
    })?;
    Ok(Some((id, data)))
}
