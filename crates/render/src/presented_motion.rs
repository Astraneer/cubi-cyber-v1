//! Previous actual PRESENTED model poses for the optional motion raster.
//! All frame storage is reserved at loading; input order is never object identity.
use crate::{block_models::BlockInstance, entity_gpu::EntityInstance};
use bytemuck::{Pod, Zeroable};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_SURFACE: AtomicU64 = AtomicU64::new(1);
pub const INVALID_INDEX: u32 = u32::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ObjectKey {
    family: u16,
    space: u32,
    mapped: bool,
    identity: [u64; 2],
}
impl ObjectKey {
    pub fn space(self) -> u32 {
        self.space
    }
    /// Exact public 64-bit actor/packet ID, scoped by a typed family and Space.
    pub fn native(family: u16, space: u32, id: u64) -> Result<Self, Error> {
        Self::new(family, space, false, [id, 0])
    }
    /// Exact UUID or block-coordinate key. Its GPU token is allocated, never a
    /// hash/truncation. The full 128-bit value participates in CPU equality.
    pub fn named(family: u16, space: u32, identity: [u64; 2]) -> Result<Self, Error> {
        Self::new(family, space, true, identity)
    }
    fn new(family: u16, space: u32, mapped: bool, identity: [u64; 2]) -> Result<Self, Error> {
        if family == 0 || family > 127 {
            return Err(Error::InvalidIdentity);
        }
        Ok(Self {
            family,
            space,
            mapped,
            identity,
        })
    }
    fn namespace(self) -> u32 {
        (u32::from(self.family) * 2 + u32::from(self.mapped)) << 24
    }
}

/// This is the resolver's exact opaque frame+serial pair; there is one pending
/// transaction. The wrapper converts FrameToken through its two getters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Token {
    pub frame: u64,
    pub serial: u64,
}
impl From<crate::taa::FrameToken> for Token {
    fn from(value: crate::taa::FrameToken) -> Self {
        Self {
            frame: value.frame(),
            serial: value.serial(),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub space: u32,
    pub geometry_epoch: u64,
    pub source_epoch: u64,
    pub extent: [u32; 2],
    pub origin: [i64; 3],
    /// Exact camera used for the presented geometry, BEFORE temporal jitter.
    pub unjittered_view_projection: [[f32; 4]; 4],
    pub reset: bool,
}
impl Frame {
    fn valid(self) -> bool {
        !self.extent.contains(&0)
            && self
                .unjittered_view_projection
                .iter()
                .flatten()
                .all(|x| x.is_finite())
            && self
                .origin
                .iter()
                .all(|x| x.unsigned_abs() <= (1_u64 << 53))
    }
    fn compatible(self, old: Self) -> bool {
        !self.reset
            && self.space == old.space
            && self.extent == old.extent
            && self.geometry_epoch == old.geometry_epoch
            && self.source_epoch == old.source_epoch
    }
}
pub trait ModelPose: Copy {
    fn position(&self) -> [f32; 3];
    fn valid(&self) -> bool;
    fn model_key(&self) -> [u32; 4] {
        [0; 4]
    }
    fn primary_visible(&self) -> bool {
        true
    }
}
impl ModelPose for EntityInstance {
    fn model_key(&self) -> [u32; 4] {
        [self.kind, self.flags & 2, 0, 0]
    }
    fn primary_visible(&self) -> bool {
        self.flags & crate::entity_gpu::PRIMARY_HIDDEN == 0
    }
    fn position(&self) -> [f32; 3] {
        self.position
    }
    fn valid(&self) -> bool {
        self.position
            .iter()
            .chain([
                &self.body_yaw,
                &self.head_yaw,
                &self.pitch,
                &self.limb_phase,
                &self.limb_amount,
                &self.death_roll,
                &self.scale,
                &self.reserved,
            ])
            .all(|x| x.is_finite())
    }
}
impl ModelPose for BlockInstance {
    fn model_key(&self) -> [u32; 4] {
        [
            self.kind,
            self.connections,
            self.flags & crate::block_models::PORT_MARKER_ONLY,
            self.reserved[3] & 0x3ffff,
        ]
    }
    fn primary_visible(&self) -> bool {
        self.flags & crate::block_models::HIDDEN == 0
    }
    fn position(&self) -> [f32; 3] {
        self.position
    }
    fn valid(&self) -> bool {
        self.position
            .iter()
            .chain([&self.yaw, &self.phase, &self.amount])
            .all(|x| x.is_finite())
    }
}
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct RigidPose {
    /// Actual world-space transform relative to this Frame's integer origin.
    pub model: [[f32; 4]; 4],
}
impl ModelPose for RigidPose {
    fn position(&self) -> [f32; 3] {
        [self.model[3][0], self.model[3][1], self.model[3][2]]
    }
    fn valid(&self) -> bool {
        self.model.iter().flatten().all(|x| x.is_finite())
            && self.model[0][3] == 0.
            && self.model[1][3] == 0.
            && self.model[2][3] == 0.
            && self.model[3][3] == 1.
    }
}
#[derive(Clone, Copy)]
pub struct ObjectPose<P> {
    pub key: ObjectKey,
    /// Exact catalog/topology epoch, not an appearance hash or upload revision.
    pub topology: u64,
    pub pose: P,
    pub visible: bool,
    pub discontinuity: bool,
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
pub struct GpuMatch {
    /// [family in high8 | exact shader owner/part in low24, token lo/hi, generation].
    pub surface: [u32; 4],
    pub previous_index: u32,
    pub valid: u32,
    pub reserved: [u32; 2],
}
const _: () = assert!(std::mem::size_of::<GpuMatch>() == 32);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidLimits,
    InvalidIdentity,
    InvalidFrame,
    InvalidPose,
    Duplicate,
    Capacity,
    Busy,
    WrongToken,
    Exhausted,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Work {
    pub records: u64,
    pub identity_comparisons: u64,
    pub matched: u64,
    pub invalid: u64,
    pub new_surfaces: u64,
}
#[derive(Clone, Copy)]
struct Row {
    key: ObjectKey,
    topology: u64,
    model_key: [u32; 4],
    visible: bool,
    surface: [u32; 4],
}
struct Bank<P> {
    rows: Vec<Row>,
    poses: Vec<P>,
    sorted: Vec<u32>,
    matches: Vec<GpuMatch>,
}
impl<P> Bank<P> {
    fn new(maximum: usize) -> Self {
        Self {
            rows: Vec::with_capacity(maximum),
            poses: Vec::with_capacity(maximum),
            sorted: Vec::with_capacity(maximum),
            matches: Vec::with_capacity(maximum),
        }
    }
    fn clear(&mut self) {
        self.rows.clear();
        self.poses.clear();
        self.sorted.clear();
        self.matches.clear();
    }
    fn find(&self, key: ObjectKey, comparisons: &mut u64) -> Option<usize> {
        let mut lo = 0;
        let mut hi = self.sorted.len();
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let at = self.sorted[mid] as usize;
            *comparisons += 1;
            match self.rows[at].key.cmp(&key) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return Some(at),
            }
        }
        None
    }
}
pub struct PresentedMotion<P> {
    maximum: usize,
    teleport_squared: f64,
    front: Bank<P>,
    back: Bank<P>,
    presented: Option<(Token, Frame)>,
    pending: Option<(Token, Frame)>,
    last_serial: u64,
    work: Work,
}
impl<P: ModelPose> PresentedMotion<P> {
    pub fn new(maximum: usize, teleport_distance: f64) -> Result<Self, Error> {
        if maximum == 0
            || maximum > 65536
            || !teleport_distance.is_finite()
            || teleport_distance <= 0.
            || teleport_distance > 1e6
        {
            return Err(Error::InvalidLimits);
        }
        Ok(Self {
            maximum,
            teleport_squared: teleport_distance * teleport_distance,
            front: Bank::new(maximum),
            back: Bank::new(maximum),
            presented: None,
            pending: None,
            last_serial: 0,
            work: Work::default(),
        })
    }
    pub fn reserved_bytes(&self) -> usize {
        2 * self.maximum
            * (std::mem::size_of::<Row>()
                + std::mem::size_of::<P>()
                + 4
                + std::mem::size_of::<GpuMatch>())
    }
    pub fn presented_frame(&self) -> Option<(Token, Frame)> {
        self.presented
    }
    pub fn presented_poses(&self) -> &[P] {
        &self.front.poses
    }
    pub fn matches(&self, token: Token) -> Result<&[GpuMatch], Error> {
        if self.pending.is_none_or(|p| p.0 != token) {
            return Err(Error::WrongToken);
        }
        Ok(&self.back.matches)
    }
    pub fn candidate_poses(&self, token: Token) -> Result<&[P], Error> {
        self.matches(token)?;
        Ok(&self.back.poses)
    }
    pub fn work(&self) -> Work {
        self.work
    }
    pub fn stage(
        &mut self,
        token: Token,
        frame: Frame,
        objects: &[ObjectPose<P>],
    ) -> Result<(), Error> {
        if self.pending.is_some() {
            return Err(Error::Busy);
        }
        if token.serial <= self.last_serial
            || !frame.valid()
            || self
                .presented
                .is_some_and(|(old, _)| token.frame <= old.frame || token.serial <= old.serial)
        {
            return Err(Error::InvalidFrame);
        }
        if objects.len() > self.maximum {
            return Err(Error::Capacity);
        }
        self.back.clear();
        self.work = Work::default();
        for (i, object) in objects.iter().enumerate() {
            self.work.records += 1;
            if !object.pose.valid() {
                self.back.clear();
                return Err(Error::InvalidPose);
            }
            self.back.rows.push(Row {
                key: object.key,
                topology: object.topology,
                model_key: object.pose.model_key(),
                visible: object.visible && object.pose.primary_visible(),
                surface: [0; 4],
            });
            self.back.poses.push(object.pose);
            self.back.sorted.push(i as u32);
        }
        let rows = &self.back.rows;
        let comparisons = &mut self.work.identity_comparisons;
        self.back.sorted.sort_unstable_by(|a, b| {
            *comparisons += 1;
            rows[*a as usize].key.cmp(&rows[*b as usize].key)
        });
        if self
            .back
            .sorted
            .windows(2)
            .any(|p| rows[p[0] as usize].key == rows[p[1] as usize].key)
        {
            self.back.clear();
            return Err(Error::Duplicate);
        }
        let mut new_surfaces = 0;
        for object in objects {
            let old = self
                .front
                .find(object.key, &mut self.work.identity_comparisons);
            if old.is_none_or(|i| {
                self.front.rows[i].topology != object.topology
                    || self.front.rows[i].model_key != object.pose.model_key()
            }) {
                new_surfaces += 1;
            }
            self.back.matches.push(GpuMatch {
                previous_index: old.map_or(INVALID_INDEX, |i| i as u32),
                ..Default::default()
            });
        }
        let mut serial = if new_surfaces == 0 {
            0
        } else {
            NEXT_SURFACE
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                    next.checked_add(new_surfaces)
                        .filter(|end| *end <= u64::from(u32::MAX) + 1)
                })
                .map_err(|_| Error::Exhausted)?
        };
        let compatible = self.presented.is_some_and(|(_, old)| frame.compatible(old));
        for (i, object) in objects.iter().enumerate() {
            let meta = &mut self.back.matches[i];
            let old_index =
                (meta.previous_index != INVALID_INDEX).then_some(meta.previous_index as usize);
            let old = old_index.map(|i| self.front.rows[i]);
            let surface = if let Some(old) = old.filter(|old| {
                old.topology == object.topology && old.model_key == object.pose.model_key()
            }) {
                old.surface
            } else {
                let uid = if object.key.mapped {
                    old.map_or(serial, |old| {
                        u64::from(old.surface[1]) | (u64::from(old.surface[2]) << 32)
                    })
                } else {
                    object.key.identity[0]
                };
                let surface = [
                    object.key.namespace(),
                    uid as u32,
                    (uid >> 32) as u32,
                    serial as u32,
                ];
                serial += 1;
                surface
            };
            self.back.rows[i].surface = surface;
            meta.surface = surface;
            let valid = compatible
                && !object.discontinuity
                && object.visible
                && object.pose.primary_visible()
                && old.is_some_and(|row| {
                    row.visible
                        && row.topology == object.topology
                        && row.model_key == object.pose.model_key()
                })
                && old_index.is_some_and(|index| {
                    let previous = self.front.poses[index].position();
                    let current = object.pose.position();
                    let old_origin = self.presented.expect("compatible presented frame").1.origin;
                    (0..3)
                        .map(|axis| {
                            let delta = (frame.origin[axis] as f64 + f64::from(current[axis]))
                                - (old_origin[axis] as f64 + f64::from(previous[axis]));
                            delta * delta
                        })
                        .sum::<f64>()
                        <= self.teleport_squared
                });
            meta.valid = u32::from(valid);
            if !valid {
                meta.previous_index = INVALID_INDEX;
                self.work.invalid += 1;
            } else {
                self.work.matched += 1;
            }
        }
        self.work.new_surfaces = new_surfaces;
        self.pending = Some((token, frame));
        self.last_serial = token.serial;
        Ok(())
    }
    /// Preflight all presentation owners before committing any of their banks.
    pub fn validate_commit(&self, token: Token) -> Result<(), Error> {
        self.pending
            .filter(|p| p.0 == token)
            .map(|_| ())
            .ok_or(Error::WrongToken)
    }
    pub fn commit_presented(&mut self, token: Token) -> Result<(), Error> {
        self.validate_commit(token)?;
        let pending = self.pending.expect("validated presentation token");
        std::mem::swap(&mut self.front, &mut self.back);
        self.presented = Some(pending);
        self.pending = None;
        Ok(())
    }
    pub fn discard(&mut self, token: Token) -> Result<(), Error> {
        self.matches(token)?;
        self.pending = None;
        self.back.clear();
        Ok(())
    }
}
