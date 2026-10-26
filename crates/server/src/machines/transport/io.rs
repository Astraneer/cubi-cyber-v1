//! World inventory adapters and capacity reservations used by live transit.
use super::*;
use crate::machines::view::Stage;
struct Payloads<'a> {
    local: &'a BTreeMap<rules::item_payload::PayloadId, rules::item_payload::Payload>,
    parent: &'a BTreeMap<rules::item_payload::PayloadId, rules::item_payload::Payload>,
    base: Option<&'a dyn rules::item_payload::Resolver>,
}
impl rules::item_payload::Resolver for Payloads<'_> {
    fn resolve(
        &self,
        id: rules::item_payload::PayloadId,
        item: rules::ItemId,
    ) -> Result<&rules::item_payload::Payload, rules::item_payload::Error> {
        if let Some(payload) = self.local.get(&id).or_else(|| self.parent.get(&id)) {
            if payload.item() != item {
                return Err(rules::item_payload::Error::Item);
            }
            return Ok(payload);
        }
        self.base
            .ok_or(rules::item_payload::Error::Missing)?
            .resolve(id, item)
    }
}
pub(super) fn put_chest(chest: &mut Chest, stack: ItemStack) -> Result<u16, MachineError> {
    let mut left = stack.count();
    for matching in [true, false] {
        for index in 0..27 {
            let old = chest.slots()[index];
            if matching && (old.is_empty() || !old.same_kind(stack)) || !matching && !old.is_empty()
            {
                continue;
            }
            let count = left.min(stack.maximum().saturating_sub(old.count()));
            if count > 0 {
                chest
                    .set_trusted(
                        index,
                        stack
                            .with_count(old.count() + count)
                            .map_err(|_| MachineError::Encoding)?,
                    )
                    .map_err(|_| MachineError::Overflow)?;
                left -= count;
            }
            if left == 0 {
                return Ok(stack.count());
            }
        }
    }
    Ok(stack.count() - left)
}
impl Stage<'_> {
    pub(super) fn page_count(&self, page: PageId) -> i64 {
        self.owner
            .transport
            .queues
            .get(&page)
            .map_or(0, |q| q.len()) as i64
            + self.parent.page_counts.get(&page).copied().unwrap_or(0)
            + self.local.page_counts.get(&page).copied().unwrap_or(0)
    }
    pub(in crate::machines) fn transport_slot(
        &self,
        stack: ItemStack,
    ) -> Result<machines::Slot, MachineError> {
        machines::Slot::from_item(
            stack,
            &Payloads {
                local: &self.local.payloads,
                parent: &self.parent.payloads,
                base: self.owner.payloads,
            },
        )
        .map_err(|_| MachineError::Encoding)
    }
    pub(super) fn effective_packet(&self, id: u64) -> Option<&Packet> {
        self.local
            .packets
            .get(&id)
            .map(|p| p.after.as_ref())
            .unwrap_or_else(|| {
                self.parent
                    .packets
                    .get(&id)
                    .map(|p| p.after.as_ref())
                    .unwrap_or_else(|| self.owner.transport.packets.get(&id))
            })
    }
    pub(super) fn set_packet(
        &mut self,
        id: u64,
        after: Option<Packet>,
    ) -> Result<(), MachineError> {
        let old = self.effective_packet(id);
        let old_count = i64::from(old.is_some());
        let old_bytes = old.map_or(0, Packet::bytes) as i64;
        let old_page = old.and_then(Packet::at).map(|p| p.page());
        let after_page = after.as_ref().and_then(Packet::at).map(|p| p.page());
        if old.is_some() && old_page.is_none() || after.is_some() && after_page.is_none() {
            return Err(MachineError::Encoding);
        }
        let changes = if old_page == after_page {
            [(None, 0), (None, 0)]
        } else {
            [(old_page, -1), (after_page, 1)]
        };
        let mut clock_bytes = 0;
        for (page, change) in changes {
            if let Some(page) = page {
                let before = self.page_count(page);
                let after = before + change;
                if before < 0 || after < 0 {
                    return Err(MachineError::Encoding);
                }
                if before == 0 && after > 0 {
                    clock_bytes += 48;
                } else if before > 0 && after == 0 {
                    clock_bytes -= 48;
                }
            }
        }
        let after_count = i64::from(after.is_some());
        let after_bytes = after.as_ref().map_or(0, Packet::bytes) as i64;
        let prefix = self
            .parent
            .packets
            .get(&id)
            .map(|p| p.after.as_ref())
            .unwrap_or_else(|| self.owner.transport.packets.get(&id))
            .cloned();
        let count = self.owner.transport.packets.len() as i64
            + self.parent.packet_count_delta
            + self.local.packet_count_delta
            + after_count
            - old_count;
        let bytes = self.owner.transport.bytes as i64
            + self.parent.packet_bytes_delta
            + self.local.packet_bytes_delta
            + self.parent.clock_bytes_delta
            + self.local.clock_bytes_delta
            + clock_bytes
            + after_bytes
            - old_bytes;
        if count < 0
            || count as usize > self.owner.packet_limit
            || bytes < 0
            || bytes as usize > MAX_TRANSPORT_BYTES
        {
            return Err(MachineError::Bounds);
        }
        self.local.packet_count_delta += after_count - old_count;
        self.local.packet_bytes_delta += after_bytes - old_bytes;
        self.local.clock_bytes_delta += clock_bytes;
        for (page, change) in changes {
            if let Some(page) = page {
                *self.local.page_counts.entry(page).or_default() += change;
            }
        }
        self.local
            .packets
            .entry(id)
            .and_modify(|p| p.after = after.clone())
            .or_insert(PacketPatch {
                before: prefix,
                after,
            });
        Ok(())
    }
    fn reservations(
        &mut self,
        target: ComponentId,
        skip: Option<u64>,
    ) -> Result<Vec<(ItemStack, Face)>, MachineError> {
        let mut ids = BTreeSet::new();
        if let Some(old) = self.owner.transport.targets.get(&target) {
            ids.extend(old.iter().copied());
        }
        ids.extend(self.parent.packets.keys().copied());
        ids.extend(self.local.packets.keys().copied());
        let mut out = Vec::new();
        for id in ids {
            if self.work.endpoint_queries >= self.budget.endpoints as u64 {
                return Err(MachineError::Bounds);
            }
            self.work.endpoint_queries += 1;
            if Some(id) == skip {
                continue;
            }
            if let Some(p) = self.effective_packet(id)
                && let Some((destination, _, face)) = p.target()
                && destination == target
            {
                out.push((p.stack, face));
            }
        }
        Ok(out)
    }
    pub(super) fn transport_room(
        &mut self,
        p: BlockPos,
        face: Face,
        stack: ItemStack,
        skip: Option<u64>,
    ) -> Result<Option<(ComponentId, u16)>, MachineError> {
        let cell = self.cell(p)?;
        let Some(id) = self.component_at(p)? else {
            return Ok(None);
        };
        let kind = self
            .world
            .component(id)
            .ok_or(MachineError::Missing)?
            .data
            .kind;
        if kind == MACHINE_COMPONENT {
            if !self.owner.machine_active(id) {
                return Ok(None);
            }
            let facing = Face::ALL[blocks::facing_of(cell.raw()) as usize];
            let mut candidate = self.machine(id)?;
            for (reserved, from) in self.reservations(id, skip)? {
                let n = candidate.insert_item(facing, from, self.transport_slot(reserved)?, false);
                if n < reserved.count() && from == face {
                    return Ok(None);
                }
            }
            let amount = candidate.insert_item(facing, face, self.transport_slot(stack)?, false);
            return Ok((amount > 0).then_some((id, amount)));
        }
        if kind == crate::survival::CHEST_COMPONENT
            && matches!(blocks::bdef(cell.id()).be, Some(BeKind::Chest))
        {
            self.ensure_chest(id)?;
            let mut candidate = self.local.chests.get(&id).expect("loaded chest").after;
            for (reserved, from) in self.reservations(id, skip)? {
                let n = put_chest(&mut candidate, reserved)?;
                if n < reserved.count() && from == face {
                    return Ok(None);
                }
            }
            let amount = put_chest(&mut candidate, stack)?;
            return Ok((amount > 0).then_some((id, amount)));
        }
        if kind == crate::survival::FURNACE_COMPONENT
            && matches!(blocks::bdef(cell.id()).be, Some(BeKind::Furnace))
        {
            self.ensure_furnace(id)?;
            let mut candidate = self.local.furnaces.get(&id).expect("loaded furnace").after;
            for (reserved, from) in self.reservations(id, skip)? {
                let n = put_furnace(&mut candidate, from, reserved)?;
                if n < reserved.count() && from == face {
                    return Ok(None);
                }
            }
            let amount = put_furnace(&mut candidate, face, stack)?;
            return Ok((amount > 0).then_some((id, amount)));
        }
        Ok(None)
    }
    pub(super) fn route(
        &mut self,
        start: BlockPos,
        color: u8,
        stack: ItemStack,
        home: Option<BlockPos>,
        only: Option<BlockPos>,
        skip: Option<u64>,
    ) -> Result<Option<Route>, MachineError> {
        let budget = RouteBudget {
            nodes: self
                .budget
                .route_nodes
                .saturating_sub(self.work.route_nodes as usize)
                .min(MAX_ROUTE_NODES),
            candidates: self
                .budget
                .endpoints
                .saturating_sub(self.work.endpoint_queries as usize)
                .min(MAX_ROUTE_CANDIDATES),
        };
        let mut work = RouteWork::default();
        let result = find_route(
            self, start, color, stack, home, only, skip, budget, &mut work,
        );
        self.work.route_nodes += work.nodes;
        self.work.routes += 1;
        if matches!(result, Err(MachineError::Bounds))
            && (budget.nodes == MAX_ROUTE_NODES && work.nodes >= MAX_ROUTE_NODES as u64
                || budget.candidates == MAX_ROUTE_CANDIDATES
                    && work.candidates >= MAX_ROUTE_CANDIDATES as u64)
        {
            Err(MachineError::RouteCapacity)
        } else {
            result
        }
    }
    pub(in crate::machines) fn spawn_packet(
        &mut self,
        at: BlockPos,
        from: BlockPos,
        stack: ItemStack,
    ) -> Result<u16, MachineError> {
        let clock = self.ensure_clock(at.page(), false)?;
        let local_now = clock.local_now(self.owner.now)?;
        let due = if clock.active {
            local_now
        } else {
            Tick(local_now.0.saturating_add(1))
        };
        let color = self.owner.configs.get(&at).map_or(0, |c| c.color);
        let Some(route) = self.route(at, color, stack, Some(from), None, None)? else {
            return Ok(0);
        };
        let id = self
            .local
            .next_packet
            .or(self.parent.next_packet)
            .unwrap_or(self.owner.transport.next_packet);
        let next = id.checked_add(1).ok_or(MachineError::Overflow)?;
        let packet = Packet {
            id,
            stack: stack
                .with_count(route.amount)
                .map_err(|_| MachineError::Encoding)?,
            color,
            from,
            path: route.path,
            step: 0,
            progress: 0,
            goal: Goal::Destination,
            home: Some(from),
            idle_direction: None,
            destination: Some(route.destination),
            last_tick: Tick(due.0.saturating_sub(1)),
            due,
            first_due: due,
        };
        packet.validate()?;
        self.set_packet(id, Some(packet))?;
        self.local.next_packet = Some(next);
        Ok(route.amount)
    }
}
impl RouteWorld for Stage<'_> {
    fn cell(&mut self, p: BlockPos) -> Result<Cell, MachineError> {
        Stage::cell(self, p)
    }
    fn color(&self, p: BlockPos) -> u8 {
        self.owner.configs.get(&p).map_or(0, |c| c.color)
    }
    fn capacity(
        &mut self,
        p: BlockPos,
        face: Face,
        stack: ItemStack,
        skip: Option<u64>,
    ) -> Result<Option<(ComponentId, u16)>, MachineError> {
        if self.owner.active.is_some_and(|active| !active(p)) {
            return Ok(None);
        }
        if self.work.endpoint_queries >= self.budget.endpoints as u64 {
            return Err(MachineError::Bounds);
        }
        self.work.endpoint_queries += 1;
        self.transport_room(p, face, stack, skip)
    }
}
