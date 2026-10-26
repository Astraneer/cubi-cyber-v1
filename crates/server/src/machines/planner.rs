//! Bounded live connection edits. Reads and copies only touched components.
use super::*;
use machines::grid::{Endpoint, TopologyBudget, TopologyEdit};

struct Reads<'a> {
    world: &'a World,
    budget: Budget,
    cells: BTreeMap<BlockPos, Cell>,
    inputs: BTreeMap<PageId, Option<Version>>,
    work: Work,
}
impl Reads<'_> {
    fn cell(&mut self, p: BlockPos) -> Result<Cell, MachineError> {
        if let Some(&cell) = self.cells.get(&p) {
            return Ok(cell);
        }
        if self.work.reads >= self.budget.reads {
            return Err(MachineError::Bounds);
        }
        self.work.reads += 1;
        self.inputs.entry(p.page()).or_insert_with(|| {
            self.world
                .page(p.page())
                .map(|_| self.world.input_version(p.page(), CHANNELS))
        });
        let CellRead::Known(cell) = self.world.read(p) else {
            return Err(MachineError::Unknown);
        };
        self.cells.insert(p, cell);
        Ok(cell)
    }
    fn machine_id(&mut self, p: BlockPos) -> Result<Option<ComponentId>, MachineError> {
        if !matches!(blocks::bdef(self.cell(p)?.id()).be, Some(BeKind::Mek(_))) {
            return Ok(None);
        }
        self.world
            .page(p.page())
            .and_then(|page| page.component(p.local_index()))
            .map(Some)
            .ok_or(MachineError::Component)
    }
}
fn resource(block: u16) -> Option<Resource> {
    match block {
        UNIVERSAL_CABLE => Some(Resource::Energy),
        MECHANICAL_PIPE => Some(Resource::Fluid),
        PRESSURIZED_TUBE => Some(Resource::Gas),
        _ => None,
    }
}
fn transmitter(block: u16) -> bool {
    resource(block).is_some() || block == LOGISTICAL_TRANSPORTER
}
fn ports(cell: Cell, face: Face, kind: Resource) -> (bool, bool) {
    let Some(BeKind::Mek(machine)) = blocks::bdef(cell.id()).be else {
        return (false, false);
    };
    match kind {
        Resource::Energy => sides::energy_io(
            machine,
            Face::ALL[blocks::facing_of(cell.raw()) as usize],
            face,
        ),
        Resource::Fluid => (
            matches!(machine, MekKind::Separator | MekKind::Heat),
            machine == MekKind::Pump && face == Face::Up,
        ),
        Resource::Gas => (machine == MekKind::Purifier, machine == MekKind::Separator),
    }
}
fn inventory(cell: Cell) -> bool {
    match blocks::bdef(cell.id()).be {
        Some(BeKind::Chest | BeKind::Furnace) => true,
        Some(BeKind::Mek(kind)) => !matches!(kind, MekKind::Heat | MekKind::Wind | MekKind::Pump),
        None => false,
    }
}
impl MachineRuntime {
    /// Reconcile accepted cell edits before running the resource phase. The
    /// caller persists this detached plan before publishing connection changes.
    pub fn plan_topology(
        &self,
        world: &World,
        changed: &[BlockPos],
        now: Tick,
        budget: Budget,
    ) -> Result<PhasePlan, MachineError> {
        self.plan_connections_inner(world, changed, None, now, budget)
    }
    /// Modes are NORMAL=0, PUSH=1, PULL=2, NONE=3, packed in Face::ALL order.
    pub fn plan_connections(
        &self,
        world: &World,
        config: TransmitterConfig,
        now: Tick,
        budget: Budget,
    ) -> Result<PhasePlan, MachineError> {
        validate_config(config)?;
        self.plan_connections_inner(world, &[config.position], Some(config), now, budget)
    }
    fn plan_connections_inner(
        &self,
        world: &World,
        changed: &[BlockPos],
        supplied: Option<TransmitterConfig>,
        now: Tick,
        budget: Budget,
    ) -> Result<PhasePlan, MachineError> {
        if changed.len() > 64 {
            return Err(MachineError::Bounds);
        }
        let mut reads = Reads {
            world,
            budget,
            cells: BTreeMap::new(),
            inputs: BTreeMap::new(),
            work: Work::default(),
        };
        let mut affected = BTreeSet::new();
        for &p in changed {
            affected.insert(p);
            for face in Face::ALL {
                affected.insert(offset(p, face)?);
            }
        }
        if affected.len() > 448 {
            return Err(MachineError::Bounds);
        }
        let mut configs = BTreeMap::new();
        let mut edits = BTreeMap::<SpaceId, Vec<TopologyEdit>>::new();
        let mut arm_values = BTreeMap::new();
        let mut keys = BTreeSet::new();
        let mut machine_ids = BTreeSet::new();
        for &p in &affected {
            let cell = reads.cell(p)?;
            if let Some(id) = reads.machine_id(p)? {
                machine_ids.insert(id);
            }
            let before = self.transmitter_config(p);
            let after = transmitter(cell.id()).then(|| {
                supplied
                    .filter(|c| c.position == p)
                    .or(before.filter(|c| c.block == cell.id()))
                    .unwrap_or(TransmitterConfig {
                        position: p,
                        block: cell.id(),
                        modes: 0,
                        color: 0,
                    })
            });
            if supplied.is_some_and(|c| c.position == p && c.block != cell.id()) {
                return Err(MachineError::Component);
            }
            if before != after {
                configs.insert(p, ConfigPatch { before, after });
            }
            if let Some(key) = self.network_at(p) {
                keys.insert(key);
            }
            if let Some(config) = after {
                let arms = self.derive_arms(&mut reads, config, supplied)?;
                arm_values.insert(p, arms);
            }
        }
        for &p in &affected {
            let cell = reads.cell(p)?;
            let old = self
                .grids
                .get(&p.space)
                .and_then(|g| g.node([p.x, p.y, p.z]));
            let mut links = 0;
            if resource(cell.id()).is_some() {
                let a = arm_values[&p];
                for face in Face::ALL {
                    if arms::arm(a, face as u8) == 1
                        && reads.cell(offset(p, face)?)?.id() == cell.id()
                    {
                        links |= 1 << face as u8;
                    }
                }
            }
            let target = resource(cell.id());
            let list = edits.entry(p.space).or_default();
            if let Some(old) = &old
                && target != Some(old.resource)
            {
                list.push(TopologyEdit::Remove {
                    position: old.position,
                });
            }
            match (old, target) {
                (None, Some(resource)) => list.push(TopologyEdit::Add {
                    position: [p.x, p.y, p.z],
                    resource,
                    links,
                }),
                (Some(old), Some(resource)) if old.resource != resource => {
                    list.push(TopologyEdit::Add {
                        position: old.position,
                        resource,
                        links,
                    })
                }
                (Some(old), Some(_)) if old.links != links => list.push(TopologyEdit::Relink {
                    position: old.position,
                    links,
                }),
                _ => {}
            }
            if list.len() > 64 {
                return Err(MachineError::Bounds);
            }
        }
        let mut topology = Vec::new();
        for (space, edits) in edits {
            if edits.is_empty() {
                continue;
            }
            let empty = Grid::new(self.config.transmitters, self.config.endpoints);
            let grid = self.grids.get(&space).unwrap_or(&empty);
            let mut work = GridWork::default();
            let change = grid.plan_edits(
                &edits,
                TopologyBudget {
                    nodes: budget.reads,
                    endpoints: budget.endpoints as u64,
                    bytes: budget.bytes,
                },
                &mut work,
            )?;
            reads.work.bytes = reads.work.bytes.saturating_add(
                work.topology_nodes
                    .saturating_mul(std::mem::size_of::<machines::grid::NodeSnapshot>() as u64),
            );
            keys.extend(
                change
                    .affected_before()
                    .chain(change.affected_after())
                    .map(|network| NetworkKey { space, network }),
            );
            topology.push(TopologyPatch {
                space,
                records: change.records().clone(),
            });
        }
        let mut delta = self.delta(Vec::new(), Vec::new(), Vec::new(), self.next_order)?;
        delta.topology = topology;
        let mut endpoints = Vec::new();
        let mut network_data = Vec::new();
        for key in keys {
            let Ok(network) = self.projected_network(key, &delta) else {
                network_data.push((key, None));
                continue;
            };
            let kind = network.resource();
            let mut entries = Vec::new();
            let mut producers = BTreeSet::new();
            for &member in network.members() {
                let p = BlockPos {
                    space: key.space,
                    x: member[0],
                    y: member[1],
                    z: member[2],
                };
                let cell = reads.cell(p)?;
                let config = supplied
                    .filter(|c| c.position == p)
                    .or_else(|| self.transmitter_config(p).filter(|c| c.block == cell.id()))
                    .unwrap_or(TransmitterConfig {
                        position: p,
                        block: cell.id(),
                        modes: 0,
                        color: 0,
                    });
                let a = if let Some(&a) = arm_values.get(&p) {
                    a
                } else {
                    self.derive_arms(&mut reads, config, supplied)?
                };
                for face in Face::ALL {
                    let mode = arms::arm(a, face as u8);
                    if mode == 0 {
                        continue;
                    }
                    let q = offset(p, face)?;
                    let target = reads.cell(q)?;
                    let (input, output) = ports(target, face.opposite(), kind);
                    if !input && !output {
                        continue;
                    }
                    let id = reads.machine_id(q)?.ok_or(MachineError::Component)?;
                    machine_ids.insert(id);
                    if output {
                        producers.insert(id);
                    }
                    let pull = mode == 3;
                    if input && !pull || output && pull {
                        entries.push(Endpoint {
                            owner: id.0,
                            position: [q.x, q.y, q.z],
                            face: face.opposite(),
                            pull,
                        });
                    }
                    reads.work.endpoint_queries += 1;
                    if reads.work.endpoint_queries > budget.endpoints as u64 {
                        return Err(MachineError::Bounds);
                    }
                }
            }
            entries.sort_unstable_by_key(|e| (e.owner, e.pull, e.position, e.face as u8));
            entries.dedup_by_key(|e| (e.owner, e.pull));
            let before = network.endpoints().to_vec();
            if before != entries {
                endpoints.push(EndpointPatch {
                    key,
                    before,
                    after: entries,
                });
            }
            network_data.push((key, Some(producers.into_iter().collect::<Vec<_>>())));
        }
        // Register newly attached components before validating endpoint owners.
        let planned_topology = std::mem::take(&mut delta.topology);
        delta = self.extend_registration(delta, world, machine_ids, now)?;
        delta.topology = planned_topology;
        delta.configs = configs.into_values().collect();
        delta.endpoints = endpoints;
        let due = Tick(now.0.checked_add(1).ok_or(MachineError::Overflow)?);
        for (key, producers) in network_data {
            let before = self.network_meta(key);
            let after = if let Some(producers) = producers {
                let order = if let Some(meta) = &before {
                    meta.order
                } else {
                    let order = delta.next_order_after;
                    delta.next_order_after = order.checked_add(1).ok_or(MachineError::Overflow)?;
                    order
                };
                Some(NetworkMeta {
                    key,
                    order,
                    producers,
                })
            } else {
                None
            };
            let event = Event::Network(key);
            let old = self.pending(event);
            let next = after.as_ref().map(|m| {
                old.map_or(
                    Pending {
                        event,
                        due,
                        first_due: due,
                        order: m.order,
                    },
                    |old| Pending {
                        due: old.due.min(due),
                        first_due: old.first_due.min(due),
                        ..old
                    },
                )
            });
            if before != after {
                delta.networks.push(NetworkPatch { before, after });
            }
            if old != next {
                delta.queue.push(QueuePatch {
                    before: old,
                    after: next,
                });
            }
        }
        let mut transactions = BTreeMap::new();
        for (p, a) in arm_values {
            let cell = reads.cell(p)?;
            let state = arms::with_arms(cell.state(), a);
            if cell.state() != state {
                transactions
                    .entry(p.space)
                    .or_insert_with(|| Transaction::new(now, p.space))
                    .expect_page(p.page(), world.page_version(p.page()))
                    .set_cell(p, Cell::from_parts(cell.id(), state));
            }
        }
        let delta = self.register_pulls(delta, world, affected, now)?;
        self.validate_delta(&delta)?;
        Ok(PhasePlan {
            transactions: transactions.into_values().collect(),
            delta,
            inputs: reads
                .inputs
                .into_iter()
                .map(|(page, version)| jobs::InputVersion { page, version })
                .collect(),
            fluid_wakes: Vec::new(),
            payloads: Vec::new(),
            item_spawns: Vec::new(),
            work: reads.work,
        })
    }
    fn derive_arms(
        &self,
        reads: &mut Reads<'_>,
        config: TransmitterConfig,
        supplied: Option<TransmitterConfig>,
    ) -> Result<u16, MachineError> {
        let mut result = 0;
        for face in Face::ALL {
            let mode = arms::arm(config.modes, face as u8);
            if mode == 3 {
                continue;
            }
            let q = offset(config.position, face)?;
            let neighbor = reads.cell(q)?;
            let code = if neighbor.id() == config.block {
                let other = supplied
                    .filter(|c| c.position == q)
                    .or_else(|| {
                        self.transmitter_config(q)
                            .filter(|c| c.block == neighbor.id())
                    })
                    .unwrap_or(TransmitterConfig {
                        position: q,
                        block: neighbor.id(),
                        modes: 0,
                        color: 0,
                    });
                if arms::arm(other.modes, face.opposite() as u8) != 3
                    && (config.block != LOGISTICAL_TRANSPORTER
                        || config.color == 0
                        || other.color == 0
                        || config.color == other.color)
                {
                    1
                } else {
                    0
                }
            } else {
                let accepts = resource(config.block).map_or_else(
                    || inventory(neighbor),
                    |r| {
                        let (i, o) = ports(neighbor, face.opposite(), r);
                        i || o
                    },
                );
                if accepts {
                    match mode {
                        1 => 2,
                        2 => 3,
                        _ => 1,
                    }
                } else {
                    0
                }
            };
            result |= code << (2 * face as u8);
        }
        Ok(result)
    }
}
