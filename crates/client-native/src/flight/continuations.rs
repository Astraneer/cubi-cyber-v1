//! Event-loop polls are CPU work belonging to the next presentation. Counting
//! them as frames would turn one slow GPU frame into many apparent fast frames.
use telemetry::{AllocationCounts, flight::Sample};

#[derive(Default)]
pub(super) struct CpuContinuations {
    retained: Option<Sample>,
}
impl CpuContinuations {
    pub(super) fn push(&mut self, sample: Sample) {
        match &mut self.retained {
            Some(retained) => merge(retained, &sample),
            None => self.retained = Some(sample),
        }
    }
    pub(super) fn finish(&mut self, sample: &mut Sample) {
        if let Some(retained) = self.retained.take() {
            merge(sample, &retained);
        }
    }
}
fn merge(sample: &mut Sample, earlier: &Sample) {
    sample.cpu_ns = sample
        .cpu_ns
        .zip(earlier.cpu_ns)
        .map(|(a, b)| a.saturating_add(b));
    for (current, prior) in sample.cpu_stages_ns.iter_mut().zip(earlier.cpu_stages_ns) {
        if let Some(prior) = prior {
            *current = Some(current.unwrap_or(0).saturating_add(prior));
        }
    }
    sample.work.accumulate(earlier.work);
    sample.allocations = sample
        .allocations
        .zip(earlier.allocations)
        .map(|(mut current, prior)| {
            add_allocations(&mut current, prior);
            current
        });
    sample.observations.0 |= earlier.observations.0;
}
fn add_allocations(current: &mut AllocationCounts, prior: AllocationCounts) {
    macro_rules! add {
        ($($field:ident),+) => { $(current.$field = current.$field.saturating_add(prior.$field);)+ };
    }
    add!(
        alloc_calls,
        zeroed_alloc_calls,
        realloc_calls,
        dealloc_calls,
        failed_calls,
        allocated_bytes,
        released_bytes
    );
}
#[cfg(test)]
mod tests {
    use super::*;
    use telemetry::{WorkCounts, flight::Observations};
    #[test]
    fn fifty_cpu_polls_remain_one_presentation_and_preserve_work_allocations_and_unknowns() {
        let mut owner = CpuContinuations::default();
        for _ in 0..50 {
            owner.push(Sample {
                cpu_ns: Some(1_000_000),
                cpu_stages_ns: [None, Some(300_000), None, None, None, None, None, None],
                work: WorkCounts {
                    cells_scanned: 8,
                    ..WorkCounts::ZERO
                },
                allocations: Some(AllocationCounts {
                    alloc_calls: 1,
                    allocated_bytes: 32,
                    ..AllocationCounts::ZERO
                }),
                observations: Observations(Observations::GPU_WAIT),
                ..Sample::default()
            });
        }
        let mut actual = Sample {
            frame: 7,
            monotonic_ns: 2_000_000_000,
            interval_ns: 1_000_000_000,
            cpu_ns: Some(2_000_000),
            allocations: Some(AllocationCounts::ZERO),
            work: WorkCounts {
                upload_bytes: 128,
                ..WorkCounts::ZERO
            },
            ..Sample::default()
        };
        owner.finish(&mut actual);
        assert_eq!(actual.frame, 7);
        assert_eq!(actual.interval_ns, 1_000_000_000);
        assert_eq!(actual.cpu_ns, Some(52_000_000));
        assert_eq!(actual.cpu_stages_ns[1], Some(15_000_000));
        assert_eq!(actual.work.cells_scanned, 400);
        assert_eq!(actual.work.upload_bytes, 128);
        assert_eq!(actual.allocations.unwrap().alloc_calls, 50);
        assert_eq!(actual.allocations.unwrap().allocated_bytes, 1600);
        assert_ne!(actual.observations.0 & Observations::GPU_WAIT, 0);
        let mut recorder = telemetry::flight::FlightRecorder::new(16, 16).unwrap();
        recorder.record_frame(actual).unwrap();
        assert_eq!(recorder.frames().count(), 1);
        assert_eq!(recorder.frames().next().unwrap().interval_ns, 1_000_000_000);
        let before = actual;
        owner.finish(&mut actual);
        assert_eq!(actual, before, "a continuation is consumed only once");
        owner.push(Sample::default());
        owner.finish(&mut actual);
        assert_eq!(actual.cpu_ns, None);
        assert_eq!(actual.allocations, None, "unmeasured work stays unknown");
    }
}
