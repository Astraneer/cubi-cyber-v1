//! Durable dirty positions and derived affected-network guards. An accepted
//! voxel edit cannot feed a stale resource graph while a topology job waits.
use super::*;
use crate::topology_queue;
pub const PENDING_TOPOLOGY_CAPACITY: usize = 32768;
fn error(error: topology_queue::Error) -> MachineError {
    match error {
        topology_queue::Error::Bounds => MachineError::Bounds,
        topology_queue::Error::Stale => MachineError::Stale,
        topology_queue::Error::Overflow => MachineError::Overflow,
        _ => MachineError::Encoding,
    }
}
fn neighborhood(p: BlockPos) -> impl Iterator<Item = BlockPos> {
    std::iter::once(p).chain(
        Face::ALL
            .into_iter()
            .filter_map(move |face| offset(p, face).ok()),
    )
}
impl MachineRuntime {
    pub fn pending_topology(&self) -> &topology_queue::Queue {
        &self.topology_queue
    }
    pub fn extend_topology_registration(
        &self,
        mut delta: RuntimeDelta,
        positions: impl IntoIterator<Item = BlockPos>,
        now: Tick,
    ) -> Result<RuntimeDelta, MachineError> {
        self.validate_delta(&delta)?;
        let previous = delta
            .topology_queue
            .take()
            .map_or_else(|| self.topology_queue.plan(), Ok)
            .map_err(error)?;
        let queue = self
            .topology_queue
            .extend_registration(previous, positions, now)
            .map_err(error)?;
        if !queue.is_empty() {
            delta.topology_queue = Some(queue);
        }
        self.validate_delta(&delta)?;
        Ok(delta)
    }
    pub fn extend_topology_retry(
        &self,
        mut delta: RuntimeDelta,
        positions: impl IntoIterator<Item = BlockPos>,
        due: Tick,
    ) -> Result<RuntimeDelta, MachineError> {
        self.validate_delta(&delta)?;
        let previous = delta
            .topology_queue
            .take()
            .map_or_else(|| self.topology_queue.plan(), Ok)
            .map_err(error)?;
        let queue = self
            .topology_queue
            .extend_retry(previous, positions, due)
            .map_err(error)?;
        if !queue.is_empty() {
            delta.topology_queue = Some(queue);
        }
        self.validate_delta(&delta)?;
        Ok(delta)
    }
    pub fn extend_topology_completion(
        &self,
        mut delta: RuntimeDelta,
        positions: impl IntoIterator<Item = BlockPos>,
    ) -> Result<RuntimeDelta, MachineError> {
        self.validate_delta(&delta)?;
        let previous = delta
            .topology_queue
            .take()
            .map_or_else(|| self.topology_queue.plan(), Ok)
            .map_err(error)?;
        let queue = self
            .topology_queue
            .extend_completion(previous, positions)
            .map_err(error)?;
        if !queue.is_empty() {
            delta.topology_queue = Some(queue);
        }
        self.validate_delta(&delta)?;
        Ok(delta)
    }
    pub(super) fn topology_registration(
        &self,
        positions: &[BlockPos],
        now: Tick,
    ) -> Result<Option<topology_queue::Delta>, MachineError> {
        let delta = self
            .topology_queue
            .extend_registration(
                self.topology_queue.plan().map_err(error)?,
                positions.iter().copied(),
                now,
            )
            .map_err(error)?;
        Ok((!delta.is_empty()).then_some(delta))
    }
    pub(super) fn new_dirty_networks(&self, positions: &[BlockPos]) -> BTreeSet<NetworkKey> {
        positions
            .iter()
            .flat_map(|&p| neighborhood(p))
            .filter_map(|p| self.network_at(p))
            .collect()
    }
    pub(super) fn dirty_rechecks(&self, delta: &RuntimeDelta) -> BTreeSet<BlockPos> {
        let mut positions = BTreeSet::new();
        if let Some(queue) = &delta.topology_queue {
            positions.extend(queue.patches.iter().map(|p| p.position));
        }
        for patch in &delta.topology {
            for slot in &patch.records.before_slots {
                if let Some(state) = &slot.state
                    && state.network.is_some()
                {
                    let key = NetworkKey {
                        space: patch.space,
                        network: NetworkId {
                            slot: slot.index,
                            generation: state.generation,
                        },
                    };
                    if let Some(found) = self.dirty_networks.get(&key) {
                        positions.extend(found.iter().copied());
                    }
                }
            }
            for edit in std::iter::once(&patch.records.edit).chain(&patch.records.extra_edits) {
                let p = match *edit {
                    machines::grid::TopologyEdit::Add { position, .. }
                    | machines::grid::TopologyEdit::Remove { position }
                    | machines::grid::TopologyEdit::Relink { position, .. } => position,
                };
                positions.extend(neighborhood(BlockPos {
                    space: patch.space,
                    x: p[0],
                    y: p[1],
                    z: p[2],
                }));
            }
        }
        positions
    }
    pub(super) fn refresh_dirty(&mut self, positions: impl IntoIterator<Item = BlockPos>) {
        for p in positions {
            if let Some(previous) = self.dirty_positions.remove(&p) {
                for key in previous {
                    if let Some(found) = self.dirty_networks.get_mut(&key) {
                        found.remove(&p);
                        if found.is_empty() {
                            self.dirty_networks.remove(&key);
                        }
                    }
                }
            }
            if self.topology_queue.get(p).is_some() {
                let keys = self.new_dirty_networks(&[p]);
                for &key in &keys {
                    self.dirty_networks.entry(key).or_default().insert(p);
                }
                self.dirty_positions.insert(p, keys);
            }
        }
    }
    pub(super) fn restore_dirty(&mut self) {
        let positions = self
            .topology_queue
            .snapshot()
            .entries
            .into_iter()
            .map(|e| e.position)
            .collect::<Vec<_>>();
        self.refresh_dirty(positions);
    }
    pub(super) fn validate_topology_queue(&self, delta: &RuntimeDelta) -> Result<(), MachineError> {
        if let Some(queue) = &delta.topology_queue {
            self.topology_queue.validate(queue).map_err(error)?;
        }
        Ok(())
    }
    pub(super) fn acknowledge_topology_queue(
        &mut self,
        delta: Option<topology_queue::Delta>,
    ) -> Result<(), MachineError> {
        if let Some(queue) = delta {
            self.topology_queue.acknowledge(queue).map_err(error)?;
        }
        Ok(())
    }
}
impl ReadOwner<'_> {
    pub(super) fn network_dirty(&self, key: NetworkKey) -> bool {
        self.dirty_networks.contains_key(&key) || self.new_dirty.contains(&key)
    }
}
pub(super) fn restore_queue(
    snapshot: &topology_queue::Snapshot,
) -> Result<topology_queue::Queue, MachineError> {
    topology_queue::Queue::restore(PENDING_TOPOLOGY_CAPACITY, snapshot).map_err(error)
}
pub(super) fn new_queue() -> Result<topology_queue::Queue, MachineError> {
    topology_queue::Queue::new(PENDING_TOPOLOGY_CAPACITY).map_err(error)
}
