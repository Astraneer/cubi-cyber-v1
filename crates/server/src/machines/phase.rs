use super::{view::Stage, *};

impl MachineRuntime {
    pub fn plan_tick(
        &mut self,
        world: &World,
        now: Tick,
        budget: Budget,
    ) -> Result<PhasePlan, MachineError> {
        self.plan_tick_inner(
            world,
            now,
            budget,
            PhaseContext {
                payloads: None,
                active: None,
                activity: &[],
            },
            &[],
        )
    }
    pub fn plan_tick_with_payloads(
        &mut self,
        world: &World,
        payloads: &dyn rules::item_payload::Resolver,
        now: Tick,
        budget: Budget,
    ) -> Result<PhasePlan, MachineError> {
        self.plan_tick_inner(
            world,
            now,
            budget,
            PhaseContext {
                payloads: Some(payloads),
                active: None,
                activity: &[],
            },
            &[],
        )
    }
    pub fn plan_tick_with_context(
        &mut self,
        world: &World,
        context: PhaseContext<'_>,
        now: Tick,
        budget: Budget,
    ) -> Result<PhasePlan, MachineError> {
        self.plan_tick_inner(world, now, budget, context, &[])
    }
    pub fn plan_tick_with_topology(
        &mut self,
        world: &World,
        context: PhaseContext<'_>,
        positions: &[BlockPos],
        now: Tick,
        budget: Budget,
    ) -> Result<PhasePlan, MachineError> {
        self.plan_tick_inner(world, now, budget, context, positions)
    }
    fn plan_tick_inner(
        &mut self,
        world: &World,
        now: Tick,
        budget: Budget,
        context: PhaseContext<'_>,
        topology: &[BlockPos],
    ) -> Result<PhasePlan, MachineError> {
        if self.owner_tick().is_some_and(|tick| tick != now) {
            return Err(MachineError::Stale);
        }
        if self.owner_pending() && (!context.activity.is_empty() || !topology.is_empty()) {
            return Err(MachineError::Stale);
        }
        let topology_queue = self.topology_registration(topology, now)?;
        let new_dirty = self.new_dirty_networks(topology);
        let initial =
            self.extend_machine_page_activity(self.noop_delta()?, context.activity, now)?;
        let activity_meta: BTreeMap<_, _> = initial
            .machines
            .iter()
            .map(|p| {
                (
                    p.after.or(p.before).expect("validated metadata").id,
                    p.after,
                )
            })
            .collect();
        let mut queue: BTreeMap<_, _> = initial
            .queue
            .iter()
            .map(|p| {
                (
                    p.after.or(p.before).expect("validated event").event,
                    p.after,
                )
            })
            .collect();
        let mut cohort = self.owner.project();
        let mut work = Work::default();
        let mut overlay = Overlay {
            clocks: self.transport.activity_patches(context.activity, now)?,
            ..Overlay::default()
        };
        let transit_due = if overlay.clocks.is_empty() {
            self.transport_due(now)
        } else {
            !self.transport.agenda(&overlay, now, 1)?.is_empty()
        };
        let due = self
            .events
            .range(..=(now, u8::MAX, u64::MAX))
            .any(|(_, p)| !queue.contains_key(&p.event))
            || queue.values().filter_map(|p| *p).any(|p| p.due <= now);
        if !due
            && !transit_due
            && !self.owner_pending()
            && overlay.clocks.is_empty()
            && topology_queue.is_none()
            && initial.machines.is_empty()
            && initial.queue.is_empty()
        {
            return Ok(PhasePlan {
                transactions: Vec::new(),
                delta: self.delta(Vec::new(), Vec::new(), Vec::new(), self.next_order)?,
                inputs: Vec::new(),
                fluid_wakes: Vec::new(),
                payloads: Vec::new(),
                item_spawns: Vec::new(),
                work,
            });
        }
        if cohort.state.is_none() && (due || transit_due) {
            cohort.state = Some(OwnerState {
                tick: now,
                stage: OwnerStage::Prepare,
                prepared_after: None,
            });
        }
        // Activity changes join the same detached cohort. They are admitted
        // before the finite due-index capture, never by a dense idle scan.
        for (&event, &after) in &queue {
            if let Some(before) = self.pending(event) {
                cohort.remove(before);
            }
            if let Some(p) = after
                && p.due <= now
                && cohort.state.is_some()
            {
                cohort.put(p);
            }
        }
        if let Some(state) = cohort.state
            && state.stage == OwnerStage::Prepare
            && budget.events > 0
        {
            let lower = state
                .prepared_after
                .map_or(std::ops::Bound::Unbounded, std::ops::Bound::Excluded);
            let mut source = self
                .events
                .range((lower, std::ops::Bound::Included((now, u8::MAX, u64::MAX))));
            let mut cursor = state.prepared_after;
            for (&key, &p) in source.by_ref().take(budget.events) {
                work.prepared += 1;
                cursor = Some(key);
                let effective = queue.get(&p.event).copied().unwrap_or(Some(p));
                if let Some(p) = effective.filter(|p| p.due <= now) {
                    cohort.put(p);
                }
            }
            cohort.state = Some(if source.next().is_none() {
                OwnerState {
                    stage: OwnerStage::Machines,
                    prepared_after: None,
                    ..state
                }
            } else {
                OwnerState {
                    prepared_after: cursor,
                    ..state
                }
            });
        }
        let machine_limit = budget.events;
        let machine_budget = budget;
        let frozen = self
            .owner
            .active_pages
            .as_deref()
            .filter(|_| self.owner.context_tick == Some(now));
        let frozen_active =
            |p: BlockPos| frozen.is_some_and(|pages| pages.binary_search(&p.page()).is_ok());
        let owner = ReadOwner {
            dirty_networks: &self.dirty_networks,
            new_dirty: &new_dirty,
            active: if frozen.is_some() {
                Some(&frozen_active)
            } else {
                context.active
            },
            now,
            packet_limit: self.config.packets,
            transport: &self.transport,
            configs: &self.configs,
            payloads: context.payloads,
            machines: &self.machines,
            activity_meta: &activity_meta,
            positions: &self.positions,
            grids: &self.grids,
        };
        let mut metadata = activity_meta.clone();
        let mut retired = Vec::new();
        let mut processed = BTreeSet::new();
        loop {
            let Some(state) = cohort.state else {
                break;
            };
            let current_phase = match state.stage {
                OwnerStage::Prepare | OwnerStage::Transit => break,
                OwnerStage::Machines => 0,
                OwnerStage::Resources => 1,
            };
            let Some(event) = cohort.next(current_phase) else {
                cohort.state = Some(OwnerState {
                    stage: if current_phase == 0 {
                        OwnerStage::Resources
                    } else {
                        OwnerStage::Transit
                    },
                    ..state
                });
                continue;
            };
            let current_order = event.order;
            if work.events >= machine_limit as u64 {
                work.deferred += 1;
                break;
            }
            cohort.remove(event);
            work.events += 1;
            work.oldest_age = work.oldest_age.max(now.0.saturating_sub(event.first_due.0));
            queue.insert(event.event, None);
            processed.insert(event.event);
            if let Event::Machine(id) = event.event {
                let meta = owner.machine_meta(id).ok_or(MachineError::Missing)?;
                if !owner.machine_active(id) {
                    metadata.insert(
                        id,
                        Some(MachineMeta {
                            active: false,
                            ..meta
                        }),
                    );
                    continue;
                }
                if meta.last_tick >= now {
                    schedule(
                        self,
                        &mut queue,
                        &mut cohort,
                        &processed,
                        Pending {
                            due: Tick(now.0.saturating_add(1)),
                            ..event
                        },
                        now,
                        current_phase,
                    )?;
                    continue;
                }
            }
            if let Event::Machine(id) = event.event
                && world
                    .component(id)
                    .is_none_or(|r| r.data.kind != MACHINE_COMPONENT)
            {
                metadata.insert(id, None);
                retired.push(id);
                continue;
            }
            if let Event::Network(key) = event.event
                && owner.network_dirty(key)
            {
                work.deferred += 1;
                schedule(
                    self,
                    &mut queue,
                    &mut cohort,
                    &processed,
                    Pending {
                        due: Tick(now.0.saturating_add(1)),
                        ..event
                    },
                    now,
                    current_phase,
                )?;
                continue;
            }
            let space = match event.event {
                Event::Machine(id) => {
                    owner
                        .machine_meta(id)
                        .ok_or(MachineError::Missing)?
                        .anchor
                        .space
                }
                Event::Network(key) => key.space,
            };
            let mut stage = Stage::new(owner, world, &overlay, &mut work, machine_budget, space);
            let mut next_machine = None;
            let result = (|| match event.event {
                Event::Machine(id) => {
                    let meta = owner.machine_meta(id).ok_or(MachineError::Missing)?;
                    let mut machine = stage.machine(id)?;
                    let before = machine.clone();
                    advance_idle_clocks(
                        &mut machine,
                        now.0.saturating_sub(meta.last_tick.0).saturating_sub(1),
                    );
                    let cell = stage.cell(meta.anchor)?;
                    let facing = Face::ALL[blocks::facing_of(cell.raw()) as usize];
                    machine.tick(
                        [meta.anchor.x, meta.anchor.y, meta.anchor.z],
                        facing,
                        &mut stage,
                    );
                    if let Some(error) = stage.error {
                        return Err(error);
                    }
                    let changed = before != machine;
                    let again = should_continue(&before, &machine);
                    let shown = if machine.active.shown {
                        cell.state() | blocks::ST_LIT
                    } else {
                        cell.state() & !blocks::ST_LIT
                    };
                    if shown != cell.state() {
                        stage
                            .local
                            .cells
                            .insert(meta.anchor, (cell, Cell::from_parts(cell.id(), shown)));
                    }
                    stage
                        .local
                        .machines
                        .get_mut(&id)
                        .expect("loaded machine")
                        .after = machine;
                    next_machine = Some((
                        MachineMeta {
                            last_tick: now,
                            ..meta
                        },
                        again,
                    ));
                    if changed {
                        for face in Face::ALL {
                            if let Ok(q) = offset(meta.anchor, face)
                                && let Some(&neighbor) = owner.positions.get(&q)
                            {
                                stage.wake_machines.insert(neighbor);
                            }
                        }
                    }
                    Ok(())
                }
                Event::Network(key) => {
                    let grid = owner.grids.get(&key.space).ok_or(MachineError::Missing)?;
                    let network = grid.network(key.network)?;
                    if network.endpoints().len()
                        > budget
                            .endpoints
                            .saturating_sub(stage.work.endpoint_queries as usize)
                    {
                        return Err(MachineError::Bounds);
                    }
                    let before = stage.reservoir(key)?;
                    let mut grid_work = GridWork::default();
                    let (after, again) = grid.project_phase(
                        key.network,
                        before,
                        &mut stage,
                        &mut grid_work,
                        &mut self.phase_scratch,
                    )?;
                    if let Some(error) = stage.error {
                        return Err(error);
                    }
                    stage.work.endpoint_queries += grid_work.endpoint_queries;
                    stage.work.transfers += grid_work.transfers;
                    stage.local.reservoirs.insert(
                        key,
                        (
                            overlay
                                .reservoirs
                                .get(&key)
                                .map_or(network.reservoir(), |v| v.0),
                            after,
                        ),
                    );
                    if again {
                        stage.wake_networks.insert(key);
                    }
                    if after.flow > 0
                        && let Some(ids) = self.producers.get(&key)
                    {
                        stage.wake_machines.extend(ids.iter().copied());
                    }
                    Ok(())
                }
            })();
            if let Err(error) = result {
                drop(stage);
                match error {
                    MachineError::Unknown => {
                        work.unknown += 1;
                        schedule(
                            self,
                            &mut queue,
                            &mut cohort,
                            &processed,
                            Pending {
                                due: Tick(now.0.saturating_add(1)),
                                ..event
                            },
                            now,
                            current_phase,
                        )?;
                        continue;
                    }
                    MachineError::Bounds => {
                        work.deferred += 1;
                        schedule(
                            self,
                            &mut queue,
                            &mut cohort,
                            &processed,
                            event,
                            now,
                            current_phase,
                        )?;
                        cohort.put(event);
                        break;
                    }
                    _ => return Err(error),
                }
            }
            let Stage {
                local,
                wake_machines,
                wake_networks,
                ..
            } = stage;
            overlay.merge(local);
            if let Some((meta, again)) = next_machine {
                metadata.insert(meta.id, Some(meta));
                work.machines += 1;
                work.recipe_ticks += 1;
                if again {
                    schedule(
                        self,
                        &mut queue,
                        &mut cohort,
                        &processed,
                        Pending {
                            event: Event::Machine(meta.id),
                            due: Tick(now.0.saturating_add(1)),
                            first_due: Tick(now.0.saturating_add(1)),
                            order: meta.order,
                        },
                        now,
                        current_phase,
                    )?;
                }
            } else {
                work.networks += 1;
            }
            for id in wake_machines {
                let meta = owner.machine_meta(id).ok_or(MachineError::Missing)?;
                if !owner.machine_active(id)
                    || world
                        .component(id)
                        .is_none_or(|r| r.data.kind != MACHINE_COMPONENT)
                {
                    continue;
                }
                let due = if current_phase == 0
                    && meta.order > current_order
                    && !processed.contains(&Event::Machine(id))
                {
                    now
                } else {
                    Tick(now.0.saturating_add(1))
                };
                schedule(
                    self,
                    &mut queue,
                    &mut cohort,
                    &processed,
                    Pending {
                        event: Event::Machine(id),
                        due,
                        first_due: due,
                        order: meta.order,
                    },
                    now,
                    current_phase,
                )?;
            }
            for key in wake_networks {
                let order = *self.network_orders.get(&key).ok_or(MachineError::Missing)?;
                let due = if current_phase == 0 {
                    now
                } else {
                    Tick(now.0.saturating_add(1))
                };
                schedule(
                    self,
                    &mut queue,
                    &mut cohort,
                    &processed,
                    Pending {
                        event: Event::Network(key),
                        due,
                        first_due: due,
                        order,
                    },
                    now,
                    current_phase,
                )?;
            }
        }
        let transport_wakes = if cohort.state.is_some_and(|s| s.stage == OwnerStage::Transit) {
            transport::advance(owner, world, &mut overlay, &mut work, budget, now)?
        } else {
            BTreeSet::new()
        };
        for id in transport_wakes {
            if let Some(meta) = owner.machine_meta(id).filter(|_| owner.machine_active(id)) {
                let due = Tick(now.0.saturating_add(1));
                schedule(
                    self,
                    &mut queue,
                    &mut cohort,
                    &processed,
                    Pending {
                        event: Event::Machine(id),
                        due,
                        first_due: due,
                        order: meta.order,
                    },
                    now,
                    3,
                )?;
            }
        }
        if cohort.state.is_some_and(|s| s.stage == OwnerStage::Transit)
            && !self.transport.has_due_projected(&overlay, now)?
        {
            cohort.state = None;
        }
        let transport_delta = self.transport.projected_delta(&overlay)?;
        let mut transactions = BTreeMap::<SpaceId, Transaction>::new();
        for (&id, change) in &overlay.machines {
            if change.before == change.after {
                continue;
            }
            let meta = owner.machine_meta(id).ok_or(MachineError::Missing)?;
            if world
                .component(id)
                .is_none_or(|record| record.version != change.version)
            {
                return Err(MachineError::Stale);
            }
            transactions
                .entry(meta.anchor.space)
                .or_insert_with(|| Transaction::new(now, meta.anchor.space))
                .expect_page(meta.anchor.page(), world.page_version(meta.anchor.page()))
                .set_component(
                    id,
                    machine_component(meta.anchor, &change.after),
                    ChannelMask::CONTENTS,
                );
        }
        for (&id, change) in &overlay.chests {
            if change.before == change.after {
                continue;
            }
            if world
                .component(id)
                .is_none_or(|record| record.version != change.version)
            {
                return Err(MachineError::Stale);
            }
            transactions
                .entry(change.anchor.space)
                .or_insert_with(|| Transaction::new(now, change.anchor.space))
                .expect_page(
                    change.anchor.page(),
                    world.page_version(change.anchor.page()),
                )
                .set_component(
                    id,
                    ComponentData {
                        anchor: change.anchor,
                        kind: crate::survival::CHEST_COMPONENT,
                        payload: change.after.encode().to_vec(),
                    },
                    ChannelMask::CONTENTS,
                );
        }
        let mut fluid_wakes = Vec::new();
        for (&id, change) in &overlay.furnaces {
            if change.before == change.after {
                continue;
            }
            if world
                .component(id)
                .is_none_or(|r| r.version != change.version)
            {
                return Err(MachineError::Stale);
            }
            transactions
                .entry(change.anchor.space)
                .or_insert_with(|| Transaction::new(now, change.anchor.space))
                .expect_page(
                    change.anchor.page(),
                    world.page_version(change.anchor.page()),
                )
                .set_component(
                    id,
                    crate::survival::furnace_component(change.anchor, &change.after),
                    ChannelMask::CONTENTS,
                );
        }
        for (&position, &(before, after)) in &overlay.cells {
            if before == after {
                continue;
            }
            transactions
                .entry(position.space)
                .or_insert_with(|| Transaction::new(now, position.space))
                .expect_page(position.page(), world.page_version(position.page()))
                .set_cell(position, after);
            if before.id() == LAVA && after.id() == AIR {
                fluid_wakes.push(position);
            }
        }
        let machines = metadata
            .into_iter()
            .filter_map(|(id, after)| {
                let before = owner.machines.get(&id).copied();
                (before != after).then_some(MetaPatch { before, after })
            })
            .collect();
        let queue = queue
            .into_iter()
            .filter_map(|(event, after)| {
                let before = self.pending(event);
                (before != after).then_some(QueuePatch { before, after })
            })
            .collect();
        let reservoirs = overlay
            .reservoirs
            .into_iter()
            .filter_map(|(key, (before, after))| {
                (before != after).then_some(ReservoirPatch { key, before, after })
            })
            .collect();
        let inputs = overlay
            .inputs
            .into_iter()
            .map(|(page, version)| jobs::InputVersion { page, version })
            .collect();
        let mut delta = self.delta(machines, queue, reservoirs, self.next_order)?;
        delta.owner = cohort.delta();
        delta.topology_queue = topology_queue;
        if !transport_delta.is_empty() {
            delta.transport = Some(transport_delta);
        }
        let delta = self.extend_registration(delta, world, retired, now)?;
        self.validate_delta(&delta)?;
        Ok(PhasePlan {
            transactions: transactions.into_values().collect(),
            delta,
            inputs,
            fluid_wakes,
            payloads: overlay.payloads.into_values().collect(),
            item_spawns: overlay.item_spawns,
            work,
        })
    }
}
fn schedule(
    runtime: &MachineRuntime,
    queue: &mut BTreeMap<Event, Option<Pending>>,
    cohort: &mut barrier::Cohort<'_>,
    processed: &BTreeSet<Event>,
    mut next: Pending,
    now: Tick,
    current_phase: u8,
) -> Result<(), MachineError> {
    let old = queue
        .get(&next.event)
        .copied()
        .unwrap_or_else(|| runtime.pending(next.event));
    if let Some(old) = old {
        next.due = next.due.min(old.due);
        next.first_due = next.first_due.min(old.first_due);
    }
    queue.insert(next.event, Some(next));
    if next.due <= now && phase(next.event) >= current_phase && !processed.contains(&next.event) {
        cohort.put(next);
    }
    if queue.len() > runtime.config.events {
        return Err(MachineError::Bounds);
    }
    Ok(())
}
fn advance_idle_clocks(machine: &mut Machine, elapsed: u64) {
    if elapsed == 0 {
        return;
    }
    if machine.eject && machines::definition(machine.kind).output.is_some() {
        machine.eject_delay = ((u64::from(machine.eject_delay) + 11 - elapsed % 11) % 11) as u8;
    }
    machine.active.delay = machine.active.delay.saturating_sub(elapsed.min(255) as u8);
    if machine.active.delay == 0 {
        machine.active.shown = machine.active.logic;
    }
    if machine.kind == MekKind::Wind {
        machine.ticker = machine.ticker.saturating_add(elapsed);
    }
}
fn should_continue(before: &Machine, after: &Machine) -> bool {
    if after.last.working() || after.active.delay > 0 {
        return true;
    }
    let d = machines::definition(after.kind);
    if d.output
        .is_some_and(|i| after.eject && !after.slots[i].stack.is_empty())
        || d.upgrade_slot
            .is_some_and(|i| !after.slots[i].stack.is_empty())
        || d.energy_slot
            .is_some_and(|i| !after.slots[i].stack.is_empty())
    {
        return true;
    }
    match after.kind {
        MekKind::Heat => {
            after.heat > 3000.0 || after.tank.amount > 0 || after.energy != before.energy
        }
        MekKind::Wind => {
            after.produced > 0 || after.energy != before.energy || !after.ticker.is_multiple_of(20)
        }
        MekKind::Cube => {
            after.energy != before.energy
                || after.slots[..2].iter().any(|slot| !slot.stack.is_empty())
        }
        _ => false,
    }
}
