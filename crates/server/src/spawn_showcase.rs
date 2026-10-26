//! The protected public spawn display uses ordinary World components and the
//! ordinary machine owner. Only its fixed authored coordinates remain active.
use super::*;
use core_types::ChannelMask;
use rules::{
    blocks::{BeKind, MekKind},
    crossroads::{Hall, PAVILIONS},
    ids::*,
    items::*,
    machines::{GasMode, Machine, Slot, Tank},
    stack::ItemStack,
    survival::furnace::Chest,
};

const REFILL_EVERY: u64 = 20;
const CHEST_KEEP: u32 = 48;
const CHANNELS: ChannelMask = ChannelMask(ChannelMask::CONTENTS.0 | ChannelMask::CONNECTIONS.0);

#[derive(Clone, Copy)]
struct Part {
    position: BlockPos,
    cell: Cell,
    hall: Hall,
    u: i32,
    source: bool,
}
impl Part {
    fn container(self) -> bool {
        self.cell.id() == CHEST
            || matches!(rules::blocks::bdef(self.cell.id()).be, Some(BeKind::Mek(_)))
    }
    fn matches(self, cell: Cell) -> bool {
        cell.id() == self.cell.id()
            && (!self.container()
                || rules::blocks::facing_of(cell.raw())
                    == rules::blocks::facing_of(self.cell.raw()))
            && (cell.id() != BOUNDING || cell.state() == self.cell.state())
    }
}
pub(super) struct Owner {
    pub(super) center: [i32; 3],
    parts: Vec<Part>,
    pages: Vec<PageId>,
    active: BTreeSet<PageId>,
    pull: BlockPos,
    ready: bool,
}
impl Owner {
    pub(super) fn new(config: &ServerConfig, center: [i32; 3]) -> Option<Self> {
        // Explicit fixture/custom spawns do not relocate the canonical square.
        // Never install an infinite display outside its protected spawn area.
        if !config.generation_enabled
            || config.spawn_override.is_some()
            || config.spawn_protection_radius < 32
        {
            return None;
        }
        let position = |offset: [i32; 2], u, v, height| BlockPos {
            space: SpaceId(0),
            x: center[0] + offset[0] + v,
            y: center[1] + height,
            z: center[2] + offset[1] - u,
        };
        let mut parts = BTreeMap::new();
        for pavilion in PAVILIONS {
            let hall = pavilion.hall;
            let mut add = |u, v, height, block, face, source| {
                let p = position(pavilion.offset, u, v, height);
                parts.insert(
                    p,
                    Part {
                        position: p,
                        cell: Cell::from_parts(block, face),
                        hall,
                        u,
                        source,
                    },
                );
            };
            let cable_end = if hall == Hall::Double { 1 } else { 0 };
            for u in -3..=cable_end {
                add(u, -1, 1, UNIVERSAL_CABLE, 0, false);
            }
            match hall {
                Hall::Wind => {
                    for u in -5..=-3 {
                        add(u, -1, 0, UNIVERSAL_CABLE, 0, false);
                    }
                    add(-5, -1, 1, WIND_GENERATOR, 5, false);
                    for h in 2..=5 {
                        add(-5, -1, h, BOUNDING, (h - 1) as u16, false);
                    }
                    for (u, block) in [(-1, ENRICHMENT_CHAMBER), (0, ENERGIZED_SMELTER), (1, CHEST)]
                    {
                        add(u, 0, 1, block, 5, false);
                    }
                }
                Hall::Steel => {
                    add(-4, -1, 1, HEAT_GENERATOR, 2, false);
                    for (u, block) in [
                        (-2, METALLURGIC_INFUSER),
                        (-1, METALLURGIC_INFUSER),
                        (0, ENERGIZED_SMELTER),
                        (1, CHEST),
                    ] {
                        add(u, 0, 1, block, 5, false);
                    }
                }
                Hall::Double => {
                    add(-7, 1, 1, CHEST, 0, true);
                    for u in -6..=-2 {
                        add(u, 1, 1, LOGISTICAL_TRANSPORTER, 0, false);
                    }
                    add(-4, -1, 1, ENERGY_CUBE, 2, false);
                    for (u, block) in [
                        (-3, ELECTROLYTIC_SEPARATOR),
                        (-2, PURIFICATION_CHAMBER),
                        (-1, CRUSHER),
                        (0, ENRICHMENT_CHAMBER),
                        (1, ENERGIZED_SMELTER),
                        (2, CHEST),
                    ] {
                        add(u, 0, 1, block, 5, false);
                    }
                }
            }
        }
        let parts: Vec<_> = parts.into_values().collect();
        let active = parts.iter().map(|p| p.position.page()).collect();
        // Fixed column halo is needed by actual topology/sky certification. All
        // work is requested asynchronously, never generated from a cell read.
        let mut columns = BTreeSet::new();
        for part in &parts {
            for dx in -1..=1 {
                for dz in -1..=1 {
                    columns.insert((
                        (part.position.x + dx).div_euclid(16),
                        (part.position.z + dz).div_euclid(16),
                    ));
                }
            }
        }
        let pages = columns
            .into_iter()
            .flat_map(|(x, z)| {
                (0..8).map(move |y| PageId {
                    space: SpaceId(0),
                    x,
                    y,
                    z,
                })
            })
            .collect();
        Some(Self {
            center,
            parts,
            pages,
            active,
            pull: position(
                PAVILIONS
                    .iter()
                    .find(|p| p.hall == Hall::Double)
                    .expect("authored doubling pavilion")
                    .offset,
                -6,
                1,
                1,
            ),
            ready: false,
        })
    }
    pub(super) fn active_pages(&self) -> impl Iterator<Item = PageId> + '_ {
        self.active.iter().copied()
    }
    pub(super) fn contains_window(&self, world: &World, position: BlockPos) -> bool {
        // Protection belongs to the authored site from the first known cell.
        // Other showcase pages may still be generating after Store recovery.
        if position.space != SpaceId(0) {
            return false;
        }
        let CellRead::Known(cell) = world.read(position) else {
            return false;
        };
        let p = rules::blocks::main_of(cell.raw(), [position.x, position.y, position.z]);
        self.parts.iter().any(|part| {
            part.container()
                && part.position.x == p[0]
                && part.position.y == p[1]
                && part.position.z == p[2]
        })
    }
}
impl Server {
    /// Public authored landmark, never a private seed or current player pose.
    pub fn showcase_center(&self) -> Option<[i32; 3]> {
        self.spawn_showcase.as_ref().map(|owner| owner.center)
    }
    pub(super) fn showcase_window(&self, position: BlockPos) -> bool {
        self.spawn_showcase
            .as_ref()
            .is_some_and(|owner| owner.contains_window(&self.world, position))
    }
    pub(super) fn tick_showcase(&mut self, batch: &mut Batch) -> Result<(), ServerError> {
        let Some(mut owner) = self.spawn_showcase.take() else {
            return Ok(());
        };
        let result = self.tick_showcase_inner(&mut owner, batch);
        self.spawn_showcase = Some(owner);
        result
    }
    fn tick_showcase_inner(
        &mut self,
        owner: &mut Owner,
        batch: &mut Batch,
    ) -> Result<(), ServerError> {
        if !owner.ready {
            let mut pending = false;
            for &page in &owner.pages {
                if self.world.page(page).is_none() {
                    self.request_generation(page);
                    pending = true;
                }
            }
            if pending {
                return Ok(());
            }
        } else if !self.tick.0.is_multiple_of(REFILL_EVERY) {
            return Ok(());
        }
        let mut tx = Transaction::new(self.tick, SpaceId(0));
        let mut changed = false;
        for &part in &owner.parts {
            let current = self.world.read(part.position);
            let matching = matches!(current, CellRead::Known(cell) if part.matches(cell));
            if !matching {
                tx.set_cell(part.position, part.cell);
                changed = true;
            }
            let previous = self
                .world
                .page(part.position.page())
                .and_then(|p| p.component(part.position.local_index()));
            if !part.container() {
                if let Some(id) = previous {
                    tx.remove_component(id, CHANNELS);
                    changed = true;
                }
                continue;
            }
            let data = previous.and_then(|id| self.world.component(id));
            let valid = matching
                && data.is_some_and(|record| {
                    if part.cell.id() == CHEST {
                        record.data.kind == survival::CHEST_COMPONENT
                            && Chest::decode(&record.data.payload).is_ok()
                    } else if let Some(BeKind::Mek(kind)) = rules::blocks::bdef(part.cell.id()).be {
                        record.data.kind == machines::MACHINE_COMPONENT
                            && Machine::decode(&record.data.payload).is_ok_and(|m| m.kind == kind)
                    } else {
                        false
                    }
                });
            let id = if let Some(id) = previous {
                id
            } else {
                windows::fresh_component(
                    &self.world,
                    part.position,
                    if part.cell.id() == CHEST {
                        survival::CHEST_COMPONENT
                    } else {
                        machines::MACHINE_COMPONENT
                    },
                )
                .map_err(|_| ServerError::Invalid("showcase component capacity"))?
            };
            let refill = !valid || self.tick.0.is_multiple_of(REFILL_EVERY);
            let after = if part.cell.id() == CHEST {
                let mut chest = if valid {
                    Chest::decode(&data.expect("valid component").data.payload)
                        .expect("validated chest")
                } else {
                    Chest::default()
                };
                let before = chest;
                if refill && part.source {
                    let total: u32 = chest.slots().iter().map(|s| u32::from(s.count())).sum();
                    if total < 16 {
                        chest
                            .set_trusted(0, ItemStack::new(I_RAW_IRON, 32).expect("authored stack"))
                            .expect("chest slot");
                    }
                } else if refill
                    && !part.source
                    && chest
                        .slots()
                        .iter()
                        .map(|s| u32::from(s.count()))
                        .sum::<u32>()
                        > CHEST_KEEP
                {
                    chest = Chest::default();
                }
                (!valid || chest != before)
                    .then(|| survival::chest_component(part.position, &chest))
            } else {
                let Some(BeKind::Mek(kind)) = rules::blocks::bdef(part.cell.id()).be else {
                    unreachable!("authored machine");
                };
                let mut machine = if valid {
                    Machine::decode(&data.expect("valid component").data.payload)
                        .expect("validated machine")
                } else {
                    let mut m = Machine::new(kind);
                    m.eject = !matches!(
                        kind,
                        MekKind::Heat | MekKind::Wind | MekKind::Cube | MekKind::Separator
                    );
                    m
                };
                let before = machine.clone();
                if refill {
                    match kind {
                        MekKind::Heat => top_up(&mut machine.slots[0], I_COAL, 16),
                        MekKind::Cube => machine.energy = machine.capacity(),
                        MekKind::Enrich if part.hall == Hall::Wind => {
                            top_up(&mut machine.slots[0], I_RAW_IRON, 12)
                        }
                        MekKind::Infuser => {
                            top_up(&mut machine.slots[0], I_COAL, 16);
                            if part.u == -2 {
                                top_up(&mut machine.slots[1], I_IRON_INGOT, 12);
                            }
                        }
                        MekKind::Separator => {
                            if machine.tank.amount < 12000 {
                                machine.tank = Tank {
                                    substance: rules::machine_recipes::SUB_WATER,
                                    amount: 12000,
                                };
                            }
                            machine.gas_mode[0] = GasMode::Excess;
                        }
                        _ => {}
                    }
                }
                (!valid || machine != before)
                    .then(|| machines::machine_component(part.position, &machine))
            };
            if let Some(data) = after {
                tx.set_component(
                    id,
                    data,
                    if valid {
                        ChannelMask::CONTENTS
                    } else {
                        CHANNELS
                    },
                );
                changed = true;
            }
        }
        if changed {
            batch.changes.push(
                self.world
                    .commit(tx)
                    .map_err(|_| ServerError::Invalid("showcase world admission"))?,
            );
        }
        owner.ready = true;
        Ok(())
    }
    /// Pull-side configuration is its own ordinary durable topology boundary,
    /// preceding the next global tick so it cannot collide with a kind4 delta.
    pub(super) fn finish_showcase_boundary(&mut self) -> Result<bool, ServerError> {
        let Some(owner) = self.spawn_showcase.as_ref().filter(|o| o.ready) else {
            return Ok(false);
        };
        let position = owner.pull;
        let modes = 2 << (2 * 3); // SOUTH: previous local-u source chest.
        let desired = machines::TransmitterConfig {
            position,
            block: LOGISTICAL_TRANSPORTER,
            modes,
            color: 0,
        };
        if self.runtimes.machines.transmitter_config(position) == Some(desired) {
            return Ok(false);
        }
        let plan = match self.runtimes.machines.plan_connections(
            &self.world,
            desired,
            self.tick,
            machines::Budget::default(),
        ) {
            Ok(plan) => plan,
            Err(machines::MachineError::Unknown | machines::MachineError::Bounds) => {
                return Ok(false);
            }
            Err(_) => return Err(ServerError::Invalid("showcase pull topology")),
        };
        self.runtimes
            .machines
            .validate(&self.world, &plan)
            .map_err(|_| ServerError::Invalid("showcase topology guard"))?;
        let mut batch = Batch::empty(self.tick);
        for tx in plan.transactions {
            batch.changes.push(
                self.world
                    .commit(tx)
                    .map_err(|_| ServerError::Invalid("showcase topology commit"))?,
            );
        }
        batch.runtimes.push(save::RuntimeRecord {
            kind: save::RUNTIME_MACHINES,
            payload: plan
                .delta
                .encode()
                .map_err(|_| ServerError::Invalid("showcase topology codec"))?,
        });
        self.prepare_light_batch(&mut batch)?;
        self.submit_batch(batch, Vec::new());
        Ok(true)
    }
}
fn top_up(slot: &mut Slot, item: u16, count: u16) {
    if slot.stack.is_empty() || slot.stack.item() == item && slot.stack.count() < count {
        *slot = Slot::new(ItemStack::new(item, count).expect("authored showcase stack"));
    }
}

#[cfg(test)]
mod tests;
