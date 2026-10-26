//! Derived agenda indexes; no schema or full-snapshot work is required to read
//! the oldest original deadline after a retry or to account encoded event rows.
use super::*;
fn bytes(p: Pending) -> usize {
    match p.event {
        Event::Machine(_) => 33,
        Event::Network(_) => 37,
    }
}
impl MachineRuntime {
    pub fn machine_count(&self) -> usize {
        self.machines.len()
    }
    pub fn event_payload_bytes(&self) -> usize {
        self.event_bytes
    }
    pub fn oldest_overdue(&self, now: Tick) -> u64 {
        self.event_ages
            .first_key_value()
            .map_or(0, |(due, _)| now.0.saturating_sub(due.0))
    }
    pub(super) fn remove_pending_indexed(&mut self, event: Event) -> Option<Pending> {
        let key = self.keys.remove(&event)?;
        let old = self.events.remove(&key).expect("agenda key index");
        let count = self
            .event_ages
            .get_mut(&old.first_due)
            .expect("agenda age index");
        *count -= 1;
        if *count == 0 {
            self.event_ages.remove(&old.first_due);
        }
        self.event_bytes -= bytes(old);
        Some(old)
    }
    pub(super) fn insert_pending_indexed(&mut self, pending: Pending) {
        self.remove_pending_indexed(pending.event);
        self.keys.insert(pending.event, queue_key(pending));
        self.events.insert(queue_key(pending), pending);
        *self.event_ages.entry(pending.first_due).or_default() += 1;
        self.event_bytes += bytes(pending);
    }
}
