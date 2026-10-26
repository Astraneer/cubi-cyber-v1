//! Durable single-owner item transit. Paths and reservations are bounded; a
//! changed packet is published with its source/recipient world transaction.
use super::*;
pub const MAX_PACKET_PATH: usize = 128;
pub const MAX_TRANSPORT_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_TRANSPORT_DELTA_BYTES: usize = 8 * 1024 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Goal {
    Destination,
    Home,
    Idle,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Packet {
    pub id: u64,
    pub stack: ItemStack,
    pub color: u8,
    pub from: BlockPos,
    pub path: Vec<BlockPos>,
    pub step: u16,
    pub progress: u8,
    pub goal: Goal,
    pub home: Option<BlockPos>,
    pub idle_direction: Option<Face>,
    pub destination: Option<ComponentId>,
    pub last_tick: Tick,
    pub due: Tick,
    pub first_due: Tick,
}
impl Packet {
    pub fn at(&self) -> Option<BlockPos> {
        self.path.get(usize::from(self.step)).copied()
    }
    pub fn target(&self) -> Option<(ComponentId, BlockPos, Face)> {
        if self.goal == Goal::Idle {
            return None;
        }
        let p = *self.path.last()?;
        let previous = *self.path.get(self.path.len().checked_sub(2)?)?;
        Some((self.destination?, p, direction(p, previous)?))
    }
    pub fn is_last(&self) -> bool {
        usize::from(self.step) + if self.goal == Goal::Idle { 1 } else { 2 } >= self.path.len()
    }
    fn bytes(&self) -> usize {
        128usize.saturating_add(self.path.len().saturating_mul(16))
    }
    pub fn validate(&self) -> Result<(), MachineError> {
        let at = self.at().ok_or(MachineError::Encoding)?;
        if self.id == 0
            || self.stack.is_empty()
            || self.path.len() > MAX_PACKET_PATH
            || self.color > 18
            || self.progress > 99
            || self.first_due > self.due
            || self.path.iter().any(|p| p.space != at.space)
            || self.from.space != at.space
            || direction(at, self.from).is_none()
            || self.home.is_some_and(|p| p.space != at.space)
            || self
                .path
                .windows(2)
                .any(|p| direction(p[0], p[1]).is_none())
            || self.goal != Goal::Idle
                && (self.target().is_none() || usize::from(self.step) + 1 >= self.path.len())
            || self.goal == Goal::Idle && self.destination.is_some()
        {
            return Err(MachineError::Encoding);
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pull {
    pub position: BlockPos,
    pub order: u64,
    pub failures: u8,
    pub due: Tick,
    pub first_due: Tick,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TransitEvent {
    Pull(BlockPos),
    Packet(u64),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PacketPatch {
    pub before: Option<Packet>,
    pub after: Option<Packet>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PullPatch {
    pub before: Option<Pull>,
    pub after: Option<Pull>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransportSnapshot {
    pub next_packet: u64,
    pub next_pull_order: u64,
    pub packets: Vec<Packet>,
    pub pulls: Vec<Pull>,
    pub clocks: Vec<ClockRecord>,
}
impl Default for TransportSnapshot {
    fn default() -> Self {
        Self {
            next_packet: 1,
            next_pull_order: 0,
            packets: Vec::new(),
            pulls: Vec::new(),
            clocks: Vec::new(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransportDelta {
    next_packet_before: u64,
    next_packet_after: u64,
    next_pull_before: u64,
    next_pull_after: u64,
    pub packets: Vec<PacketPatch>,
    pub pulls: Vec<PullPatch>,
    pub clocks: Vec<ClockPatch>,
    legacy: bool,
}
impl TransportDelta {
    pub fn is_empty(&self) -> bool {
        self.next_packet_before == self.next_packet_after
            && self.next_pull_before == self.next_pull_after
            && self.packets.is_empty()
            && self.pulls.is_empty()
            && self.clocks.is_empty()
    }
}
pub(super) struct TransportOwner {
    next_packet: u64,
    next_pull_order: u64,
    packets: BTreeMap<u64, Packet>,
    pulls: BTreeMap<BlockPos, Pull>,
    targets: BTreeMap<ComponentId, BTreeSet<u64>>,
    positions: BTreeMap<BlockPos, BTreeSet<u64>>,
    pages: BTreeMap<PageId, BTreeSet<u64>>,
    queues: BTreeMap<PageId, BTreeMap<(Tick, u8, u64), TransitEvent>>,
    due_pages: BTreeMap<(Tick, u8, u64, PageId), ()>,
    page_min: BTreeMap<PageId, (Tick, u8, u64)>,
    clocks: BTreeMap<PageId, PageClock>,
    bytes: usize,
}
impl TransportOwner {
    pub(super) fn new() -> Self {
        Self {
            next_packet: 1,
            next_pull_order: 0,
            packets: BTreeMap::new(),
            pulls: BTreeMap::new(),
            targets: BTreeMap::new(),
            positions: BTreeMap::new(),
            pages: BTreeMap::new(),
            queues: BTreeMap::new(),
            due_pages: BTreeMap::new(),
            page_min: BTreeMap::new(),
            clocks: BTreeMap::new(),
            bytes: 0,
        }
    }
    pub(super) fn snapshot(&self) -> TransportSnapshot {
        TransportSnapshot {
            next_packet: self.next_packet,
            next_pull_order: self.next_pull_order,
            packets: self.packets.values().cloned().collect(),
            pulls: self.pulls.values().copied().collect(),
            clocks: self
                .clocks
                .iter()
                .map(|(&page, &clock)| ClockRecord { page, clock })
                .collect(),
        }
    }
    pub(super) fn delta(&self) -> TransportDelta {
        TransportDelta {
            next_packet_before: self.next_packet,
            next_packet_after: self.next_packet,
            next_pull_before: self.next_pull_order,
            next_pull_after: self.next_pull_order,
            packets: Vec::new(),
            pulls: Vec::new(),
            clocks: Vec::new(),
            legacy: false,
        }
    }
    pub(super) fn restore(
        config: Config,
        snapshot: &TransportSnapshot,
    ) -> Result<Self, MachineError> {
        if snapshot.packets.len() > config.packets
            || snapshot.pulls.len() > config.transmitters
            || snapshot.packets.windows(2).any(|p| p[0].id >= p[1].id)
            || snapshot
                .pulls
                .windows(2)
                .any(|p| p[0].position >= p[1].position)
            || snapshot.packets.iter().fold(
                snapshot
                    .pulls
                    .len()
                    .saturating_add(snapshot.clocks.len())
                    .saturating_mul(48),
                |total, p| total.saturating_add(p.bytes()),
            ) > MAX_TRANSPORT_BYTES
        {
            return Err(MachineError::Bounds);
        }
        let mut owner = Self::new();
        let mut delta = owner.delta();
        delta.next_packet_after = snapshot.next_packet;
        delta.next_pull_after = snapshot.next_pull_order;
        delta.packets = snapshot
            .packets
            .iter()
            .cloned()
            .map(|after| PacketPatch {
                before: None,
                after: Some(after),
            })
            .collect();
        delta.pulls = snapshot
            .pulls
            .iter()
            .copied()
            .map(|after| PullPatch {
                before: None,
                after: Some(after),
            })
            .collect();
        delta.clocks = snapshot
            .clocks
            .iter()
            .map(|r| ClockPatch {
                page: r.page,
                before: None,
                after: Some(r.clock),
            })
            .collect();
        if delta.clocks.is_empty() {
            owner.finish_clocks(&mut delta)?;
        }
        owner.apply(config, delta)?;
        Ok(owner)
    }
    pub(super) fn validate(
        &self,
        config: Config,
        delta: &TransportDelta,
    ) -> Result<(), MachineError> {
        if delta.legacy {
            let mut normalized = delta.clone();
            normalized.legacy = false;
            self.finish_clocks(&mut normalized)?;
            return self.validate(config, &normalized);
        }
        if self.next_packet != delta.next_packet_before
            || self.next_pull_order != delta.next_pull_before
            || delta.next_packet_after < self.next_packet
            || delta.next_packet_after == 0
            || delta.next_pull_after < self.next_pull_order
        {
            return Err(MachineError::Stale);
        }
        if delta.packets.len() > config.packets || delta.pulls.len() > config.transmitters {
            return Err(MachineError::Bounds);
        }
        let mut packets = self.packets.len();
        let mut pulls = self.pulls.len();
        let mut bytes = self.bytes;
        let mut ids = BTreeSet::new();
        let mut positions = BTreeSet::new();
        let mut orders = BTreeSet::new();
        for patch in &delta.packets {
            let packet = patch
                .after
                .as_ref()
                .or(patch.before.as_ref())
                .ok_or(MachineError::Encoding)?;
            if !ids.insert(packet.id)
                || self.packets.get(&packet.id) != patch.before.as_ref()
                || patch
                    .before
                    .as_ref()
                    .zip(patch.after.as_ref())
                    .is_some_and(|(a, b)| a.id != b.id)
            {
                return Err(MachineError::Stale);
            }
            if let Some(before) = &patch.before {
                packets -= 1;
                bytes = bytes
                    .checked_sub(before.bytes())
                    .ok_or(MachineError::Encoding)?;
            }
            if let Some(after) = &patch.after {
                after.validate()?;
                if after.id >= delta.next_packet_after
                    || patch.before.is_none() && after.id < self.next_packet
                {
                    return Err(MachineError::Encoding);
                }
                packets += 1;
                bytes = bytes
                    .checked_add(after.bytes())
                    .ok_or(MachineError::Overflow)?;
            }
        }
        for patch in &delta.pulls {
            let pull = patch.after.or(patch.before).ok_or(MachineError::Encoding)?;
            if !positions.insert(pull.position)
                || self.pulls.get(&pull.position).copied() != patch.before
                || patch
                    .before
                    .zip(patch.after)
                    .is_some_and(|(a, b)| a.position != b.position || a.order != b.order)
            {
                return Err(MachineError::Stale);
            }
            if patch.before.is_some() {
                pulls -= 1;
                bytes = bytes.checked_sub(48).ok_or(MachineError::Encoding)?;
            }
            if let Some(after) = patch.after {
                if after.first_due > after.due
                    || after.order >= delta.next_pull_after
                    || patch.before.is_none()
                        && (after.order < self.next_pull_order || !orders.insert(after.order))
                {
                    return Err(MachineError::Encoding);
                }
                pulls += 1;
                bytes = bytes.checked_add(48).ok_or(MachineError::Overflow)?;
            }
        }
        let clock_bytes = self.validate_clocks(delta)?;
        bytes = if clock_bytes < 0 {
            bytes.checked_sub((-clock_bytes) as usize)
        } else {
            bytes.checked_add(clock_bytes as usize)
        }
        .ok_or(MachineError::Encoding)?;
        if packets > config.packets || pulls > config.transmitters || bytes > MAX_TRANSPORT_BYTES {
            return Err(MachineError::Bounds);
        }
        Ok(())
    }
    pub(super) fn apply(
        &mut self,
        config: Config,
        mut delta: TransportDelta,
    ) -> Result<(), MachineError> {
        if delta.legacy {
            delta.legacy = false;
            self.finish_clocks(&mut delta)?;
        }
        self.validate(config, &delta)?;
        let affected = activity::affected(&delta)?;
        for page in &affected {
            if let Some(key) = self.page_min.remove(page) {
                self.due_pages.remove(&(key.0, key.1, key.2, *page));
            }
        }
        for patch in &delta.clocks {
            if patch.before.is_some() {
                self.clocks.remove(&patch.page);
                self.bytes -= 48;
            }
            if let Some(after) = patch.after {
                self.clocks.insert(patch.page, after);
                self.bytes += 48;
            }
        }
        for patch in delta.packets {
            if let Some(before) = patch.before {
                self.unindex(&before);
                self.bytes -= before.bytes();
                self.packets.remove(&before.id);
            }
            if let Some(after) = patch.after {
                self.index(&after);
                self.bytes += after.bytes();
                self.packets.insert(after.id, after);
            }
        }
        for patch in delta.pulls {
            if let Some(before) = patch.before {
                self.pulls.remove(&before.position);
                let page = before.position.page();
                if let Some(queue) = self.queues.get_mut(&page) {
                    queue.remove(&(before.due, 0, before.order));
                    if queue.is_empty() {
                        self.queues.remove(&page);
                    }
                }
                self.bytes -= 48;
            }
            if let Some(after) = patch.after {
                self.pulls.insert(after.position, after);
                self.queues
                    .entry(after.position.page())
                    .or_default()
                    .insert(
                        (after.due, 0, after.order),
                        TransitEvent::Pull(after.position),
                    );
                self.bytes += 48;
            }
        }
        for page in affected {
            self.refresh_page(page)?;
        }
        self.next_packet = delta.next_packet_after;
        self.next_pull_order = delta.next_pull_after;
        Ok(())
    }
    fn index(&mut self, p: &Packet) {
        if let Some((id, _, _)) = p.target() {
            self.targets.entry(id).or_default().insert(p.id);
        }
        if let Some(at) = p.at() {
            self.queues
                .entry(at.page())
                .or_default()
                .insert((p.due, 1, p.id), TransitEvent::Packet(p.id));
            self.positions.entry(at).or_default().insert(p.id);
            self.pages.entry(at.page()).or_default().insert(p.id);
        }
    }
    fn unindex(&mut self, p: &Packet) {
        if let Some((id, _, _)) = p.target() {
            remove_index(&mut self.targets, id, p.id);
        }
        if let Some(at) = p.at() {
            if let Some(queue) = self.queues.get_mut(&at.page()) {
                queue.remove(&(p.due, 1, p.id));
                if queue.is_empty() {
                    self.queues.remove(&at.page());
                }
            }
            remove_index(&mut self.positions, at, p.id);
            remove_index(&mut self.pages, at.page(), p.id);
        }
    }
}
fn remove_index<K: Ord + Copy>(map: &mut BTreeMap<K, BTreeSet<u64>>, key: K, id: u64) {
    if let Some(ids) = map.get_mut(&key) {
        ids.remove(&id);
        if ids.is_empty() {
            map.remove(&key);
        }
    }
}
pub(super) fn direction(a: BlockPos, b: BlockPos) -> Option<Face> {
    if a.space != b.space {
        return None;
    }
    Face::ALL
        .into_iter()
        .find(|&f| offset(a, f).ok() == Some(b))
}
#[path = "transport/codec.rs"]
mod codec;
impl MachineRuntime {
    pub fn transport_due(&self, now: Tick) -> bool {
        self.transport
            .due_pages
            .first_key_value()
            .is_some_and(|(key, _)| key.0 <= now)
    }
    pub fn packet_count(&self) -> usize {
        self.transport.packets.len()
    }
    pub fn packet(&self, id: u64) -> Option<&Packet> {
        self.transport.packets.get(&id)
    }
    pub fn transport_delta(&self) -> TransportDelta {
        self.transport.delta()
    }
    pub fn packets_in_pages(
        &self,
        pages: &[PageId],
        limit: usize,
        out: &mut Vec<Packet>,
    ) -> Result<(), MachineError> {
        if pages.len() > 4096 || limit > self.config.packets {
            return Err(MachineError::Bounds);
        }
        let mut ids = BTreeSet::new();
        for page in pages {
            if let Some(found) = self.transport.pages.get(page) {
                for &id in found {
                    ids.insert(id);
                    if ids.len() > limit {
                        return Err(MachineError::Bounds);
                    }
                }
            }
        }
        out.clear();
        out.extend(
            ids.into_iter()
                .filter_map(|id| self.transport.packets.get(&id).cloned()),
        );
        Ok(())
    }
}
#[path = "transport/route.rs"]
mod route;
pub use route::{Route, RouteBudget, RouteWork, RouteWorld, find_route};
#[path = "transport/io.rs"]
mod io;
#[path = "transport/tick.rs"]
mod tick;
pub(super) use tick::advance;
impl TransportOwner {
    pub(super) fn projected_delta(
        &self,
        overlay: &Overlay,
    ) -> Result<TransportDelta, MachineError> {
        let mut delta = self.delta();
        delta.next_packet_after = overlay.next_packet.unwrap_or(self.next_packet);
        delta.packets = overlay
            .packets
            .values()
            .filter(|p| p.before != p.after)
            .cloned()
            .collect();
        delta.pulls = overlay
            .pulls
            .values()
            .filter(|p| p.before != p.after)
            .copied()
            .collect();
        delta.clocks = overlay.clocks.values().copied().collect();
        // Generated overlays only contain validated packet positions.
        self.finish_clocks(&mut delta)?;
        Ok(delta)
    }
}
impl MachineRuntime {
    pub(super) fn register_pulls(
        &self,
        mut delta: RuntimeDelta,
        world: &World,
        positions: impl IntoIterator<Item = BlockPos>,
        now: Tick,
    ) -> Result<RuntimeDelta, MachineError> {
        let mut transport = delta
            .transport
            .take()
            .unwrap_or_else(|| self.transport.delta());
        let mut pulls: BTreeMap<_, _> = transport
            .pulls
            .into_iter()
            .map(|p| {
                (
                    p.after.or(p.before).expect("validated pull patch").position,
                    p,
                )
            })
            .collect();
        let mut seen = BTreeSet::new();
        for position in positions {
            if !seen.insert(position) {
                continue;
            }
            if seen.len() > 448 {
                return Err(MachineError::Bounds);
            }
            let CellRead::Known(cell) = world.read(position) else {
                return Err(MachineError::Unknown);
            };
            let config = delta
                .configs
                .iter()
                .find(|p| p.after.or(p.before).is_some_and(|c| c.position == position))
                .map(|p| p.after)
                .unwrap_or_else(|| self.transmitter_config(position));
            let wanted = cell.id() == LOGISTICAL_TRANSPORTER
                && config.is_some_and(|c| {
                    Face::ALL
                        .into_iter()
                        .any(|f| arms::arm(c.modes, f as u8) == 2)
                });
            let before = self.transport.pulls.get(&position).copied();
            let effective = pulls.get(&position).map(|p| p.after).unwrap_or(before);
            if wanted && effective.is_none() {
                let order = transport.next_pull_after;
                transport.next_pull_after = order.checked_add(1).ok_or(MachineError::Overflow)?;
                let clock = transport
                    .clocks
                    .iter()
                    .find(|p| p.page == position.page())
                    .and_then(|p| p.after)
                    .or_else(|| self.transport.clocks.get(&position.page()).copied())
                    .unwrap_or_default();
                let due = Tick(
                    clock
                        .local_now(now)?
                        .0
                        .checked_add(1)
                        .ok_or(MachineError::Overflow)?,
                );
                pulls.insert(
                    position,
                    PullPatch {
                        before,
                        after: Some(Pull {
                            position,
                            order,
                            failures: 0,
                            due,
                            first_due: due,
                        }),
                    },
                );
            } else if !wanted && effective.is_some() {
                pulls.insert(
                    position,
                    PullPatch {
                        before,
                        after: None,
                    },
                );
            }
        }
        transport.pulls = pulls
            .into_values()
            .filter(|p| p.before != p.after)
            .collect();
        self.transport.finish_clocks(&mut transport)?;
        if !transport.is_empty() {
            delta.transport = Some(transport);
        }
        self.validate_delta(&delta)?;
        Ok(delta)
    }
}
#[path = "transport/furnace.rs"]
mod furnace;
pub(super) use furnace::put_furnace;
#[path = "transport/activity.rs"]
mod activity;
pub use activity::{ClockPatch, ClockRecord, PageClock};

impl MachineRuntime {
    pub fn packets_in_page(&self, page: PageId) -> impl Iterator<Item = &transport::Packet> {
        self.transport
            .pages
            .get(&page)
            .into_iter()
            .flat_map(|ids| ids.iter())
            .filter_map(|id| self.transport.packets.get(id))
    }
}
