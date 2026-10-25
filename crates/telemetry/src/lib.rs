//! Allocation-free logical work recording and opt-in allocation measurements.
//!
//! The recorder does not infer timings or promise that a renderer is allocation-free.
//! Callers supply measurements. The allocator observes only the measuring thread;
//! worker threads must establish their own scope if their work is to be measured.

pub mod allocator;
pub mod flight;

pub use allocator::{AllocationCounts, AllocationScope, CountingAllocator, ScopeError};

/// Logical work, independent of the speed of the machine running a test.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorkCounts {
    pub cells_scanned: u64,
    pub quads_emitted: u64,
    pub upload_bytes: u64,
    pub pipeline_compilations: u64,
    pub synchronous_generations: u64,
}

impl WorkCounts {
    pub const ZERO: Self = Self {
        cells_scanned: 0,
        quads_emitted: 0,
        upload_bytes: 0,
        pipeline_compilations: 0,
        synchronous_generations: 0,
    };

    /// Saturation prevents a long recording session from wrapping to a small value.
    pub fn accumulate(&mut self, other: Self) {
        self.cells_scanned = self.cells_scanned.saturating_add(other.cells_scanned);
        self.quads_emitted = self.quads_emitted.saturating_add(other.quads_emitted);
        self.upload_bytes = self.upload_bytes.saturating_add(other.upload_bytes);
        self.pipeline_compilations = self
            .pipeline_compilations
            .saturating_add(other.pipeline_compilations);
        self.synchronous_generations = self
            .synchronous_generations
            .saturating_add(other.synchronous_generations);
    }
}

/// Counters belong to one subsystem/worker; combine snapshots at frame boundaries.
#[derive(Clone, Copy, Debug, Default)]
pub struct WorkCounters {
    counts: WorkCounts,
}

impl WorkCounters {
    pub const fn new() -> Self {
        Self {
            counts: WorkCounts::ZERO,
        }
    }

    pub fn record(&mut self, work: WorkCounts) {
        self.counts.accumulate(work);
    }

    pub const fn snapshot(&self) -> WorkCounts {
        self.counts
    }

    pub fn reset(&mut self) {
        self.counts = WorkCounts::ZERO;
    }

    /// Returns this interval and starts an empty one without allocating.
    pub fn take(&mut self) -> WorkCounts {
        let counts = self.counts;
        self.reset();
        counts
    }
}

/// One frame's externally measured times and work. Unknown GPU time stays `None`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameRecord {
    pub frame: u64,
    pub cpu_ns: u64,
    pub gpu_ns: Option<u64>,
    pub work: WorkCounts,
}

impl FrameRecord {
    pub const EMPTY: Self = Self {
        frame: 0,
        cpu_ns: 0,
        gpu_ns: None,
        work: WorkCounts::ZERO,
    };
}

/// A bounded frame history. Storage is reserved inline before the frame loop.
///
/// Oldest samples are overwritten at capacity. The caller chooses capacity for its
/// frame-rate/capture window; M0 does not assume that 600 samples always span 10 s.
#[derive(Debug)]
pub struct FrameRecorder<const N: usize> {
    records: [FrameRecord; N],
    next: usize,
    len: usize,
}

impl<const N: usize> Default for FrameRecorder<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> FrameRecorder<N> {
    pub const fn new() -> Self {
        assert!(N > 0, "frame recorder capacity must be positive");
        Self {
            records: [FrameRecord::EMPTY; N],
            next: 0,
            len: 0,
        }
    }

    /// Adds a sample; returns the displaced oldest sample when the buffer was full.
    pub fn push(&mut self, record: FrameRecord) -> Option<FrameRecord> {
        let replaced = if self.len == N {
            Some(self.records[self.next])
        } else {
            None
        };
        self.records[self.next] = record;
        self.next = (self.next + 1) % N;
        self.len = (self.len + 1).min(N);
        replaced
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub const fn capacity(&self) -> usize {
        N
    }

    pub fn clear(&mut self) {
        self.next = 0;
        self.len = 0;
    }

    /// Chronological access without copying, boxing or allocating an iterator.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &FrameRecord> + DoubleEndedIterator {
        let oldest = if self.len == N { self.next } else { 0 };
        (0..self.len).map(move |offset| &self.records[(oldest + offset) % N])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_intervals_are_independent_and_saturating() {
        let mut counters = WorkCounters::new();
        counters.record(WorkCounts {
            cells_scanned: 4096,
            upload_bytes: 1024,
            ..WorkCounts::ZERO
        });
        let first = counters.take();
        assert_eq!(first.cells_scanned, 4096);
        assert_eq!(first.upload_bytes, 1024);
        assert_eq!(counters.snapshot(), WorkCounts::ZERO);
        counters.record(WorkCounts {
            cells_scanned: u64::MAX,
            ..WorkCounts::ZERO
        });
        counters.record(WorkCounts {
            cells_scanned: 1,
            ..WorkCounts::ZERO
        });
        assert_eq!(counters.snapshot().cells_scanned, u64::MAX);
    }

    #[test]
    fn bounded_recorder_evicts_oldest_and_preserves_order() {
        let mut recorder = FrameRecorder::<3>::new();
        for frame in 0..7 {
            let displaced = recorder.push(FrameRecord {
                frame,
                ..FrameRecord::EMPTY
            });
            assert_eq!(displaced.map(|value| value.frame), frame.checked_sub(3));
        }
        assert_eq!(recorder.len(), 3);
        assert_eq!(
            recorder
                .iter()
                .map(|record| record.frame)
                .collect::<Vec<_>>(),
            [4, 5, 6]
        );
        assert_eq!(
            recorder
                .iter()
                .rev()
                .map(|record| record.frame)
                .collect::<Vec<_>>(),
            [6, 5, 4]
        );
        recorder.clear();
        assert!(recorder.is_empty());
        assert_eq!(recorder.iter().count(), 0);
        recorder.push(FrameRecord {
            frame: 9,
            ..FrameRecord::EMPTY
        });
        assert_eq!(recorder.iter().next().unwrap().frame, 9);
    }

    #[test]
    #[should_panic(expected = "frame recorder capacity must be positive")]
    fn zero_capacity_is_rejected() {
        let _ = FrameRecorder::<0>::new();
    }
}
