//! Reproducible packet delay, jitter, loss and reorder with explicit backpressure.
use core_types::DeterministicRng;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug)]
pub struct NetworkProfile {
    pub delay_ms: u64,
    pub jitter_ms: u64,
    pub loss_percent: u8,
}
impl Default for NetworkProfile {
    fn default() -> Self {
        Self {
            delay_ms: 75,
            jitter_ms: 20,
            loss_percent: 2,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LinkCounters {
    pub sent: u64,
    pub delivered: u64,
    pub lost: u64,
    pub queued_bytes: usize,
    pub peak_bytes: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Backpressure;

pub struct SimulatedLink<T> {
    profile: NetworkProfile,
    rng: DeterministicRng,
    queue: BTreeMap<(u64, u64), (usize, T)>,
    maximum_packets: usize,
    maximum_bytes: usize,
    sequence: u64,
    counters: LinkCounters,
}
impl<T> SimulatedLink<T> {
    pub fn new(
        profile: NetworkProfile,
        seed: u64,
        maximum_packets: usize,
        maximum_bytes: usize,
    ) -> Self {
        assert!(profile.loss_percent <= 100 && profile.jitter_ms <= profile.delay_ms);
        Self {
            profile,
            rng: DeterministicRng::from_seed(seed),
            queue: BTreeMap::new(),
            maximum_packets,
            maximum_bytes,
            sequence: 0,
            counters: LinkCounters::default(),
        }
    }
    pub fn send(&mut self, now_ms: u64, bytes: usize, value: T) -> Result<(), Backpressure> {
        if self.queue.len() >= self.maximum_packets
            || bytes
                > self
                    .maximum_bytes
                    .saturating_sub(self.counters.queued_bytes)
        {
            return Err(Backpressure);
        }
        self.counters.sent += 1;
        if self.rng.below(100).expect("positive") < u64::from(self.profile.loss_percent) {
            self.counters.lost += 1;
            return Ok(());
        }
        let jitter = self
            .rng
            .below(self.profile.jitter_ms.saturating_mul(2).saturating_add(1))
            .expect("positive");
        let due = now_ms
            .saturating_add(self.profile.delay_ms - self.profile.jitter_ms)
            .saturating_add(jitter);
        self.sequence = self
            .sequence
            .checked_add(1)
            .expect("simulation sequence exhausted");
        self.queue.insert((due, self.sequence), (bytes, value));
        self.counters.queued_bytes += bytes;
        self.counters.peak_bytes = self.counters.peak_bytes.max(self.counters.queued_bytes);
        Ok(())
    }
    pub fn receive(&mut self, now_ms: u64) -> Option<T> {
        if self
            .queue
            .first_key_value()
            .is_none_or(|((due, _), _)| *due > now_ms)
        {
            return None;
        }
        let (_, (bytes, value)) = self.queue.pop_first().expect("due packet");
        self.counters.queued_bytes -= bytes;
        self.counters.delivered += 1;
        Some(value)
    }
    pub fn counters(&self) -> LinkCounters {
        self.counters
    }
}
