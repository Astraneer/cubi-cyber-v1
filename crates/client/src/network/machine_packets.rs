//! Confirmed per-page pipe-item replica and constant-size GPU instance bridge.
//! The connection bounds its page map and supplies the certified public version.
use core_types::{PageId, Tick, Version};
use net::machine_packets::{self as wire, Frame, Pose, Request};
use rules::{
    entities::mob::Instance,
    items::{self, ItemKind},
};
use std::collections::BTreeMap;
pub const PIPE_INSTANCE_FLAG: u32 = 4096;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Capacity,
    Invalid,
    Time,
    Item,
    Page,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Apply {
    Applied(Request),
    Stale(Request),
    MissingBase(Request),
    WrongView(Request),
}
#[derive(Clone)]
pub struct PacketPage {
    page: PageId,
    view_version: Option<Version>,
    revision: Option<u64>,
    tick: Tick,
    rows: BTreeMap<u64, Pose>,
    maximum: usize,
}
impl PacketPage {
    pub fn new(page: PageId, maximum: usize) -> Result<Self, Error> {
        if maximum == 0 || maximum > wire::MAX_PACKETS {
            return Err(Error::Capacity);
        }
        Ok(Self {
            page,
            view_version: None,
            revision: None,
            tick: Tick(0),
            rows: BTreeMap::new(),
            maximum,
        })
    }
    pub fn len(&self) -> usize {
        self.rows.len()
    }
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
    pub fn revision(&self) -> Option<u64> {
        self.revision
    }
    pub fn view_version(&self) -> Option<Version> {
        self.view_version
    }
    pub fn tick(&self) -> Tick {
        self.tick
    }
    pub fn get(&self, id: u64) -> Option<Pose> {
        self.rows.get(&id).copied()
    }
    pub fn poses(&self) -> impl Iterator<Item = Pose> + '_ {
        self.rows.values().copied()
    }
    pub fn apply(&mut self, frame: Frame, certified: Version) -> Result<Apply, Error> {
        frame.validate().map_err(|_| Error::Invalid)?;
        if frame.page != self.page {
            return Err(Error::Page);
        }
        if frame.view_version != certified {
            return Ok(Apply::WrongView(Request::Full {
                page: self.page,
                view_version: certified,
                have: self.revision,
            }));
        }
        if self.view_version == Some(certified)
            && self.revision.is_some_and(|old| frame.revision <= old)
        {
            return Ok(Apply::Stale(Request::Ack {
                page: self.page,
                view_version: frame.view_version,
                revision: self.revision.expect("existing revision"),
            }));
        }
        if frame.base.is_some()
            && (frame.base != self.revision || self.view_version != Some(certified))
        {
            return Ok(Apply::MissingBase(Request::Full {
                page: self.page,
                view_version: certified,
                have: self.revision,
            }));
        }
        if self.revision.is_some() && frame.tick < self.tick {
            return Err(Error::Time);
        }
        wire::resulting_ids(&frame, self.rows.keys().copied(), self.maximum)
            .map_err(|_| Error::Capacity)?;
        if frame
            .upserts
            .iter()
            .any(|p| matches!(items::idef(p.item).kind, ItemKind::None))
        {
            return Err(Error::Item);
        }
        // Every possible failure has been checked. Only this page is changed;
        // no clone or scan of other confirmed AOI pages occurs.
        if frame.base.is_none() {
            self.rows.clear();
        }
        for id in frame.removes {
            self.rows.remove(&id);
        }
        for pose in frame.upserts {
            self.rows.insert(pose.id, pose);
        }
        self.revision = Some(frame.revision);
        self.view_version = Some(certified);
        self.tick = frame.tick;
        Ok(Apply::Applied(Request::Ack {
            page: self.page,
            view_version: certified,
            revision: frame.revision,
        }))
    }
    /// Retained output storage, no model vertices or per-frame heap operations.
    /// A caller combining pages resolves duplicate IDs by the newest page tick.
    pub fn instances(
        &self,
        tick: Tick,
        alpha: f64,
        origin: [i64; 3],
        out: &mut [Instance],
    ) -> Result<usize, Error> {
        if !alpha.is_finite() {
            return Err(Error::Time);
        }
        if self.rows.len() > out.len() {
            return Err(Error::Capacity);
        }
        for (pose, instance) in self.rows.values().zip(out.iter_mut()) {
            *instance = instance_for(*pose, self.page, tick, alpha, origin);
        }
        Ok(self.rows.len())
    }
}

#[path = "machine_packets/scene.rs"]
mod scene;
pub use scene::PacketScene;
fn instance_for(pose: Pose, page: PageId, tick: Tick, alpha: f64, origin: [i64; 3]) -> Instance {
    let block = matches!(items::idef(pose.item).kind, ItemKind::Block(_));
    Instance {
        position: pose
            .relative_position(page, tick, alpha, origin)
            .map(|v| v as f32),
        body_yaw: 45.,
        head_yaw: 45.,
        pitch: 0.,
        limb_phase: 0.,
        limb_amount: 0.,
        kind: if block { 6 } else { 7 },
        flags: 32 | PIPE_INSTANCE_FLAG,
        color: u32::from(pose.item),
        id_low: pose.id as u32,
        id_high: (pose.id >> 32) as u32,
        death_roll: 0.,
        scale: if block { 0.2 } else { 0.4 },
        reserved: 0.,
    }
}
