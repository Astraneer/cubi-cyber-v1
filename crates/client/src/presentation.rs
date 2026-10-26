//! Counts completed GPU presentation frames between a committed edit and the
//! first submitted frame that represents it. Encoding alone is not completion.
use std::collections::VecDeque;
const MAX_GROUPS: usize = 64;
/// The exact CPU upload stamp used by UiLayer. A later world frame can use the
/// same UI without creating a new UI presentation requirement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UiStamp {
    pub revision: u64,
    pub width: u32,
    pub height: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UiSubmissionReceipt {
    pub stamp: UiStamp,
    pub frame: u64,
}
/// Latches the first actually submitted/presented frame for each upload stamp.
/// Completion is still proved by the existing queue completion counter.
#[derive(Clone, Copy, Debug, Default)]
pub struct UiSubmission {
    last_submitted: u64,
    receipt: Option<UiSubmissionReceipt>,
}
impl UiSubmission {
    /// Surface or View replacement invalidates the old image even when the UI
    /// revision and dimensions happen to be unchanged. Frame IDs stay monotone.
    pub fn invalidate(&mut self) {
        self.receipt = None;
    }
    /// Call only after successful queue submission AND surface presentation,
    /// with the UI stamp encoded in that same foreground turn. Abandoned
    /// encoders never call this method; callbacks never move the receipt.
    pub fn submitted(&mut self, frame: u64, stamp: Option<UiStamp>) {
        if frame <= self.last_submitted {
            return;
        }
        self.last_submitted = frame;
        let Some(stamp) = stamp.filter(|stamp| stamp.width != 0 && stamp.height != 0) else {
            self.receipt = None;
            return;
        };
        if self.receipt.is_none_or(|receipt| receipt.stamp != stamp) {
            self.receipt = Some(UiSubmissionReceipt { stamp, frame });
        }
    }
    pub fn receipt(&self) -> Option<UiSubmissionReceipt> {
        self.receipt
    }
}
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
    fn ui(revision: u64, width: u32, height: u32) -> UiStamp {
        UiStamp {
            revision,
            width,
            height,
        }
    }
    #[test]
    fn completed_ui_does_not_wait_for_a_new_heavy_world_frame() {
        let mut receipt = UiSubmission::default();
        receipt.submitted(100, Some(ui(7, 780, 437)));
        let completed = 100;
        // This Scene is still pending, but the exact requested UI was already
        // displayed by frame100. The old latest-submission fence waited101.
        receipt.submitted(101, Some(ui(7, 780, 437)));
        assert_eq!(receipt.receipt().unwrap().frame, 100);
        assert!(completed >= receipt.receipt().unwrap().frame);
        assert!(completed < receipt.last_submitted);
    }
    #[test]
    fn uploaded_but_abandoned_and_coalesced_revisions_do_not_get_false_receipts() {
        let mut receipt = UiSubmission::default();
        receipt.submitted(10, Some(ui(4, 780, 437)));
        let abandoned_upload = ui(5, 780, 437);
        assert!(receipt.receipt().unwrap().stamp.revision < abandoned_upload.revision);
        // Revision5's encoder was abandoned. A subsequent upload coalesces it
        // into revision6; only its real submission can qualify the newer UI.
        receipt.submitted(11, Some(ui(6, 780, 437)));
        assert_eq!(receipt.receipt().unwrap().stamp.revision, 6);
        assert!(receipt.receipt().unwrap().frame > 10);
        receipt.submitted(10, Some(abandoned_upload)); // stale ID cannot undo it
        assert_eq!(receipt.receipt().unwrap().stamp.revision, 6);
        assert_eq!(receipt.receipt().unwrap().frame, 11);
    }
    #[test]
    fn resize_and_same_size_view_replacement_need_their_own_submissions() {
        let mut receipt = UiSubmission::default();
        receipt.submitted(20, Some(ui(8, 780, 437)));
        receipt.invalidate(); // same-size surface recreation/reprepare
        assert!(receipt.receipt().is_none());
        receipt.submitted(20, Some(ui(8, 780, 437)));
        assert!(receipt.receipt().is_none());
        receipt.submitted(21, Some(ui(8, 780, 437)));
        assert_eq!(receipt.receipt().unwrap().frame, 21);
        receipt.invalidate(); // actual resize, without changing UI revision
        receipt.submitted(22, Some(ui(8, 960, 540)));
        assert_eq!(receipt.receipt().unwrap().stamp, ui(8, 960, 540));
        assert_eq!(receipt.receipt().unwrap().frame, 22);
    }
    #[test]
    fn non_ui_or_zero_extent_work_cannot_certify_a_ui_image() {
        let mut receipt = UiSubmission::default();
        receipt.submitted(1, None);
        assert!(receipt.receipt().is_none());
        receipt.submitted(2, Some(ui(1, 0, 437)));
        assert!(receipt.receipt().is_none());
        receipt.submitted(3, Some(ui(1, 780, 437)));
        assert_eq!(receipt.receipt().unwrap().frame, 3);
    }
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
