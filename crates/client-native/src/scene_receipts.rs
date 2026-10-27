//! Opt-in host receipts for actual terrain Scene submissions. No GPU objects,
//! callbacks, queries, readiness changes or private world data are introduced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    pub camera: u64,
    pub world: u64,
    pub visual: u64,
    pub uploads: u64,
    pub extent: [u32; 2],
    pub samples: u32,
    pub temporal: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pending {
    frame: u64,
    owner: u64,
    stamp: Stamp,
    terrain_passes: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Record {
    pub event: &'static str,
    pub reason: &'static str,
    pub frame: u64,
    pub owner: u64,
    pub stamp: Stamp,
    pub terrain_passes: u32,
    pub completed_through: u64,
}
impl Record {
    pub fn print(self, camera: Option<([[f32; 4]; 4], [i64; 3])>) {
        let (matrix, origin) = camera.map_or((None, None), |(m, o)| (Some(m), Some(o)));
        // The completed matrix comes from the actual committed renderer cache;
        // a submit carries the exact base-camera signature used by encode.
        println!(
            "NATIVE_SCENE_RECEIPT {{\"event\":\"{}\",\"reason\":\"{}\",\"frame\":{},\"owner\":{},\"camera\":{},\"world\":{},\"visual\":{},\"uploads\":{},\"extent\":{:?},\"samples\":{},\"temporal\":{},\"terrain_passes\":{},\"completed_through\":{},\"camera_matrix\":{},\"origin\":{}}}",
            self.event,
            self.reason,
            self.frame,
            self.owner,
            self.stamp.camera,
            self.stamp.world,
            self.stamp.visual,
            self.stamp.uploads,
            self.stamp.extent,
            self.stamp.samples,
            self.stamp.temporal,
            self.terrain_passes,
            self.completed_through,
            matrix.map_or_else(|| "null".into(), |m| format!("{m:?}")),
            origin.map_or_else(|| "null".into(), |o| format!("{o:?}")),
        );
    }
}
pub struct Tracker {
    owner: u64,
    pending: Option<Pending>,
    last_completed: u64,
}
const _: () = assert!(std::mem::size_of::<Tracker>() <= 256);
impl Tracker {
    pub fn from_env() -> Option<Self> {
        (std::env::var("CUBI_NATIVE_SCENE_RECEIPTS").as_deref() == Ok("1")).then_some(Self::new())
    }
    fn new() -> Self {
        Self {
            owner: 0,
            pending: None,
            last_completed: 0,
        }
    }
    pub fn pending_frame(&self) -> Option<u64> {
        self.pending.map(|p| p.frame)
    }
    pub fn replace_owner(&mut self) -> Option<Record> {
        let retired = self
            .pending
            .take()
            .map(|p| Self::record(p, "rejected", "owner_retired", 0));
        self.owner = self
            .owner
            .checked_add(1)
            .expect("scene receipt owner exhausted");
        retired
    }
    pub fn submitted(&mut self, frame: u64, stamp: Stamp, terrain_passes: u32) -> Option<Record> {
        if terrain_passes == 0 {
            return None;
        }
        let p = Pending {
            frame,
            owner: self.owner,
            stamp,
            terrain_passes,
        };
        if frame == 0 || frame <= self.last_completed || self.pending.is_some() {
            return Some(Self::record(p, "rejected", "invalid_or_busy_submit", 0));
        }
        self.pending = Some(p);
        Some(Self::record(p, "submitted", "terrain_scene", 0))
    }
    pub fn discard(&mut self, frame: u64) -> Option<Record> {
        if self.pending.is_none_or(|p| p.frame != frame) {
            return None;
        }
        self.pending
            .take()
            .map(|p| Self::record(p, "rejected", "discarded", 0))
    }
    pub fn completed(
        &mut self,
        completed: u64,
        rendered: Stamp,
        validated_commit: Option<u64>,
    ) -> Option<Record> {
        let p = self.pending?;
        if completed < p.frame {
            return None;
        }
        self.pending = None;
        let reason = if p.owner != self.owner {
            "owner_mismatch"
        } else if p.stamp.temporal && validated_commit != Some(p.frame) {
            "missing_validated_commit"
        } else if p.stamp != rendered {
            "rendered_stamp_mismatch"
        } else {
            self.last_completed = p.frame;
            return Some(Self::record(
                p,
                "completed",
                "gpu_complete_and_presented",
                completed,
            ));
        };
        Some(Self::record(p, "rejected", reason, completed))
    }
    fn record(p: Pending, event: &'static str, reason: &'static str, completed: u64) -> Record {
        Record {
            event,
            reason,
            frame: p.frame,
            owner: p.owner,
            stamp: p.stamp,
            terrain_passes: p.terrain_passes,
            completed_through: completed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn stamp(temporal: bool) -> Stamp {
        Stamp {
            camera: 11,
            world: 22,
            visual: 33,
            uploads: 44,
            extent: [960, 540],
            samples: if temporal { 1 } else { 4 },
            temporal,
        }
    }
    #[test]
    fn cached_and_post_only_never_create_scene_receipts() {
        let mut t = Tracker::new();
        assert!(t.submitted(1, stamp(true), 0).is_none());
        assert!(t.pending_frame().is_none());
        assert!(t.completed(99, stamp(true), None).is_none());
    }
    #[test]
    fn late_exact_temporal_completion_requires_the_validated_commit() {
        let mut t = Tracker::new();
        let s = stamp(true);
        assert_eq!(t.submitted(5, s, 3).unwrap().event, "submitted");
        assert!(t.completed(4, s, None).is_none());
        assert_eq!(t.pending_frame(), Some(5));
        let r = t.completed(8, s, Some(5)).unwrap();
        assert_eq!((r.event, r.frame, r.completed_through), ("completed", 5, 8));
        assert!(t.completed(8, s, Some(5)).is_none());
    }
    #[test]
    fn missing_or_wrong_commit_is_rejected() {
        for commit in [None, Some(4), Some(6)] {
            let mut t = Tracker::new();
            let s = stamp(true);
            t.submitted(5, s, 1);
            assert_eq!(
                t.completed(5, s, commit).unwrap().reason,
                "missing_validated_commit"
            );
        }
    }
    #[test]
    fn non_temporal_completion_uses_exact_rendered_stamp() {
        let mut t = Tracker::new();
        let s = stamp(false);
        t.submitted(5, s, 1);
        assert_eq!(t.completed(5, s, None).unwrap().event, "completed");
        let mut changed = s;
        changed.visual += 1;
        t.submitted(6, s, 1);
        assert_eq!(
            t.completed(6, changed, None).unwrap().reason,
            "rendered_stamp_mismatch"
        );
    }
    #[test]
    fn discard_reprepare_and_old_callbacks_cannot_certify_a_replacement() {
        let mut t = Tracker::new();
        let s = stamp(true);
        t.submitted(5, s, 1);
        assert_eq!(t.discard(5).unwrap().reason, "discarded");
        assert!(t.completed(5, s, Some(5)).is_none());
        t.submitted(6, s, 1);
        let old_owner = t.replace_owner().unwrap().owner;
        assert!(t.completed(6, s, Some(6)).is_none());
        let new_owner = t.submitted(7, s, 1).unwrap().owner;
        assert_ne!(old_owner, new_owner);
        assert!(t.completed(6, s, Some(6)).is_none());
        assert_eq!(t.completed(7, s, Some(7)).unwrap().event, "completed");
    }
    #[test]
    fn one_pending_bound_rejects_overlap_and_preserves_the_original() {
        let mut t = Tracker::new();
        let s = stamp(true);
        t.submitted(5, s, 1);
        assert_eq!(
            t.submitted(6, s, 1).unwrap().reason,
            "invalid_or_busy_submit"
        );
        assert_eq!(t.pending_frame(), Some(5));
        assert_eq!(t.completed(5, s, Some(5)).unwrap().frame, 5);
    }
}
