//! Resource phases finish before PULL and one movement per packet/global tick.
use super::*;
use crate::machines::view::Stage;
use rules::entities::{EntityId, item::ItemEntity};
pub(in crate::machines) fn advance(
    owner: ReadOwner<'_>,
    world: &World,
    overlay: &mut Overlay,
    work: &mut Work,
    budget: Budget,
    now: Tick,
) -> Result<BTreeSet<ComponentId>, MachineError> {
    let remaining = budget.events.saturating_sub(work.events as usize);
    let mut agenda = owner.transport.agenda(overlay, now, remaining)?;
    for patch in overlay.packets.values() {
        if let Some(p) = &patch.after
            && let Some(at) = p.at()
        {
            let clock = overlay
                .clocks
                .get(&at.page())
                .map(|p| p.after)
                .unwrap_or_else(|| owner.transport.clocks.get(&at.page()).copied())
                .ok_or(MachineError::Encoding)?;
            if clock.active && p.due <= clock.local_now(now)? {
                agenda.insert((1, p.id), TransitEvent::Packet(p.id));
            }
        }
    }
    let mut wakes = BTreeSet::new();
    let mut processed = BTreeSet::new();
    while let Some((key, event)) = agenda.pop_first() {
        if work.events >= budget.events as u64 {
            work.deferred += 1 + agenda.len() as u64;
            break;
        }
        if !processed.insert(key) {
            continue;
        }
        let position = match event {
            TransitEvent::Pull(p) => p,
            TransitEvent::Packet(id) => overlay
                .packets
                .get(&id)
                .map(|p| p.after.as_ref())
                .unwrap_or_else(|| owner.transport.packets.get(&id))
                .and_then(Packet::at)
                .ok_or(MachineError::Missing)?,
        };
        let clock = overlay
            .clocks
            .get(&position.page())
            .map(|p| p.after)
            .unwrap_or_else(|| owner.transport.clocks.get(&position.page()).copied())
            .ok_or(MachineError::Encoding)?;
        if !clock.active {
            continue;
        }
        work.events += 1;
        let space = match event {
            TransitEvent::Pull(p) => p.space,
            TransitEvent::Packet(id) => {
                overlay
                    .packets
                    .get(&id)
                    .map(|p| p.after.as_ref())
                    .unwrap_or_else(|| owner.transport.packets.get(&id))
                    .and_then(Packet::at)
                    .ok_or(MachineError::Missing)?
                    .space
            }
        };
        let mut stage = Stage::new(owner, world, overlay, work, budget, space);
        let result = match event {
            TransitEvent::Packet(id) => {
                let packet = stage
                    .effective_packet(id)
                    .cloned()
                    .ok_or(MachineError::Missing)?;
                stage.work.oldest_age = stage.work.oldest_age.max(
                    stage
                        .local_now(position.page())?
                        .0
                        .saturating_sub(packet.first_due.0),
                );
                stage.tick_packet(packet)
            }
            TransitEvent::Pull(p) => {
                let pull = stage
                    .parent
                    .pulls
                    .get(&p)
                    .map(|p| p.after)
                    .unwrap_or_else(|| owner.transport.pulls.get(&p).copied())
                    .ok_or(MachineError::Missing)?;
                stage.work.oldest_age = stage.work.oldest_age.max(
                    stage
                        .local_now(position.page())?
                        .0
                        .saturating_sub(pull.first_due.0),
                );
                stage.tick_pull(pull)
            }
        };
        if let Err(error) = result {
            let spawn_exhausted = stage.spawn_exhausted;
            drop(stage);
            match error {
                MachineError::Unknown | MachineError::Bounds
                    if error == MachineError::Unknown || spawn_exhausted =>
                {
                    if error == MachineError::Unknown {
                        work.unknown += 1;
                    } else {
                        work.deferred += 1;
                    }
                    let mut retry = Stage::new(owner, world, overlay, work, budget, space);
                    match event {
                        TransitEvent::Packet(id) => {
                            let mut p = retry
                                .effective_packet(id)
                                .cloned()
                                .ok_or(MachineError::Missing)?;
                            p.due = Tick(retry.local_now(position.page())?.0.saturating_add(1));
                            retry.set_packet(id, Some(p))?;
                        }
                        TransitEvent::Pull(p) => {
                            let mut pull = retry
                                .parent
                                .pulls
                                .get(&p)
                                .map(|p| p.after)
                                .unwrap_or_else(|| owner.transport.pulls.get(&p).copied())
                                .ok_or(MachineError::Missing)?;
                            pull.due = Tick(retry.local_now(position.page())?.0.saturating_add(1));
                            retry.set_pull(pull);
                        }
                    }
                    overlay.merge(retry.local);
                    continue;
                }
                MachineError::Bounds => {
                    work.deferred += 1;
                    break;
                }
                _ => return Err(error),
            }
        }
        for patch in stage.local.packets.values() {
            if let Some(p) = &patch.after
                && let Some(at) = p.at()
                && stage.page_clock(at.page()).active
                && p.due <= stage.local_now(at.page())?
                && !processed.contains(&(1, p.id))
            {
                agenda.insert((1, p.id), TransitEvent::Packet(p.id));
            }
        }
        wakes.extend(stage.wake_machines);
        overlay.merge(stage.local);
    }
    Ok(wakes)
}
impl Stage<'_> {
    fn linked(&mut self, a: BlockPos, b: BlockPos, color: u8) -> Result<bool, MachineError> {
        let Some(face) = direction(a, b) else {
            return Ok(false);
        };
        let ac = self.cell(a)?;
        let bc = self.cell(b)?;
        Ok(ac.id() == LOGISTICAL_TRANSPORTER
            && bc.id() == LOGISTICAL_TRANSPORTER
            && arms::arm(arms::arms_of(ac.state()), face as u8) == 1
            && arms::arm(arms::arms_of(bc.state()), face.opposite() as u8) == 1
            && (self.owner.configs.get(&b).map_or(0, |c| c.color) == 0
                || self.owner.configs.get(&b).is_some_and(|c| c.color == color)))
    }
    fn tick_packet(&mut self, mut packet: Packet) -> Result<(), MachineError> {
        self.work.packets += 1;
        let at = packet.at().ok_or(MachineError::Encoding)?;
        if !self.page_active(at.page())? {
            return Ok(());
        }
        let now = self.local_now(at.page())?;
        if self.cell(at)?.id() != LOGISTICAL_TRANSPORTER {
            return self.drop_packet(&packet);
        }
        if packet.last_tick >= now {
            packet.due = Tick(now.0.saturating_add(1));
            return self.set_packet(packet.id, Some(packet));
        }
        packet.last_tick = now;
        packet.due = Tick(now.0.saturating_add(1));
        packet.first_due = packet.due;
        packet.progress = packet.progress.saturating_add(5);
        if packet.progress >= 100 {
            if !packet.is_last() {
                let next = packet.path[usize::from(packet.step) + 1];
                if self.linked(at, next, packet.color)? {
                    packet.step += 1;
                    packet.from = at;
                    packet.progress -= 100;
                    let clock = self.ensure_clock(next.page(), true)?;
                    let next_now = clock.local_now(self.owner.now)?;
                    packet.last_tick = next_now;
                    packet.due = Tick(next_now.0.saturating_add(1));
                    packet.first_due = packet.due;
                    return self.set_packet(packet.id, Some(packet));
                }
                return self.reroute(packet, Some(next), true);
            }
            if let Some((_, target, _)) = packet.target() {
                let face = direction(at, target).ok_or(MachineError::Encoding)?;
                let target_cell = self.cell(target)?;
                let amount = if target_cell.id() != LOGISTICAL_TRANSPORTER
                    && arms::arm(arms::arms_of(self.cell(at)?.state()), face as u8) != 0
                {
                    self.push_items_result(at, face, packet.stack)?
                } else {
                    0
                };
                if amount >= packet.stack.count() {
                    return self.set_packet(packet.id, None);
                }
                packet.stack = packet
                    .stack
                    .with_count(packet.stack.count() - amount)
                    .map_err(|_| MachineError::Encoding)?;
                return self.reroute(packet, Some(target), true);
            }
            return self.reroute(packet, None, true);
        }
        if packet.progress == 50 {
            let accepted = if packet.is_last() {
                if let Some((_, target, face)) = packet.target() {
                    let outward = face.opposite();
                    let cell = self.cell(target)?;
                    if cell.id() != LOGISTICAL_TRANSPORTER
                        && arms::arm(arms::arms_of(self.cell(at)?.state()), outward as u8) != 0
                    {
                        if let Some((id, n)) =
                            self.transport_room(target, face, packet.stack, Some(packet.id))?
                        {
                            packet.destination = Some(id);
                            n > 0
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                } else {
                    false
                }
            } else {
                self.linked(at, packet.path[usize::from(packet.step) + 1], packet.color)?
            };
            if !accepted {
                return self.reroute(packet, None, false);
            }
        }
        self.set_packet(packet.id, Some(packet))
    }
    fn idle_line(
        &mut self,
        at: BlockPos,
        face: Face,
        color: u8,
    ) -> Result<Option<Vec<BlockPos>>, MachineError> {
        let mut path = vec![at];
        let mut p = at;
        while path.len() < MAX_PACKET_PATH {
            let q = offset(p, face)?;
            if !self.linked(p, q, color)? {
                break;
            }
            if self.work.route_nodes >= self.budget.route_nodes as u64 {
                return Err(MachineError::Bounds);
            }
            self.work.route_nodes += 1;
            path.push(q);
            p = q;
        }
        Ok((path.len() > 1).then_some(path))
    }
    fn reroute(
        &mut self,
        mut packet: Packet,
        refused: Option<BlockPos>,
        exit: bool,
    ) -> Result<(), MachineError> {
        let at = packet.at().ok_or(MachineError::Encoding)?;
        let back = refused.or(Some(packet.from));
        let mut found = self
            .route(
                at,
                packet.color,
                packet.stack,
                packet.home,
                None,
                Some(packet.id),
            )?
            .map(|route| (route.path, Goal::Destination, Some(route.destination), None));
        if found.is_none()
            && let Some(home) = packet.home
        {
            if let Some(route) = self.route(
                at,
                packet.color,
                packet.stack,
                None,
                Some(home),
                Some(packet.id),
            )? {
                found = Some((route.path, Goal::Home, Some(route.destination), None));
            } else {
                packet.home = None;
            }
        }
        if found.is_none()
            && let Some(face) = packet.idle_direction
            && let Some(path) = self.idle_line(at, face, packet.color)?
        {
            found = Some((path, Goal::Idle, None, Some(face)));
        }
        if found.is_none() {
            let reverse = back.and_then(|b| direction(at, b));
            for face in Face::ALL
                .into_iter()
                .filter(|f| Some(*f) != reverse)
                .chain(reverse)
            {
                if let Some(path) = self.idle_line(at, face, packet.color)? {
                    found = Some((path, Goal::Idle, None, Some(face)));
                    break;
                }
            }
        }
        let Some((path, goal, destination, idle_direction)) = found else {
            return self.drop_packet(&packet);
        };
        packet.path = path;
        packet.step = 0;
        packet.goal = goal;
        packet.destination = destination;
        packet.idle_direction = idle_direction;
        if let Some(p) = refused {
            packet.from = p;
            packet.progress = 0;
        } else if exit {
            packet.progress = 50;
        }
        self.set_packet(packet.id, Some(packet))
    }
    fn drop_packet(&mut self, packet: &Packet) -> Result<(), MachineError> {
        if self
            .parent
            .item_spawns
            .len()
            .saturating_add(self.local.item_spawns.len())
            >= self.budget.item_spawns
        {
            self.spawn_exhausted = true;
            return Err(MachineError::Bounds);
        }
        let at = packet.at().ok_or(MachineError::Encoding)?;
        let prev = packet.from;
        let entering = [at.x - prev.x, at.y - prev.y, at.z - prev.z];
        let direction = if packet.progress < 50 {
            entering
        } else {
            packet
                .path
                .get(usize::from(packet.step) + 1)
                .map_or(entering, |next| {
                    [next.x - at.x, next.y - at.y, next.z - at.z]
                })
        };
        let fraction = f64::from(packet.progress) / 100.0 - 0.5;
        let position = [
            f64::from(at.x) + 0.5 + f64::from(direction[0]) * fraction,
            f64::from(at.y) + 0.5 + f64::from(direction[1]) * fraction,
            f64::from(at.z) + 0.5 + f64::from(direction[2]) * fraction,
        ];
        let mut hash = blake3::Hasher::new();
        hash.update(b"mek-packet-drop-v1");
        hash.update(&at.space.0.to_le_bytes());
        hash.update(&packet.id.to_le_bytes());
        let id = EntityId(u64::from_le_bytes(
            hash.finalize().as_bytes()[..8]
                .try_into()
                .map_err(|_| MachineError::Encoding)?,
        ));
        let mut entity = ItemEntity::new(id, at.space, packet.stack, position, packet.id, 10)
            .map_err(|_| MachineError::Encoding)?;
        entity.last_tick = self.owner.now;
        self.local.item_spawns.push(entity);
        self.set_packet(packet.id, None)
    }
    fn set_pull(&mut self, pull: Pull) {
        let prefix = self
            .parent
            .pulls
            .get(&pull.position)
            .map(|p| p.after)
            .unwrap_or_else(|| self.owner.transport.pulls.get(&pull.position).copied());
        self.local
            .pulls
            .entry(pull.position)
            .and_modify(|p| p.after = Some(pull))
            .or_insert(PullPatch {
                before: prefix,
                after: Some(pull),
            });
    }
    fn tick_pull(&mut self, mut pull: Pull) -> Result<(), MachineError> {
        self.work.pulls += 1;
        let at = pull.position;
        if !self.page_active(at.page())? {
            return Ok(());
        }
        let now = self.local_now(at.page())?;
        let cell = self.cell(at)?;
        let config = self.owner.configs.get(&at).copied();
        let Some(config) = config.filter(|_| cell.id() == LOGISTICAL_TRANSPORTER) else {
            let count = self.page_count(at.page());
            if count <= 0 {
                return Err(MachineError::Encoding);
            }
            *self.local.page_counts.entry(at.page()).or_default() -= 1;
            if count == 1 {
                self.local.clock_bytes_delta -= 48;
            }
            self.local.pulls.insert(
                at,
                PullPatch {
                    before: Some(pull),
                    after: None,
                },
            );
            return Ok(());
        };
        let mut wait = 3;
        let mut failures = pull.failures;
        for face in Face::ALL {
            if arms::arm(config.modes, face as u8) != 2
                || arms::arm(arms::arms_of(cell.state()), face as u8) != 3
            {
                continue;
            }
            match self.pull_from(at, offset(at, face)?, face.opposite())? {
                None => {}
                Some(true) => {
                    wait = 10;
                    failures = 0;
                    break;
                }
                Some(false) => {
                    failures = failures.saturating_add(1);
                    wait = [2, 7, 20, 40][usize::from(failures.saturating_sub(1).min(3))];
                }
            }
        }
        pull.failures = failures;
        pull.due = Tick(now.0.saturating_add(wait + 1));
        pull.first_due = pull.due;
        self.set_pull(pull);
        Ok(())
    }
    fn pull_from(
        &mut self,
        at: BlockPos,
        source: BlockPos,
        face: Face,
    ) -> Result<Option<bool>, MachineError> {
        let cell = self.cell(source)?;
        let Some(id) = self.component_at(source)? else {
            return Ok(None);
        };
        let kind = self
            .world
            .component(id)
            .ok_or(MachineError::Missing)?
            .data
            .kind;
        let mut candidates = Vec::new();
        if kind == crate::survival::CHEST_COMPONENT {
            self.ensure_chest(id)?;
            for i in (0..27).rev() {
                let stack = self.local.chests[&id].after.slots()[i];
                if !stack.is_empty() {
                    candidates.push((i, stack, None));
                }
            }
        } else if kind == crate::survival::FURNACE_COMPONENT {
            self.ensure_furnace(id)?;
            let stack = self.local.furnaces[&id].after.slots()[2];
            if !stack.is_empty() {
                candidates.push((2, stack, None));
            }
        } else if kind == MACHINE_COMPONENT {
            let machine = self.machine(id)?;
            let facing = Face::ALL[blocks::facing_of(cell.raw()) as usize];
            let mode = machine.sides[sides::relative(facing, face) as usize];
            if !matches!(
                mode,
                machines::sides::SideMode::Output | machines::sides::SideMode::Both
            ) {
                return Ok(None);
            }
            if let Some(i) = machines::definition(machine.kind).output
                && !machine.slots[i].stack.is_empty()
            {
                let (stack, payload) = machine.slots[i]
                    .to_item()
                    .map_err(|_| MachineError::Encoding)?;
                candidates.push((i, stack, payload));
            }
        }
        if candidates.is_empty() {
            return Ok(None);
        }
        let mut attempted = Vec::<ItemStack>::new();
        for (index, stack, payload) in candidates {
            if attempted.iter().any(|s| s.same_kind(stack)) {
                continue;
            }
            attempted.push(stack);
            let mut temporary = None;
            if let Some(payload) = payload {
                let id = payload.id();
                match self.local.payloads.entry(id) {
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        entry.insert(payload);
                        temporary = Some(id);
                    }
                    std::collections::btree_map::Entry::Occupied(entry) => {
                        if entry.get() != &payload {
                            return Err(MachineError::Encoding);
                        }
                    }
                }
            }
            let accepted = self.spawn_packet(
                at,
                source,
                stack.with_count(1).map_err(|_| MachineError::Encoding)?,
            )?;
            if accepted == 0 {
                if let Some(id) = temporary {
                    self.local.payloads.remove(&id);
                }
                continue;
            }
            if kind == crate::survival::CHEST_COMPONENT {
                let old = self.local.chests[&id].after.slots()[index];
                self.local
                    .chests
                    .get_mut(&id)
                    .expect("loaded chest")
                    .after
                    .set_trusted(
                        index,
                        old.with_count(old.count() - 1)
                            .map_err(|_| MachineError::Encoding)?,
                    )
                    .map_err(|_| MachineError::Encoding)?;
            } else if kind == crate::survival::FURNACE_COMPONENT {
                let old = self.local.furnaces[&id].after.slots()[index];
                self.local
                    .furnaces
                    .get_mut(&id)
                    .expect("loaded furnace")
                    .after
                    .set_trusted(
                        index,
                        old.with_count(old.count() - 1)
                            .map_err(|_| MachineError::Encoding)?,
                    )
                    .map_err(|_| MachineError::Encoding)?;
            } else {
                let machine = self.machine_mut(id)?;
                let slot = &mut machine.slots[index];
                slot.stack = slot
                    .stack
                    .with_count(slot.stack.count() - 1)
                    .map_err(|_| MachineError::Encoding)?;
                if slot.stack.is_empty() {
                    slot.charge = 0;
                }
                self.wake_machines.insert(id);
            }
            return Ok(Some(true));
        }
        Ok(Some(false))
    }
}
