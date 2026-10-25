use super::*;

/// Event-local writes are merged only after every dependency remained known.
/// A failed pump/ray/route discards this overlay, preserving prior events.
pub(super) struct Stage<'a> {
    pub(super) space: SpaceId,
    pub(super) owner: ReadOwner<'a>,
    pub(super) world: &'a World,
    pub(super) parent: &'a Overlay,
    pub(super) local: Overlay,
    pub(super) work: &'a mut Work,
    pub(super) budget: Budget,
    pub(super) error: Option<MachineError>,
    pub(super) wake_machines: BTreeSet<ComponentId>,
    pub(super) wake_networks: BTreeSet<NetworkKey>,
}
impl<'a> Stage<'a> {
    fn current_space(&self) -> SpaceId {
        self.space
    }
    pub(super) fn new(
        owner: ReadOwner<'a>,
        world: &'a World,
        parent: &'a Overlay,
        work: &'a mut Work,
        budget: Budget,
        space: SpaceId,
    ) -> Self {
        Self {
            space,
            owner,
            world,
            parent,
            local: Overlay::default(),
            work,
            budget,
            error: None,
            wake_machines: BTreeSet::new(),
            wake_networks: BTreeSet::new(),
        }
    }
    pub(super) fn cell(&mut self, p: BlockPos) -> Result<Cell, MachineError> {
        if self.work.reads >= self.budget.reads {
            return Err(MachineError::Bounds);
        }
        self.work.reads += 1;
        self.local.inputs.entry(p.page()).or_insert_with(|| {
            self.world
                .page(p.page())
                .map(|_| self.world.input_version(p.page(), CHANNELS))
        });
        if let Some((_, value)) = self
            .local
            .cells
            .get(&p)
            .or_else(|| self.parent.cells.get(&p))
        {
            return Ok(*value);
        }
        match self.world.read(p) {
            CellRead::Known(value) => Ok(value),
            CellRead::Unknown => Err(MachineError::Unknown),
        }
    }
    fn component_at(&mut self, p: BlockPos) -> Result<Option<ComponentId>, MachineError> {
        self.cell(p)?;
        Ok(self
            .world
            .page(p.page())
            .and_then(|page| page.component(p.local_index())))
    }
    fn ensure_machine(&mut self, id: ComponentId) -> Result<(), MachineError> {
        if self.local.machines.contains_key(&id) {
            return Ok(());
        }
        if self
            .work
            .bytes
            .saturating_add(2 * machines::MACHINE_BYTES as u64)
            > self.budget.bytes
        {
            return Err(MachineError::Bounds);
        }
        self.work.bytes += 2 * machines::MACHINE_BYTES as u64;
        let change = if let Some(change) = self.parent.machines.get(&id) {
            change.clone()
        } else {
            let record = self.world.component(id).ok_or(MachineError::Missing)?;
            if record.data.kind != MACHINE_COMPONENT {
                return Err(MachineError::Component);
            }
            let anchor = record.data.anchor;
            let version = record.version;
            let before =
                Machine::decode(&record.data.payload).map_err(|_| MachineError::Encoding)?;
            let cell = self.cell(anchor)?;
            if !matches!(blocks::bdef(cell.id()).be,Some(BeKind::Mek(kind)) if kind==before.kind) {
                return Err(MachineError::Component);
            }
            MachineChange {
                version,
                before: before.clone(),
                after: before,
            }
        };
        self.local.machines.insert(id, change);
        Ok(())
    }
    pub(super) fn machine(&mut self, id: ComponentId) -> Result<Machine, MachineError> {
        self.ensure_machine(id)?;
        Ok(self
            .local
            .machines
            .get(&id)
            .expect("ensured machine")
            .after
            .clone())
    }
    fn machine_mut(&mut self, id: ComponentId) -> Result<&mut Machine, MachineError> {
        self.ensure_machine(id)?;
        Ok(&mut self
            .local
            .machines
            .get_mut(&id)
            .expect("ensured machine")
            .after)
    }
    fn ensure_chest(&mut self, id: ComponentId) -> Result<(), MachineError> {
        if self.local.chests.contains_key(&id) {
            return Ok(());
        }
        if self
            .work
            .bytes
            .saturating_add(2 * machines::MACHINE_BYTES as u64)
            > self.budget.bytes
        {
            return Err(MachineError::Bounds);
        }
        self.work.bytes += 2 * rules::survival::furnace::CHEST_BYTES as u64;
        let change = if let Some(change) = self.parent.chests.get(&id) {
            *change
        } else {
            let record = self.world.component(id).ok_or(MachineError::Missing)?;
            if record.data.kind != crate::survival::CHEST_COMPONENT {
                return Err(MachineError::Component);
            }
            let anchor = record.data.anchor;
            let version = record.version;
            let before = Chest::decode(&record.data.payload).map_err(|_| MachineError::Encoding)?;
            if self.cell(anchor)?.id() != CHEST {
                return Err(MachineError::Component);
            }
            ChestChange {
                version,
                before,
                after: before,
                anchor,
            }
        };
        self.local.chests.insert(id, change);
        Ok(())
    }
    pub(super) fn reservoir(&mut self, key: NetworkKey) -> Result<Reservoir, MachineError> {
        if let Some((_, after)) = self
            .local
            .reservoirs
            .get(&key)
            .or_else(|| self.parent.reservoirs.get(&key))
        {
            return Ok(*after);
        }
        Ok(self
            .owner
            .grids
            .get(&key.space)
            .ok_or(MachineError::Missing)?
            .network(key.network)?
            .reservoir())
    }
    fn set_reservoir(&mut self, key: NetworkKey, after: Reservoir) -> Result<(), MachineError> {
        let before = self
            .parent
            .reservoirs
            .get(&key)
            .map(|(before, _)| *before)
            .unwrap_or(
                self.owner
                    .grids
                    .get(&key.space)
                    .ok_or(MachineError::Missing)?
                    .network(key.network)?
                    .reservoir(),
            );
        self.local.reservoirs.insert(key, (before, after));
        self.wake_networks.insert(key);
        Ok(())
    }
    fn push_energy_result(
        &mut self,
        p: BlockPos,
        face: Face,
        amount: u64,
    ) -> Result<u64, MachineError> {
        if amount == 0 {
            return Ok(0);
        }
        let q = offset(p, face)?;
        let cell = self.cell(q)?;
        if cell.id() == UNIVERSAL_CABLE
            && arms::arm(arms::arms_of(cell.state()), face.opposite() as u8) != 0
        {
            let grid = self
                .owner
                .grids
                .get(&q.space)
                .ok_or(MachineError::Missing)?;
            let key = NetworkKey {
                space: q.space,
                network: grid
                    .network_at([q.x, q.y, q.z])
                    .ok_or(MachineError::Missing)?,
            };
            let mut state = self.reservoir(key)?;
            let accepted = amount.min(
                grid.network(key.network)?
                    .capacity()
                    .saturating_sub(state.stored),
            );
            state.stored += accepted;
            self.set_reservoir(key, state)?;
            return Ok(accepted);
        }
        let Some(id) = self.component_at(q)? else {
            return Ok(0);
        };
        if self
            .world
            .component(id)
            .is_none_or(|record| record.data.kind != MACHINE_COMPONENT)
        {
            return Ok(0);
        }
        if !matches!(blocks::bdef(cell.id()).be,Some(BeKind::Mek(kind)) if sides::energy_io(kind,Face::ALL[blocks::facing_of(cell.raw()) as usize],face.opposite()).0)
        {
            return Ok(0);
        }
        if !self.owner.machines.get(&id).is_some_and(|m| m.active) {
            return Ok(0);
        }
        let accepted = self.machine_mut(id)?.insert_energy(amount);
        if accepted > 0 {
            self.wake_machines.insert(id);
        }
        Ok(accepted)
    }
    fn push_items_result(
        &mut self,
        p: BlockPos,
        face: Face,
        stack: ItemStack,
    ) -> Result<u16, MachineError> {
        if stack.is_empty() {
            return Ok(0);
        }
        let q = offset(p, face)?;
        let cell = self.cell(q)?;
        let Some(id) = self.component_at(q)? else {
            return Ok(0);
        };
        let kind = self
            .world
            .component(id)
            .ok_or(MachineError::Missing)?
            .data
            .kind;
        if kind == MACHINE_COMPONENT {
            if !self.owner.machines.get(&id).is_some_and(|m| m.active) {
                return Ok(0);
            }
            let facing = Face::ALL[blocks::facing_of(cell.raw()) as usize];
            let accepted = self.machine_mut(id)?.insert_item(
                facing,
                face.opposite(),
                machines::Slot::new(stack),
                false,
            );
            if accepted > 0 {
                self.wake_machines.insert(id);
            }
            return Ok(accepted);
        }
        if kind == crate::survival::CHEST_COMPONENT {
            self.ensure_chest(id)?;
            let chest = &mut self.local.chests.get_mut(&id).expect("ensured chest").after;
            let mut left = stack.count();
            for matching in [true, false] {
                for index in 0..27 {
                    let old = chest.slots()[index];
                    if (matching && (old.is_empty() || !old.same_kind(stack)))
                        || (!matching && !old.is_empty())
                    {
                        continue;
                    }
                    let accepted = left.min(stack.maximum().saturating_sub(old.count()));
                    if accepted > 0 {
                        chest
                            .set_trusted(
                                index,
                                stack
                                    .with_count(old.count() + accepted)
                                    .map_err(|_| MachineError::Encoding)?,
                            )
                            .map_err(|_| MachineError::Overflow)?;
                        left -= accepted;
                    }
                    if left == 0 {
                        return Ok(stack.count());
                    }
                }
            }
            return Ok(stack.count() - left);
        }
        Ok(0)
    }
    fn push_substance_result(
        &mut self,
        p: BlockPos,
        face: Face,
        substance: u8,
        amount: u32,
    ) -> Result<u32, MachineError> {
        if amount == 0 {
            return Ok(0);
        }
        let q = offset(p, face)?;
        let cell = self.cell(q)?;
        if [MECHANICAL_PIPE, PRESSURIZED_TUBE].contains(&cell.id())
            && arms::arm(arms::arms_of(cell.state()), face.opposite() as u8) != 0
        {
            let grid = self
                .owner
                .grids
                .get(&q.space)
                .ok_or(MachineError::Missing)?;
            let key = NetworkKey {
                space: q.space,
                network: grid
                    .network_at([q.x, q.y, q.z])
                    .ok_or(MachineError::Missing)?,
            };
            let network = grid.network(key.network)?;
            let mut state = self.reservoir(key)?;
            let valid = match network.resource {
                Resource::Fluid => [3, 6].contains(&substance),
                Resource::Gas => [4, 5].contains(&substance),
                Resource::Energy => false,
            };
            if !valid || state.substance != 0 && state.substance != substance {
                return Ok(0);
            }
            let accepted =
                u64::from(amount).min(network.capacity().saturating_sub(state.stored)) as u32;
            if accepted > 0 {
                state.substance = substance;
                state.stored += u64::from(accepted);
                self.set_reservoir(key, state)?;
            }
            return Ok(accepted);
        }
        let Some(id) = self.component_at(q)? else {
            return Ok(0);
        };
        if self
            .world
            .component(id)
            .is_none_or(|record| record.data.kind != MACHINE_COMPONENT)
        {
            return Ok(0);
        }
        if !self.owner.machines.get(&id).is_some_and(|m| m.active) {
            return Ok(0);
        }
        let accepted = self
            .machine_mut(id)?
            .insert_substance(substance, amount, false);
        if accepted > 0 {
            self.wake_machines.insert(id);
        }
        Ok(accepted)
    }
    fn callback<T: Default>(&mut self, result: Result<T, MachineError>) -> T {
        match result {
            Ok(value) => value,
            Err(error) => {
                self.error.get_or_insert(error);
                T::default()
            }
        }
    }
}
impl MachineWorld for Stage<'_> {
    fn sky_open(&mut self, p: [i32; 3]) -> bool {
        let pos = BlockPos {
            space: self.current_space(),
            x: p[0],
            y: p[1],
            z: p[2],
        };
        let result = self.cell(pos).map(|_| {
            self.world
                .page(pos.page())
                .is_some_and(|page| page.light(pos.local_index()).sky == 15)
        });
        self.callback(result)
    }
    fn lava_neighbors(&mut self, p: [i32; 3]) -> u8 {
        let origin = BlockPos {
            space: self.current_space(),
            x: p[0],
            y: p[1],
            z: p[2],
        };
        let result = (|| {
            let mut count = 0;
            for face in Face::ALL {
                if self.cell(offset(origin, face)?)?.id() == LAVA {
                    count += 1;
                }
            }
            Ok(count)
        })();
        self.callback(result)
    }
    fn source(&mut self, p: [i32; 3]) -> Option<u8> {
        let pos = BlockPos {
            space: self.current_space(),
            x: p[0],
            y: p[1],
            z: p[2],
        };
        let result = self.cell(pos).map(|cell| match cell.id() {
            WATER if cell.state() & 15 == 0 => Some(recipes::SUB_WATER),
            LAVA if cell.state() & 15 == 0 => Some(recipes::SUB_LAVA),
            _ => None,
        });
        self.callback(result)
    }
    fn drain_lava(&mut self, p: [i32; 3]) -> bool {
        let pos = BlockPos {
            space: self.current_space(),
            x: p[0],
            y: p[1],
            z: p[2],
        };
        let result = self.cell(pos).map(|cell| {
            if cell.id() != LAVA || cell.state() & 15 != 0 {
                false
            } else {
                self.local.cells.insert(pos, (cell, Cell::AIR));
                true
            }
        });
        self.callback(result)
    }
    fn push_energy(&mut self, p: [i32; 3], face: Face, amount: u64) -> u64 {
        let p = BlockPos {
            space: self.current_space(),
            x: p[0],
            y: p[1],
            z: p[2],
        };
        let result = self.push_energy_result(p, face, amount);
        self.callback(result)
    }
    fn push_items(&mut self, p: [i32; 3], face: Face, stack: ItemStack) -> u16 {
        let p = BlockPos {
            space: self.current_space(),
            x: p[0],
            y: p[1],
            z: p[2],
        };
        let result = self.push_items_result(p, face, stack);
        self.callback(result)
    }
    fn push_substance(&mut self, p: [i32; 3], face: Face, substance: u8, amount: u32) -> u32 {
        let p = BlockPos {
            space: self.current_space(),
            x: p[0],
            y: p[1],
            z: p[2],
        };
        let result = self.push_substance_result(p, face, substance, amount);
        self.callback(result)
    }
}

impl machines::grid::GridWorld for Stage<'_> {
    fn active(&self, endpoint: machines::grid::Endpoint) -> bool {
        self.owner
            .machines
            .get(&ComponentId(endpoint.owner))
            .is_some_and(|m| {
                m.active
                    && self
                        .world
                        .component(m.id)
                        .is_some_and(|r| r.data.kind == MACHINE_COMPONENT)
            })
    }
    fn room(
        &mut self,
        endpoint: machines::grid::Endpoint,
        resource: Resource,
        substance: u8,
    ) -> u64 {
        let result = (|| {
            let machine = self.machine(ComponentId(endpoint.owner))?;
            Ok(match resource {
                Resource::Energy => machine.capacity().saturating_sub(machine.energy),
                Resource::Fluid | Resource::Gas => {
                    let mut candidate = machine;
                    u64::from(candidate.insert_substance(substance, u32::MAX, false))
                }
            })
        })();
        self.callback(result)
    }
    fn give(
        &mut self,
        endpoint: machines::grid::Endpoint,
        resource: Resource,
        substance: u8,
        amount: u64,
    ) -> u64 {
        let result = (|| {
            let id = ComponentId(endpoint.owner);
            let machine = self.machine_mut(id)?;
            let accepted = match resource {
                Resource::Energy => machine.insert_energy(amount),
                Resource::Fluid | Resource::Gas => u64::from(machine.insert_substance(
                    substance,
                    amount.min(u64::from(u32::MAX)) as u32,
                    false,
                )),
            };
            if accepted > 0 {
                self.wake_machines.insert(id);
            }
            Ok(accepted)
        })();
        self.callback(result)
    }
    fn extract(
        &mut self,
        endpoint: machines::grid::Endpoint,
        resource: Resource,
        wanted: u8,
        amount: u64,
    ) -> (u8, u64) {
        let result = (|| {
            let id = ComponentId(endpoint.owner);
            let cell = self.cell(BlockPos {
                space: self.space,
                x: endpoint.position[0],
                y: endpoint.position[1],
                z: endpoint.position[2],
            })?;
            let machine = self.machine_mut(id)?;
            let tank = match resource {
                Resource::Energy => return Ok((0, 0)),
                Resource::Fluid => &mut machine.tank,
                Resource::Gas => {
                    let relative = sides::relative(
                        Face::ALL[blocks::facing_of(cell.raw()) as usize],
                        endpoint.face,
                    );
                    let index = machine.gas_sides[relative as usize];
                    if !(1..=2).contains(&index) {
                        return Ok((0, 0));
                    }
                    &mut machine.gas[(index - 1) as usize]
                }
            };
            if tank.amount == 0 || wanted != 0 && wanted != tank.substance {
                return Ok((0, 0));
            }
            let sub = tank.substance;
            let taken = tank.take(amount.min(u64::from(u32::MAX)) as u32);
            if taken > 0 {
                self.wake_machines.insert(id);
            }
            Ok((sub, u64::from(taken)))
        })();
        self.callback(result)
    }
}

impl Overlay {
    pub(super) fn merge(&mut self, local: Overlay) {
        for (id, change) in local.machines {
            if let Some(old) = self.machines.get_mut(&id) {
                old.after = change.after;
            } else {
                self.machines.insert(id, change);
            }
        }
        for (id, change) in local.chests {
            if let Some(old) = self.chests.get_mut(&id) {
                old.after = change.after;
            } else {
                self.chests.insert(id, change);
            }
        }
        for (position, (before, after)) in local.cells {
            self.cells
                .entry(position)
                .and_modify(|value| value.1 = after)
                .or_insert((before, after));
        }
        for (key, (before, after)) in local.reservoirs {
            self.reservoirs
                .entry(key)
                .and_modify(|value| value.1 = after)
                .or_insert((before, after));
        }
        self.inputs.extend(local.inputs);
    }
}
