use super::*;

impl MachineRuntime {
    pub fn new(config: Config) -> Result<Self, MachineError> {
        if config.machines > 1_000_000
            || config.transmitters > 1_000_000
            || config.endpoints > 65_536
            || config.events > 2_000_000
            || config.packets > 65_536
        {
            return Err(MachineError::Bounds);
        }
        Ok(Self {
            config,
            revision: 0,
            next_order: 0,
            owner: barrier::OwnerBarrier::default(),
            machines: BTreeMap::new(),
            positions: BTreeMap::new(),
            machine_pages: BTreeMap::new(),
            events: BTreeMap::new(),
            event_ages: BTreeMap::new(),
            event_bytes: 0,
            keys: BTreeMap::new(),
            grids: BTreeMap::new(),
            network_orders: BTreeMap::new(),
            producers: BTreeMap::new(),
            configs: BTreeMap::new(),
            config_pages: BTreeMap::new(),
            phase_scratch: PhaseScratch::new(config.endpoints),
            transport: transport::TransportOwner::new(),
            topology_queue: topology_pending::new_queue()?,
            dirty_networks: BTreeMap::new(),
            dirty_positions: BTreeMap::new(),
        })
    }
    pub fn pending(&self, event: Event) -> Option<Pending> {
        self.keys
            .get(&event)
            .and_then(|key| self.events.get(key))
            .copied()
    }
    pub fn machine_meta(&self, id: ComponentId) -> Option<MachineMeta> {
        self.machines.get(&id).copied()
    }
    pub fn event_count(&self) -> usize {
        self.events.len()
    }
    pub fn noop_delta(&self) -> Result<RuntimeDelta, MachineError> {
        self.delta(Vec::new(), Vec::new(), Vec::new(), self.next_order)
    }
    pub fn snapshot(&self) -> RuntimeSnapshot {
        RuntimeSnapshot {
            revision: self.revision,
            next_order: self.next_order,
            owner: self.owner.snapshot(),
            transport: self.transport.snapshot(),
            topology_queue: self.topology_queue.snapshot(),
            machines: self.machines.values().copied().collect(),
            events: self.events.values().copied().collect(),
            network_orders: self
                .network_orders
                .iter()
                .map(|(&key, &order)| (key, order))
                .collect(),
            configs: self.configs.values().copied().collect(),
            producers: self
                .producers
                .iter()
                .map(|(&key, ids)| (key, ids.clone()))
                .collect(),
            grids: self
                .grids
                .iter()
                .map(|(&space, grid)| (space, grid.snapshot()))
                .collect(),
        }
    }
    pub fn restore(config: Config, snapshot: &RuntimeSnapshot) -> Result<Self, MachineError> {
        if snapshot.machines.len() > config.machines
            || snapshot.events.len() > config.events
            || snapshot.grids.len() > 64
        {
            return Err(MachineError::Bounds);
        }
        let mut runtime = Self::new(config)?;
        runtime.revision = snapshot.revision;
        runtime.next_order = snapshot.next_order;
        runtime.transport = transport::TransportOwner::restore(config, &snapshot.transport)?;
        runtime.topology_queue = topology_pending::restore_queue(&snapshot.topology_queue)?;
        let mut orders = BTreeSet::new();
        for &m in &snapshot.machines {
            if m.order >= snapshot.next_order
                || !orders.insert(m.order)
                || runtime.machines.insert(m.id, m).is_some()
                || runtime.positions.insert(m.anchor, m.id).is_some()
            {
                return Err(MachineError::Encoding);
            }
            runtime
                .machine_pages
                .entry(m.anchor.page())
                .or_default()
                .insert(m.id);
        }
        for &p in &snapshot.events {
            if runtime.keys.contains_key(&p.event) || runtime.events.contains_key(&queue_key(p)) {
                return Err(MachineError::Encoding);
            }
            runtime.insert_pending_indexed(p);
        }
        for (space, grid) in &snapshot.grids {
            if runtime
                .grids
                .insert(
                    *space,
                    Grid::restore(config.transmitters, config.endpoints, grid)?,
                )
                .is_some()
            {
                return Err(MachineError::Encoding);
            }
        }
        for &(key, order) in &snapshot.network_orders {
            if order >= snapshot.next_order
                || !orders.insert(order)
                || runtime.network_orders.insert(key, order).is_some()
                || runtime
                    .grids
                    .get(&key.space)
                    .ok_or(MachineError::Encoding)?
                    .network(key.network)
                    .is_err()
            {
                return Err(MachineError::Encoding);
            }
        }
        for (key, ids) in &snapshot.producers {
            if !runtime.network_orders.contains_key(key)
                || ids.windows(2).any(|p| p[0] >= p[1])
                || ids.iter().any(|id| !runtime.machines.contains_key(id))
                || runtime.producers.insert(*key, ids.clone()).is_some()
            {
                return Err(MachineError::Encoding);
            }
        }
        if snapshot.machines.windows(2).any(|p| p[0].id >= p[1].id)
            || snapshot
                .events
                .windows(2)
                .any(|p| queue_key(p[0]) >= queue_key(p[1]))
            || snapshot.grids.windows(2).any(|p| p[0].0 >= p[1].0)
            || snapshot.network_orders.windows(2).any(|p| p[0].0 >= p[1].0)
            || snapshot.producers.windows(2).any(|p| p[0].0 >= p[1].0)
            || snapshot
                .grids
                .iter()
                .map(|(_, g)| g.nodes.len())
                .sum::<usize>()
                > config.transmitters
        {
            return Err(MachineError::Encoding);
        }
        for &event in &snapshot.events {
            let order = match event.event {
                Event::Machine(id) => runtime
                    .machine_meta(id)
                    .filter(|m| m.active)
                    .map(|m| m.order),
                Event::Network(key) => runtime.network_orders.get(&key).copied(),
            };
            if event.first_due > event.due || order != Some(event.order) {
                return Err(MachineError::Encoding);
            }
        }
        if snapshot.configs.len() > config.transmitters
            || snapshot
                .configs
                .windows(2)
                .any(|p| p[0].position >= p[1].position)
        {
            return Err(MachineError::Encoding);
        }
        for &config in &snapshot.configs {
            validate_config(config)?;
            runtime.configs.insert(config.position, config);
            runtime
                .config_pages
                .entry(config.position.page())
                .or_default()
                .insert(config.position);
        }
        runtime.owner = barrier::OwnerBarrier::restore(&runtime, &snapshot.owner)?;
        runtime.restore_dirty();
        Ok(runtime)
    }
    pub(super) fn delta(
        &self,
        machines: Vec<MetaPatch>,
        queue: Vec<QueuePatch>,
        reservoirs: Vec<ReservoirPatch>,
        next_order: u64,
    ) -> Result<RuntimeDelta, MachineError> {
        Ok(RuntimeDelta {
            revision: self.revision,
            next_revision: self.revision.checked_add(1).ok_or(MachineError::Overflow)?,
            next_order_before: self.next_order,
            next_order_after: next_order,
            machines,
            queue,
            reservoirs,
            topology: Vec::new(),
            networks: Vec::new(),
            endpoints: Vec::new(),
            configs: Vec::new(),
            transport: None,
            topology_queue: None,
            owner: self.owner.project().delta(),
        })
    }
    pub fn plan_register(
        &self,
        world: &World,
        id: ComponentId,
        first: Tick,
    ) -> Result<RuntimeDelta, MachineError> {
        if self.machines.len() >= self.config.machines
            || self.events.len() >= self.config.events
            || self.machines.contains_key(&id)
        {
            return Err(MachineError::Bounds);
        }
        let record = world.component(id).ok_or(MachineError::Missing)?;
        let machine = read_machine(world, id)?;
        let CellRead::Known(cell) = world.read(record.data.anchor) else {
            return Err(MachineError::Unknown);
        };
        if !matches!(blocks::bdef(cell.id()).be,Some(BeKind::Mek(kind)) if kind==machine.kind)
            || self.positions.contains_key(&record.data.anchor)
        {
            return Err(MachineError::Component);
        }
        let meta = MachineMeta {
            id,
            anchor: record.data.anchor,
            order: self.next_order,
            last_tick: Tick(first.0.saturating_sub(1)),
            active: true,
        };
        let event = Pending {
            event: Event::Machine(id),
            due: first,
            first_due: first,
            order: meta.order,
        };
        self.delta(
            vec![MetaPatch {
                before: None,
                after: Some(meta),
            }],
            vec![QueuePatch {
                before: None,
                after: Some(event),
            }],
            Vec::new(),
            self.next_order
                .checked_add(1)
                .ok_or(MachineError::Overflow)?,
        )
    }
    pub fn plan_wake(&self, id: ComponentId, due: Tick) -> Result<RuntimeDelta, MachineError> {
        let meta = self.machine_meta(id).ok_or(MachineError::Missing)?;
        let event = Event::Machine(id);
        let before = self.pending(event);
        let after = before.map_or(
            Pending {
                event,
                due,
                first_due: due,
                order: meta.order,
            },
            |old| Pending {
                due: old.due.min(due),
                first_due: old.first_due.min(due),
                ..old
            },
        );
        if before.is_none() && self.events.len() >= self.config.events {
            return Err(MachineError::Bounds);
        }
        self.delta(
            Vec::new(),
            vec![QueuePatch {
                before,
                after: Some(after),
            }],
            Vec::new(),
            self.next_order,
        )
    }
    pub fn plan_activity(
        &self,
        changes: &[(ComponentId, bool)],
        now: Tick,
    ) -> Result<RuntimeDelta, MachineError> {
        if changes.len() > 1024 {
            return Err(MachineError::Bounds);
        }
        let mut ids = BTreeSet::new();
        let mut metadata = Vec::new();
        let mut queue = Vec::new();
        for &(id, active) in changes {
            if !ids.insert(id) {
                return Err(MachineError::Encoding);
            }
            let before = self.machine_meta(id).ok_or(MachineError::Missing)?;
            let after = MachineMeta {
                active,
                last_tick: if active && !before.active {
                    Tick(now.0.saturating_sub(1))
                } else {
                    before.last_tick
                },
                ..before
            };
            metadata.push(MetaPatch {
                before: Some(before),
                after: Some(after),
            });
            let event = Event::Machine(id);
            let old = self.pending(event);
            let next = if active {
                Some(Pending {
                    event,
                    due: now,
                    first_due: now,
                    order: after.order,
                })
            } else {
                None
            };
            if old != next {
                queue.push(QueuePatch {
                    before: old,
                    after: next,
                });
            }
        }
        let delta = self.delta(metadata, queue, Vec::new(), self.next_order)?;
        self.validate_delta(&delta)?;
        Ok(delta)
    }
    pub fn validate_delta(&self, delta: &RuntimeDelta) -> Result<(), MachineError> {
        self.owner.validate(self, delta)?;
        self.validate_topology_queue(delta)?;
        if let Some(transport) = &delta.transport {
            self.transport.validate(self.config, transport)?;
        }
        if delta.revision != self.revision
            || delta.next_revision != self.revision.checked_add(1).ok_or(MachineError::Overflow)?
            || delta.next_order_before != self.next_order
            || delta.next_order_after < self.next_order
        {
            return Err(MachineError::Stale);
        }
        let added = delta
            .machines
            .iter()
            .filter(|p| p.before.is_none() && p.after.is_some())
            .count();
        let removed = delta
            .machines
            .iter()
            .filter(|p| p.before.is_some() && p.after.is_none())
            .count();
        if self
            .machines
            .len()
            .saturating_add(added)
            .saturating_sub(removed)
            > self.config.machines
        {
            return Err(MachineError::Bounds);
        }
        let added_events = delta
            .queue
            .iter()
            .filter(|p| p.before.is_none() && p.after.is_some())
            .count();
        let removed_events = delta
            .queue
            .iter()
            .filter(|p| p.before.is_some() && p.after.is_none())
            .count();
        if self
            .events
            .len()
            .saturating_add(added_events)
            .saturating_sub(removed_events)
            > self.config.events
        {
            return Err(MachineError::Bounds);
        }
        let removed_ids: BTreeSet<_> = delta
            .machines
            .iter()
            .filter(|p| p.after.is_none())
            .filter_map(|p| p.before.map(|m| m.id))
            .collect();
        let mut new_positions = BTreeSet::new();
        let mut new_orders = BTreeSet::new();
        self.validate_extensions(delta)?;
        for patch in &delta.networks {
            if patch.before.is_none()
                && let Some(after) = &patch.after
            {
                new_orders.insert(after.order);
            }
        }
        let after_network_metadata: BTreeMap<_, _> = delta
            .networks
            .iter()
            .filter_map(|p| {
                p.after
                    .as_ref()
                    .or(p.before.as_ref())
                    .map(|m| (m.key, p.after.as_ref()))
            })
            .collect();
        let after_metadata: BTreeMap<_, _> = delta
            .machines
            .iter()
            .filter_map(|p| p.after.or(p.before).map(|m| (m.id, p.after)))
            .collect();
        let mut ids = BTreeSet::new();
        let mut events = BTreeSet::new();
        let mut networks = BTreeSet::new();
        for patch in &delta.machines {
            let id = patch
                .after
                .or(patch.before)
                .ok_or(MachineError::Encoding)?
                .id;
            if let Some(after) = patch.after
                && (after.order >= delta.next_order_after
                    || !new_positions.insert(after.anchor)
                    || self
                        .positions
                        .get(&after.anchor)
                        .is_some_and(|old| *old != id && !removed_ids.contains(old))
                    || patch.before.is_none()
                        && (after.order < self.next_order || !new_orders.insert(after.order))
                    || patch
                        .before
                        .is_some_and(|before| after.last_tick < before.last_tick))
            {
                return Err(MachineError::Encoding);
            }
            if !ids.insert(id)
                || self.machine_meta(id) != patch.before
                || patch.before.zip(patch.after).is_some_and(|(a, b)| {
                    a.id != b.id || a.anchor != b.anchor || a.order != b.order
                })
            {
                return Err(MachineError::Stale);
            }
        }
        for patch in &delta.queue {
            let event = patch
                .after
                .or(patch.before)
                .ok_or(MachineError::Encoding)?
                .event;
            if let Some(after) = patch.after {
                let expected_order = match after.event {
                    Event::Machine(id) => after_metadata
                        .get(&id)
                        .map(|m| m.map(|m| m.order))
                        .unwrap_or_else(|| self.machine_meta(id).map(|m| m.order)),
                    Event::Network(key) => after_network_metadata
                        .get(&key)
                        .map(|meta| meta.map(|m| m.order))
                        .unwrap_or_else(|| self.network_orders.get(&key).copied()),
                };
                if after.first_due > after.due || expected_order != Some(after.order) {
                    return Err(MachineError::Encoding);
                }
            }
            if !events.insert(event)
                || self.pending(event) != patch.before
                || patch
                    .before
                    .zip(patch.after)
                    .is_some_and(|(a, b)| a.event != b.event)
            {
                return Err(MachineError::Stale);
            }
        }
        for patch in &delta.reservoirs {
            self.grids
                .get(&patch.key.space)
                .ok_or(MachineError::Missing)?
                .validate_reservoir(patch.key.network, patch.after)?;
            if !networks.insert(patch.key)
                || self
                    .grids
                    .get(&patch.key.space)
                    .ok_or(MachineError::Missing)?
                    .network(patch.key.network)?
                    .reservoir()
                    != patch.before
            {
                return Err(MachineError::Stale);
            }
        }
        Ok(())
    }
    pub fn acknowledge(&mut self, delta: RuntimeDelta) -> Result<(), MachineError> {
        self.validate_delta(&delta)?;
        let dirty_rechecks = self.dirty_rechecks(&delta);
        let prepared = self.prepare_topology(&delta)?;
        self.apply_extensions(&delta, prepared)?;
        if let Some(transport) = delta.transport {
            self.transport.apply(self.config, transport)?;
        }
        self.acknowledge_topology_queue(delta.topology_queue)?;
        self.owner.apply(delta.owner);
        self.refresh_dirty(dirty_rechecks);
        for patch in &delta.machines {
            if let Some(before) = patch.before
                && patch.after.is_none()
            {
                self.machines.remove(&before.id);
                self.positions.remove(&before.anchor);
                if let Some(ids) = self.machine_pages.get_mut(&before.anchor.page()) {
                    ids.remove(&before.id);
                    if ids.is_empty() {
                        self.machine_pages.remove(&before.anchor.page());
                    }
                }
            }
        }
        for patch in delta.machines {
            if let Some(after) = patch.after {
                self.machines.insert(after.id, after);
                if patch.before.is_none() {
                    self.positions.insert(after.anchor, after.id);
                    self.machine_pages
                        .entry(after.anchor.page())
                        .or_default()
                        .insert(after.id);
                }
            }
        }
        for patch in delta.queue {
            if let Some(before) = patch.before {
                self.remove_pending_indexed(before.event);
            }
            if let Some(after) = patch.after {
                self.insert_pending_indexed(after);
            }
        }
        for patch in delta.reservoirs {
            self.grids
                .get_mut(&patch.key.space)
                .ok_or(MachineError::Missing)?
                .apply_reservoir(patch.key.network, patch.before, patch.after)?;
        }
        self.revision = delta.next_revision;
        self.next_order = delta.next_order_after;
        Ok(())
    }
    pub fn validate(&self, world: &World, plan: &PhasePlan) -> Result<(), MachineError> {
        self.validate_delta(&plan.delta)?;
        if plan.inputs.iter().any(|input| {
            world
                .page(input.page)
                .map(|_| world.input_version(input.page, CHANNELS))
                != input.version
        }) {
            return Err(MachineError::Stale);
        }
        Ok(())
    }
}
