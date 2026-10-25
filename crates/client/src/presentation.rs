//! Counts completed GPU presentation frames between a committed edit and the
//! first submitted frame that represents it. Encoding alone is not completion.
use std::collections::VecDeque;
const MAX_GROUPS: usize = 64;
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameCompletion {
    pub frame: u64,
    pub edit_revision: u64,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct PresentationStats {
    pub measured_edits: u64,
    pub pending_edits: u64,
    pub max_frames: u64,
    pub overflow: bool,
}
struct Armed {
    first: u64,
    last: u64,
    completed_at: u64,
}
pub struct PresentationProbe {
    pending: VecDeque<Armed>,
    last_armed: u64,
    measured: u64,
    max_frames: u64,
    overflow: bool,
}
impl PresentationProbe {
    pub fn new(revision: u64) -> Self {
        Self {
            pending: VecDeque::with_capacity(MAX_GROUPS),
            last_armed: revision,
            measured: 0,
            max_frames: 0,
            overflow: false,
        }
    }
    /// Called in the same foreground turn as the transaction commit. Consecutive
    /// edits with the same completed-frame baseline share one bounded record.
    pub fn arm(&mut self, revision: u64, completed_at: u64) {
        if revision <= self.last_armed {
            return;
        }
        let first = self.last_armed + 1;
        self.last_armed = revision;
        if let Some(last) = self.pending.back_mut()
            && last.completed_at == completed_at
            && last.last.checked_add(1) == Some(first)
        {
            last.last = revision;
            return;
        }
        if self.pending.len() == MAX_GROUPS {
            self.overflow = true;
            return;
        }
        self.pending.push_back(Armed {
            first,
            last: revision,
            completed_at,
        });
    }
    /// The host supplies the queue completion callback's frame/revision pair.
    /// A later completion may cover multiple revisions coalesced into one frame.
    pub fn observe(&mut self, completion: FrameCompletion) {
        while let Some(front) = self.pending.front_mut() {
            if front.first > completion.edit_revision {
                break;
            }
            let last = front.last.min(completion.edit_revision);
            if completion.frame < front.completed_at {
                self.overflow = true;
            } else {
                self.max_frames = self.max_frames.max(completion.frame - front.completed_at);
                self.measured = self.measured.saturating_add(last - front.first + 1);
            }
            if last == front.last {
                self.pending.pop_front();
            } else {
                front.first = last + 1;
                break;
            }
        }
    }
    pub fn reset(&mut self, revision: u64) {
        self.pending.clear();
        self.last_armed = revision;
        self.measured = 0;
        self.max_frames = 0;
        self.overflow = false;
    }
    pub fn stats(&self) -> PresentationStats {
        PresentationStats {
            measured_edits: self.measured,
            pending_edits: self.pending.iter().map(|v| v.last - v.first + 1).sum(),
            max_frames: self.max_frames,
            overflow: self.overflow,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn one_old_frame_then_edit_is_two_completed_frames() {
        let mut probe = PresentationProbe::new(0);
        probe.arm(1, 10);
        probe.observe(FrameCompletion {
            frame: 11,
            edit_revision: 0,
        });
        assert_eq!(probe.stats().measured_edits, 0);
        probe.observe(FrameCompletion {
            frame: 12,
            edit_revision: 1,
        });
        assert_eq!(probe.stats().max_frames, 2);
        assert_eq!(probe.stats().measured_edits, 1);
        assert_eq!(probe.stats().pending_edits, 0);
    }
    #[test]
    fn a_third_completed_frame_is_detected_not_hidden_by_encode_counts() {
        let mut probe = PresentationProbe::new(0);
        probe.arm(1, 10);
        probe.observe(FrameCompletion {
            frame: 13,
            edit_revision: 1,
        });
        assert_eq!(probe.stats().max_frames, 3);
    }
    #[test]
    fn coalesced_edits_retain_each_commit_baseline() {
        let mut probe = PresentationProbe::new(0);
        probe.arm(3, 10);
        probe.observe(FrameCompletion {
            frame: 11,
            edit_revision: 2,
        });
        assert_eq!(probe.stats().measured_edits, 2);
        assert_eq!(probe.stats().pending_edits, 1);
        probe.arm(5, 11);
        probe.observe(FrameCompletion {
            frame: 12,
            edit_revision: 5,
        });
        assert_eq!(probe.stats().measured_edits, 5);
        assert_eq!(probe.stats().max_frames, 2);
        assert_eq!(probe.stats().pending_edits, 0);
        probe.observe(FrameCompletion {
            frame: 12,
            edit_revision: 5,
        });
        assert_eq!(probe.stats().measured_edits, 5);
    }
    #[test]
    fn bounded_probe_fails_closed_on_overflow_or_invalid_completion() {
        let mut probe = PresentationProbe::new(0);
        for n in 1..=65 {
            probe.arm(n, n);
        }
        assert!(probe.stats().overflow);
        assert_eq!(probe.pending.len(), 64);
        probe.reset(65);
        probe.arm(66, 100);
        probe.observe(FrameCompletion {
            frame: 99,
            edit_revision: 66,
        });
        assert!(probe.stats().overflow);
        assert_eq!(probe.stats().measured_edits, 0);
    }
}
