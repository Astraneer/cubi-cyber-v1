//! Numeric live-owner observations. Reading these metrics performs no world
//! scan, runtime clone, item decode, snapshot encoding or worker wait.
use super::*;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QueueLoad {
    pub entries: usize,
    /// Serialized pending-event rows only, excluding container headers and
    /// unrelated component/owner records. This is not a heap-memory estimate.
    pub event_payload_bytes: usize,
    pub oldest_overdue_ticks: u64,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LoadMetrics {
    pub tick: Tick,
    pub peers: usize,
    pub loaded_pages: usize,
    pub mobs: usize,
    pub active_mobs: usize,
    pub mob_phase_pending: bool,
    pub pending_mob_actors: usize,
    pub items: usize,
    pub active_items: usize,
    pub registered_machines: usize,
    pub machine_phase_pending: bool,
    pub machine_phase_tick: Option<Tick>,
    pub machine_phase_stage: Option<machines::OwnerStage>,
    pub survival: QueueLoad,
    pub machines: QueueLoad,
    pub topology_positions: usize,
    pub awaiting_durability: bool,
    pub checkpoint_pending: bool,
    pub durable_wal_bytes: u64,
}
impl Server {
    pub fn load_metrics(&self) -> LoadMetrics {
        LoadMetrics {
            tick: self.tick,
            peers: self.peers.len(),
            loaded_pages: self.world.loaded_pages(),
            mobs: self.runtimes.mobs.entities().len(),
            active_mobs: self.runtimes.mobs.entities().active_len(),
            mob_phase_pending: self.mob_owner.due,
            pending_mob_actors: self.mob_owner.remaining(),
            items: self.runtimes.items.entities().len(),
            active_items: self.runtimes.items.entities().active_len(),
            registered_machines: self.runtimes.machines.machine_count(),
            machine_phase_pending: self.runtimes.machines.owner_pending(),
            machine_phase_tick: self.runtimes.machines.owner_tick(),
            machine_phase_stage: self.runtimes.machines.owner_stage(),
            survival: QueueLoad {
                entries: self.runtimes.survival.len(),
                event_payload_bytes: self.runtimes.survival.len() * 48,
                oldest_overdue_ticks: self.runtimes.survival.oldest_overdue(self.tick),
            },
            machines: QueueLoad {
                entries: self.runtimes.machines.event_count(),
                event_payload_bytes: self.runtimes.machines.event_payload_bytes(),
                oldest_overdue_ticks: self.runtimes.machines.oldest_overdue(self.tick),
            },
            topology_positions: self.runtimes.machines.pending_topology().len(),
            awaiting_durability: self.pending.is_some(),
            checkpoint_pending: self.pending.as_ref().is_some_and(|p| p.checkpoint),
            durable_wal_bytes: self.writer.wal_bytes(),
        }
    }
}
