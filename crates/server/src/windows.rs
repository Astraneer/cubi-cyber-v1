//! World binding for private authoritative windows. Plans perform no commits or
//! IO. The owner persists world, inventory, cursor and explicit drops together.
use crate::{survival, ui};
use core_types::{BlockPos, Cell, ChannelMask, ComponentId, PageId, SpaceId, Tick, Version};
use net::window::{WindowIntent, WindowSnapshot};
use rules::{
    blocks::{BeKind, MekKind},
    crafting::{RecipeBook, RecipeId},
    item_payload::{Payload, Resolver},
    machines::{self, Machine, SideMode},
    stack::ItemStack,
    survival::{
        Inventory,
        furnace::{Chest, Furnace},
    },
};
use sim::{GameMode, Player, WorldQuery};
use world::{CellRead, ComponentData, Transaction, World};

#[path = "windows/codec.rs"]
mod codec;
const MACHINE_COMPONENT: u16 = 0x8201;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
    pub target: BlockPos,
    pub expected_target: Cell,
    pub anchor: BlockPos,
    pub expected_anchor: Cell,
    pub component: Option<ComponentId>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundWindow {
    pub state: ui::WindowState,
    pub binding: Option<Binding>,
}
impl BoundWindow {
    pub fn inventory(id: u64) -> Result<Self, Error> {
        Ok(Self {
            state: ui::WindowState::open(id, ui::WindowKind::Inventory, &[])
                .map_err(Error::Window)?,
            binding: None,
        })
    }
    pub fn refresh(&mut self, world: &World) -> Result<bool, Error> {
        let Some(binding) = self.binding else {
            return Ok(false);
        };
        let block = read_bound(world, binding)?;
        let slots = block.stack_values()?;
        if matches!(block, Block::Machine(_)) {
            self.state.refresh_machine_container_trusted(&slots)
        } else {
            self.state.refresh_container_trusted(&slots)
        }
        .map_err(Error::Window)
    }
}
pub struct Context<'a> {
    pub player: &'a Player,
    pub space: SpaceId,
    pub inventory: &'a Inventory,
    pub permitted: bool,
    pub payloads: Option<&'a dyn Resolver>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Unknown,
    Occluded,
    OutOfReach,
    Protected,
    Spectator,
    Stale,
    Block,
    Component,
    Encoding,
    Capacity,
    UnsupportedTypedItem,
    Window(ui::WindowError),
}
#[must_use = "Commit private values, world transaction and drops in one durable action"]
pub struct WindowPlan {
    before: BoundWindow,
    after: BoundWindow,
    before_inventory: Inventory,
    after_inventory: Inventory,
    drops: Vec<ItemStack>,
    transaction: Option<Transaction>,
    pages: Vec<(PageId, Version)>,
    components: Vec<(ComponentId, Option<Version>)>,
    payloads: Vec<Payload>,
}
impl WindowPlan {
    pub fn window_after(&self) -> &BoundWindow {
        &self.after
    }
    pub fn inventory_after(&self) -> &Inventory {
        &self.after_inventory
    }
    pub fn drops(&self) -> &[ItemStack] {
        &self.drops
    }
    pub fn payloads(&self) -> &[Payload] {
        &self.payloads
    }
    pub fn take_payloads(&mut self) -> Vec<Payload> {
        std::mem::take(&mut self.payloads)
    }
    pub fn transaction(&self) -> Option<&Transaction> {
        self.transaction.as_ref()
    }
    pub fn take_transaction(&mut self) -> Option<Transaction> {
        self.transaction.take()
    }
    pub fn validate(
        &self,
        world: &World,
        window: &BoundWindow,
        inventory: &Inventory,
    ) -> Result<(), Error> {
        if *window != self.before
            || *inventory != self.before_inventory
            || !world.matches_versions(&self.pages)
            || self
                .components
                .iter()
                .any(|(id, v)| world.component(*id).map(|r| r.version) != *v)
        {
            return Err(Error::Stale);
        }
        Ok(())
    }
    pub fn apply_after_durable(
        self,
        window: &mut BoundWindow,
        inventory: &mut Inventory,
    ) -> Result<(), Error> {
        if *window != self.before || *inventory != self.before_inventory {
            return Err(Error::Stale);
        }
        *window = self.after;
        *inventory = self.after_inventory;
        Ok(())
    }
}
enum Block {
    Workbench,
    Chest(Box<Chest>),
    Furnace(Furnace),
    Machine(Box<Machine>),
}
impl Block {
    fn kind(&self) -> ui::WindowKind {
        match self {
            Self::Workbench => ui::WindowKind::Workbench,
            Self::Chest(_) => ui::WindowKind::Chest(27),
            Self::Furnace(_) => ui::WindowKind::Furnace,
            Self::Machine(m) => ui::WindowKind::Machine(m.kind),
        }
    }
    fn slots(&self) -> &[ItemStack] {
        match self {
            Self::Workbench => &[],
            Self::Chest(c) => c.slots(),
            Self::Furnace(f) => f.slots(),
            Self::Machine(_) => unreachable!("machine slots use stack projection"),
        }
    }
    fn stack_values(&self) -> Result<Vec<ItemStack>, Error> {
        Ok(match self {
            Self::Machine(m) => m.slots[..machines::roles(m.kind).len()]
                .iter()
                .map(|s| {
                    s.to_item()
                        .map(|(stack, _)| stack)
                        .map_err(|_| Error::Encoding)
                })
                .collect::<Result<Vec<_>, _>>()?,
            _ => self.slots().to_vec(),
        })
    }
    fn data(&self, anchor: BlockPos) -> Option<ComponentData> {
        Some(match self {
            Self::Workbench => return None,
            Self::Chest(c) => survival::chest_component(anchor, c),
            Self::Furnace(f) => survival::furnace_component(anchor, f),
            Self::Machine(m) => {
                let mut bytes = [0; machines::MACHINE_BYTES];
                let used = m.encode(&mut bytes);
                ComponentData {
                    anchor,
                    kind: MACHINE_COMPONENT,
                    payload: bytes[..used].to_vec(),
                }
            }
        })
    }
    fn set_slots(
        &mut self,
        slots: &[ItemStack],
        payloads: Option<&dyn Resolver>,
    ) -> Result<(), Error> {
        match self {
            Self::Workbench => {
                if !slots.is_empty() {
                    return Err(Error::Block);
                }
            }
            Self::Chest(c) => {
                for (i, &s) in slots.iter().enumerate() {
                    c.set_trusted(i, s).map_err(|_| Error::Encoding)?;
                }
            }
            Self::Furnace(f) => {
                for (i, &s) in slots.iter().enumerate() {
                    if f.slots()[i] != s {
                        f.set_trusted(i, s).map_err(|_| Error::Encoding)?;
                    }
                }
            }
            Self::Machine(m) => {
                for (i, &s) in slots.iter().enumerate() {
                    if m.slots[i].to_item().map_err(|_| Error::Encoding)?.0 != s {
                        m.slots[i] = if s.payload().is_some() {
                            machines::Slot::from_item(
                                s,
                                payloads.ok_or(Error::UnsupportedTypedItem)?,
                            )
                            .map_err(|_| Error::Encoding)?
                        } else {
                            machines::Slot::new(s)
                        };
                    }
                }
            }
        }
        Ok(())
    }
}
fn compatible(current: Cell, expected: Cell) -> bool {
    current.id() == expected.id()
        && rules::blocks::facing_of(current.raw()) == rules::blocks::facing_of(expected.raw())
}
fn known(world: &World, position: BlockPos) -> Result<Cell, Error> {
    match world.read(position) {
        CellRead::Known(c) => Ok(c),
        CellRead::Unknown => Err(Error::Unknown),
    }
}
fn access(
    world: &World,
    context: &Context<'_>,
    target: BlockPos,
    pages: &mut Vec<(PageId, Version)>,
) -> Result<(), Error> {
    if !context.permitted {
        return Err(Error::Protected);
    }
    if context.player.mode == GameMode::Spectator {
        return Err(Error::Spectator);
    }
    if context.space != target.space {
        return Err(Error::OutOfReach);
    }
    let eye = context.player.eye(1.0);
    let point = [target.x, target.y, target.z];
    let reach = if context.player.mode == GameMode::Creative || context.player.noclip {
        5.0
    } else {
        4.5
    };
    if (0..3)
        .map(|a| (f64::from(point[a]) + 0.5 - eye[a]).powi(2))
        .sum::<f64>()
        .sqrt()
        >= reach + 0.87
    {
        return Err(Error::OutOfReach);
    }
    // Visibility precedes comparing expected IDs, so hidden ores cannot be
    // queried through a Stale/Occluded difference.
    match sim::raycast(
        &WorldQuery::new(world, context.space),
        eye,
        context.player.look(),
        reach,
    ) {
        sim::RayResult::Hit(hit) if hit.cell == point => {}
        sim::RayResult::Unknown(_) => return Err(Error::Unknown),
        _ => return Err(Error::Occluded),
    }
    let from = eye.map(|v| (v.floor() as i32).div_euclid(16));
    let to = point.map(|v| v.div_euclid(16));
    for y in from[1].min(to[1])..=from[1].max(to[1]) {
        for z in from[2].min(to[2])..=from[2].max(to[2]) {
            for x in from[0].min(to[0])..=from[0].max(to[0]) {
                guard_page(
                    world,
                    PageId {
                        space: target.space,
                        x,
                        y,
                        z,
                    },
                    pages,
                );
            }
        }
    }
    Ok(())
}
fn guard_page(world: &World, page: PageId, pages: &mut Vec<(PageId, Version)>) {
    if !pages.iter().any(|(p, _)| *p == page) {
        pages.push((page, world.page_version(page)));
    }
}
pub(crate) fn fresh_component(
    world: &World,
    anchor: BlockPos,
    kind: u16,
) -> Result<ComponentId, Error> {
    for attempt in 0..64_u8 {
        let mut h = blake3::Hasher::new();
        h.update(b"window-block-entity-v1");
        h.update(&anchor.space.0.to_le_bytes());
        for value in [anchor.x, anchor.y, anchor.z] {
            h.update(&value.to_le_bytes());
        }
        h.update(&world.version().0.to_le_bytes());
        h.update(&kind.to_le_bytes());
        h.update(&[attempt]);
        let id = ComponentId(u64::from_le_bytes(
            h.finalize().as_bytes()[..8].try_into().unwrap(),
        ));
        if world.component(id).is_none() {
            return Ok(id);
        }
    }
    Err(Error::Capacity)
}
fn block(
    world: &World,
    anchor: BlockPos,
    cell: Cell,
    id: Option<ComponentId>,
) -> Result<Block, Error> {
    let definition = rules::block(cell.id()).ok_or(Error::Block)?;
    if cell.id() == rules::ids::CRAFTING_TABLE {
        if id.is_some() {
            return Err(Error::Component);
        }
        return Ok(Block::Workbench);
    }
    let expected_kind = match definition.be {
        Some(BeKind::Chest) => survival::CHEST_COMPONENT,
        Some(BeKind::Furnace) => survival::FURNACE_COMPONENT,
        Some(BeKind::Mek(_)) => MACHINE_COMPONENT,
        _ => return Err(Error::Block),
    };
    let bytes = if let Some(id) = id {
        let r = world.component(id).ok_or(Error::Stale)?;
        if r.data.anchor != anchor || r.data.kind != expected_kind {
            return Err(Error::Component);
        }
        Some(r.data.payload.as_slice())
    } else {
        None
    };
    Ok(match definition.be.unwrap() {
        BeKind::Chest => Block::Chest(Box::new(if let Some(b) = bytes {
            Chest::decode(b).map_err(|_| Error::Encoding)?
        } else {
            Chest::default()
        })),
        BeKind::Furnace => Block::Furnace(if let Some(b) = bytes {
            Furnace::decode(b).map_err(|_| Error::Encoding)?
        } else {
            Furnace::default()
        }),
        BeKind::Mek(kind) => {
            let m = if let Some(b) = bytes {
                Machine::decode(b).map_err(|_| Error::Encoding)?
            } else {
                Machine::new(kind)
            };
            if m.kind != kind {
                return Err(Error::Component);
            }
            Block::Machine(Box::new(m))
        }
    })
}
fn read_bound(world: &World, binding: Binding) -> Result<Block, Error> {
    if !compatible(known(world, binding.target)?, binding.expected_target)
        || !compatible(known(world, binding.anchor)?, binding.expected_anchor)
    {
        return Err(Error::Stale);
    }
    let id = world
        .page(binding.anchor.page())
        .ok_or(Error::Unknown)?
        .component(binding.anchor.local_index());
    if id != binding.component {
        return Err(Error::Stale);
    }
    block(world, binding.anchor, known(world, binding.anchor)?, id)
}
fn guard(g: net::window::WindowGuard) -> ui::WindowGuard {
    ui::WindowGuard {
        id: g.id,
        revision: g.revision,
        inventory_revision: g.inventory_revision,
    }
}
pub fn plan(
    world: &World,
    context: &Context<'_>,
    current: &BoundWindow,
    intent: WindowIntent,
    fresh_id: u64,
    now: Tick,
    book: &RecipeBook,
) -> Result<WindowPlan, Error> {
    net::window::validate_intent(intent).map_err(|_| Error::Encoding)?;
    let mut out = WindowPlan {
        before: current.clone(),
        after: current.clone(),
        before_inventory: *context.inventory,
        after_inventory: *context.inventory,
        drops: Vec::new(),
        transaction: None,
        pages: Vec::with_capacity(10),
        components: Vec::with_capacity(1),
        payloads: Vec::new(),
    };
    if matches!(
        intent,
        WindowIntent::OpenInventory | WindowIntent::OpenBlock { .. }
    ) {
        if fresh_id == 0 || fresh_id <= current.state.id() {
            return Err(Error::Stale);
        }
        let mut opened = None;
        if let WindowIntent::OpenBlock { position, expected } = intent {
            access(world, context, position, &mut out.pages)?;
            let target = known(world, position)?;
            if target != expected {
                return Err(Error::Stale);
            }
            let p = rules::blocks::main_of(target.raw(), [position.x, position.y, position.z]);
            let anchor = BlockPos {
                space: position.space,
                x: p[0],
                y: p[1],
                z: p[2],
            };
            guard_page(world, anchor.page(), &mut out.pages);
            let anchor_cell = known(world, anchor)?;
            let mut id = world
                .page(anchor.page())
                .ok_or(Error::Unknown)?
                .component(anchor.local_index());
            let value = block(world, anchor, anchor_cell, id)?;
            if let Some(data) = value.data(anchor) {
                if let Some(existing) = id {
                    out.components
                        .push((existing, world.component(existing).map(|r| r.version)));
                } else {
                    let new_id = fresh_component(world, anchor, data.kind)?;
                    let mut tx = Transaction::new(now, anchor.space);
                    tx.set_component(new_id, data, ChannelMask::CONTENTS);
                    out.transaction = Some(tx);
                    id = Some(new_id);
                    out.components.push((new_id, None));
                }
            }
            opened = Some((
                Binding {
                    target: position,
                    expected_target: target,
                    anchor,
                    expected_anchor: anchor_cell,
                    component: id,
                },
                value,
            ));
        }
        if !current.state.closed() {
            let close = current
                .state
                .plan_close(
                    &out.after_inventory,
                    current.state.guard(&out.after_inventory),
                )
                .map_err(Error::Window)?;
            out.drops.extend_from_slice(close.drops());
            close
                .apply(&mut out.after.state, &mut out.after_inventory)
                .map_err(Error::Window)?;
        }
        out.after = if let Some((binding, value)) = opened {
            BoundWindow {
                state: ui::WindowState::open(fresh_id, value.kind(), &value.stack_values()?)
                    .map_err(Error::Window)?,
                binding: Some(binding),
            }
        } else {
            BoundWindow::inventory(fresh_id)?
        };
    } else {
        let closing = matches!(intent, WindowIntent::Close { .. });
        let mut bound = if !closing {
            if let Some(binding) = current.binding {
                access(world, context, binding.target, &mut out.pages)?;
                guard_page(world, binding.anchor.page(), &mut out.pages);
                let value = read_bound(world, binding)?;
                if value.stack_values()? != current.state.container() {
                    return Err(Error::Stale);
                }
                if let Some(id) = binding.component {
                    out.components
                        .push((id, world.component(id).map(|r| r.version)));
                }
                Some(value)
            } else {
                if !matches!(current.state.kind(), ui::WindowKind::Inventory) {
                    return Err(Error::Stale);
                }
                None
            }
        } else {
            None
        };
        let ui_plan = match intent {
            WindowIntent::Click {
                guard: g,
                slot,
                right,
                shift,
            } => current.state.plan_click(
                context.inventory,
                guard(g),
                ui::Click { slot, right, shift },
            ),
            WindowIntent::RecipeFill {
                guard: g,
                recipe,
                all,
            } => current.state.plan_fill(
                context.inventory,
                guard(g),
                book,
                RecipeId::from_raw(recipe),
                all,
            ),
            WindowIntent::TakeCraft { guard: g, all } => {
                current
                    .state
                    .plan_take(context.inventory, guard(g), book, all)
            }
            WindowIntent::Close { guard: g } => {
                current.state.plan_close(context.inventory, guard(g))
            }
            WindowIntent::MachineSide {
                guard: g,
                face,
                mode,
            } => {
                let Some(Block::Machine(machine)) = bound.as_mut() else {
                    return Err(Error::Block);
                };
                let modes = [
                    SideMode::None,
                    SideMode::Input,
                    SideMode::Output,
                    SideMode::Both,
                    SideMode::Energy,
                    SideMode::Extra,
                ];
                let wanted = modes[mode as usize];
                let mut candidate = SideMode::None;
                let mut valid = false;
                for _ in 0..6 {
                    if candidate == wanted {
                        valid = true;
                        break;
                    }
                    candidate = machines::sides::cycle(machine.kind, candidate, false);
                }
                if !valid {
                    return Err(Error::Block);
                }
                machine.sides[face as usize] = wanted;
                current
                    .state
                    .plan_configuration(context.inventory, guard(g))
            }
            _ => return Err(Error::Encoding),
        }
        .map_err(Error::Window)?;
        let owned_transfer = ui_plan.container_changed();
        let changed = owned_transfer || matches!(intent, WindowIntent::MachineSide { .. });
        out.drops.extend_from_slice(ui_plan.drops());
        ui_plan
            .apply(&mut out.after.state, &mut out.after_inventory)
            .map_err(Error::Window)?;
        if owned_transfer && let Some(Block::Machine(machine)) = bound.as_ref() {
            for slot in machine
                .slots
                .iter()
                .take(machines::roles(machine.kind).len())
            {
                if let Some(payload) = slot.to_item().map_err(|_| Error::Encoding)?.1 {
                    let id = Some(payload.id());
                    if std::iter::once(out.after.state.cursor())
                        .chain(out.after.state.craft().cells().iter().copied())
                        .chain(out.after_inventory.slots().iter().copied())
                        .any(|stack| stack.payload() == id)
                    {
                        if context.payloads.is_none() {
                            return Err(Error::UnsupportedTypedItem);
                        }
                        out.payloads.push(payload);
                    }
                }
            }
        }
        if changed {
            let binding = current.binding.ok_or(Error::Component)?;
            let value = bound.as_mut().ok_or(Error::Component)?;
            value.set_slots(out.after.state.container(), context.payloads)?;
            if let Some(data) = value.data(binding.anchor) {
                let id = binding.component.ok_or(Error::Component)?;
                let mut tx = Transaction::new(now, binding.anchor.space);
                tx.set_component(id, data, ChannelMask::CONTENTS);
                out.transaction = Some(tx);
            }
        }
    }
    if let Some(tx) = out.transaction.as_mut() {
        for (page, version) in &out.pages {
            tx.expect_page(*page, *version);
        }
    }
    out.validate(world, current, context.inventory)?;
    Ok(out)
}
pub fn snapshot(
    world: &World,
    bound: &BoundWindow,
    inventory: &Inventory,
    book: &RecipeBook,
) -> Result<WindowSnapshot, Error> {
    let (kind, binding) = if bound.state.closed() {
        (net::window::WindowKind::Inventory, None)
    } else {
        (
            match bound.state.kind() {
                ui::WindowKind::Inventory => net::window::WindowKind::Inventory,
                ui::WindowKind::Workbench => net::window::WindowKind::Workbench,
                ui::WindowKind::Chest(n) => net::window::WindowKind::Chest(n),
                ui::WindowKind::Furnace => net::window::WindowKind::Furnace,
                ui::WindowKind::Machine(k) => net::window::WindowKind::Machine(mek_code(k)),
            },
            bound.binding.map(|b| b.anchor),
        )
    };
    let g = bound.state.guard(inventory);
    let mut craft = [net::window::Stack::EMPTY; 9];
    if !bound.state.closed() {
        for y in 0..bound.state.craft().height() {
            for x in 0..bound.state.craft().width() {
                craft[y * 3 + x] = stack(bound.state.craft().get(x, y).unwrap());
            }
        }
    }
    let mut result = WindowSnapshot {
        guard: net::window::WindowGuard {
            id: g.id,
            revision: g.revision,
            inventory_revision: g.inventory_revision,
        },
        kind,
        closed: bound.state.closed(),
        binding,
        cursor: stack(bound.state.cursor()),
        craft,
        craft_result: stack(bound.state.result(book)),
        container: if bound.state.closed() {
            Vec::new()
        } else {
            bound.state.container().iter().copied().map(stack).collect()
        },
        progress: 0,
        fuel: 0,
        energy: 0,
        sides: [0; 6],
    };
    if !bound.state.closed()
        && let Some(binding) = bound.binding
    {
        let value = read_bound(world, binding)?;
        if value.stack_values()? != bound.state.container() {
            return Err(Error::Stale);
        }
        match value {
            Block::Furnace(f) => {
                result.progress = (u32::from(f.cook()) * 1000 / u32::from(f.cook_total().max(1)))
                    .min(1000) as u16;
                result.fuel = (u32::from(f.burn()) * 1000 / u32::from(f.fuel_total().max(1)))
                    .min(1000) as u16;
            }
            Block::Machine(m) => {
                result.energy = m.energy;
                result.progress =
                    (u64::from(m.progress) * 1000 / u64::from(m.ticks().max(1))).min(1000) as u16;
                result.sides = m.sides.map(|s| s as u8);
            }
            _ => {}
        }
    }
    net::window::validate_snapshot(&result).map_err(|_| Error::Encoding)?;
    Ok(result)
}
fn stack(s: ItemStack) -> net::window::Stack {
    net::window::Stack {
        item: s.item(),
        count: s.count(),
        damage: s.damage(),
        payload: s.payload().map_or(0, |id| id.0),
    }
}
fn mek_code(kind: MekKind) -> u8 {
    match kind {
        MekKind::Heat => 0,
        MekKind::Infuser => 1,
        MekKind::Cube => 2,
        MekKind::Enrich => 3,
        MekKind::Smelter => 4,
        MekKind::Wind => 5,
        MekKind::Crusher => 6,
        MekKind::Purifier => 7,
        MekKind::Separator => 8,
        MekKind::Pump => 9,
    }
}
