//! Bounded block-entity birth and destruction plans. No commits, world scans or
//! invented item metadata. Stateful Mek block-items need the typed payload ledger.
use crate::{survival, windows};
use core_types::{BlockPos, Cell, ComponentId};
use rules::{
    blocks::BeKind,
    item_payload::{Payload, Resolver},
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
    pub payloads: Vec<Payload>,
    pub replace_block_drop: bool,
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
    prepare(world, position, false, true)
}
pub fn prepare_contents_with_payloads(
    world: &World,
    position: BlockPos,
) -> Result<Contents, Error> {
    prepare(world, position, true, true)
}
/// `has_block_drop` comes from the real owned-tool/creative mining result.
pub fn prepare_for_break(
    world: &World,
    position: BlockPos,
    has_block_drop: bool,
) -> Result<Contents, Error> {
    prepare(world, position, true, has_block_drop)
}
fn prepare(
    world: &World,
    position: BlockPos,
    typed: bool,
    has_block_drop: bool,
) -> Result<Contents, Error> {
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
        payloads: Vec::new(),
        replace_block_drop: false,
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
            if !has_block_drop {
                for slot in value.slots.iter().take(rules::machines::roles(kind).len()) {
                    let (stack, payload) = slot.to_item().map_err(|_| Error::Encoding)?;
                    if !stack.is_empty() {
                        result.stacks.push(stack);
                    }
                    if let Some(payload) = payload {
                        result.payloads.push(payload);
                    }
                }
                result.remove = Some(id);
                return Ok(result);
            }
            // The approved donor preserves the complete machine inside ONE
            // block item (mek_tests). Scattering slots would change that rule.
            let stored = stored_machine(&value);
            if stored != Machine::new(kind) {
                if !typed {
                    return Err(Error::UnsupportedTypedItem);
                }
                let payload = Payload::machine(&stored).map_err(|_| Error::Encoding)?;
                result.stacks.push(
                    ItemStack::new(payload.item(), 1)
                        .map_err(|_| Error::Encoding)?
                        .with_payload(payload.id())
                        .map_err(|_| Error::Encoding)?,
                );
                result.payloads.push(payload);
                result.replace_block_drop = true;
            }
        }
        _ => return Err(Error::Component),
    }
    result.remove = Some(id);
    Ok(result)
}
/// The approved MekItem transfers retained resources/configuration. Heat,
/// progress, animation clocks and location-specific pump work start fresh.
fn stored_machine(source: &Machine) -> Machine {
    let mut stored = Machine::new(source.kind);
    stored.upgrades = source.upgrades;
    stored.energy = source.energy.min(stored.capacity());
    let slots = rules::machines::roles(source.kind).len();
    stored.slots[..slots].copy_from_slice(&source.slots[..slots]);
    if source.tank.amount > 0 {
        stored.tank = source.tank;
    }
    stored.sides = source.sides;
    stored.eject = source.eject;
    stored.gas = source.gas;
    stored.gas_mode = source.gas_mode;
    stored.gas_sides = source.gas_sides;
    stored
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
pub fn initial_component_with_payload(
    world: &World,
    anchor: BlockPos,
    actual: Cell,
    held: ItemStack,
    payloads: &dyn Resolver,
) -> Result<Option<(ComponentId, ComponentData)>, Error> {
    let Some((id, mut data)) = initial_component(world, anchor, actual)? else {
        if held.payload().is_some() {
            return Err(Error::UnsupportedTypedItem);
        }
        return Ok(None);
    };
    let Some(reference) = held.payload() else {
        return Ok(Some((id, data)));
    };
    if data.kind != MACHINE_COMPONENT || held.item() != rules::items::block_item(actual.id()) {
        return Err(Error::UnsupportedTypedItem);
    }
    let payload = payloads
        .resolve(reference, held.item())
        .map_err(|_| Error::Encoding)?;
    let machine = payload
        .resolve_machine(held.item())
        .map_err(|_| Error::Encoding)?;
    let Some(BeKind::Mek(kind)) = rules::block(actual.id()).and_then(|b| b.be) else {
        return Err(Error::Component);
    };
    if machine.kind != kind {
        return Err(Error::Component);
    }
    let mut restored = stored_machine(&machine);
    for slot in &mut restored.slots {
        if slot.stack.payload().is_some() {
            if slot.charge != 0 {
                return Err(Error::Encoding);
            }
            *slot = rules::machines::Slot::from_item(slot.stack, payloads)
                .map_err(|_| Error::Encoding)?;
        }
    }
    let mut raw = [0; rules::machines::MACHINE_BYTES];
    let used = restored.encode(&mut raw);
    data.payload = raw[..used].to_vec();
    Ok(Some((id, data)))
}
