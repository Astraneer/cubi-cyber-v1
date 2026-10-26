//! Public pipe-item motion. No payload references, inventories, machine energy,
//! route beyond the current segment, or private component bytes can be encoded.
use core_types::{LocalIdx, PageId, SpaceId, Tick, Version};
use std::collections::BTreeSet;
pub const MAX_PACKETS: usize = 512;
pub const HEADER_BYTES: usize = 56;
pub const ROW_BYTES: usize = 28;
pub const MAX_BYTES: usize = HEADER_BYTES + MAX_PACKETS * (ROW_BYTES + 8);
pub const ACTIVE: u8 = 1;
pub const NO_EXIT: u8 = 6;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pose {
    pub id: u64,
    pub index: LocalIdx,
    pub item: u16,
    pub count: u16,
    pub color: u8,
    /// Direction of travel in Face6 order, not the face of the entering port.
    pub entry: u8,
    pub exit: u8,
    pub progress: u8,
    pub flags: u8,
    pub epoch: Tick,
}
impl Pose {
    pub fn validate(self) -> Result<(), Error> {
        if self.id == 0
            || self.item == 0
            || self.count == 0
            || self.count > 64
            || self.color > 18
            || self.entry > 5
            || self.exit > NO_EXIT
            || self.progress > 99
            || self.flags & !ACTIVE != 0
        {
            return Err(Error::Pose);
        }
        Ok(())
    }
    pub fn progress_at(self, tick: Tick, alpha: f64) -> f64 {
        let alpha = if alpha.is_finite() {
            alpha.clamp(0., 1.)
        } else {
            0.
        };
        let elapsed = if self.flags & ACTIVE != 0 {
            (tick.0.saturating_sub(self.epoch.0)) as f64 + alpha
        } else {
            0.
        };
        (f64::from(self.progress) + 5. * elapsed).min(99.)
    }
    pub fn same_motion(self, other: Self, tick: Tick) -> bool {
        self.id == other.id
            && self.index == other.index
            && self.item == other.item
            && self.count == other.count
            && self.color == other.color
            && self.entry == other.entry
            && self.exit == other.exit
            && self.flags == other.flags
            && self.progress_at(tick, 0.) == other.progress_at(tick, 0.)
    }
    /// Integer origin subtraction precedes f32 conversion at the render bridge.
    /// Extrapolation stops inside this cell; lifecycle/turns come from the owner.
    pub fn relative_position(
        self,
        page: PageId,
        tick: Tick,
        alpha: f64,
        origin: [i64; 3],
    ) -> [f64; 3] {
        let (x, y, z) = self.index.xyz();
        let progress = self.progress_at(tick, alpha);
        let face = if progress < 50. || self.exit == NO_EXIT {
            self.entry
        } else {
            self.exit
        };
        let direction = match face {
            0 => [0., -1., 0.],
            1 => [0., 1., 0.],
            2 => [0., 0., -1.],
            3 => [0., 0., 1.],
            4 => [-1., 0., 0.],
            _ => [1., 0., 0.],
        };
        let cells = [
            i64::from(page.x) * 16 + i64::from(x),
            i64::from(page.y) * 16 + i64::from(y),
            i64::from(page.z) * 16 + i64::from(z),
        ];
        let center = [0.5, 0.25, 0.5];
        std::array::from_fn(|i| {
            (i128::from(cells[i]) - i128::from(origin[i])) as f64
                + center[i]
                + direction[i] * (progress / 100. - 0.5)
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub page: PageId,
    pub view_version: Version,
    pub revision: u64,
    pub base: Option<u64>,
    pub tick: Tick,
    pub upserts: Vec<Pose>,
    pub removes: Vec<u64>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Bounds,
    Encoding,
    Pose,
    Revision,
    Lifecycle,
}
fn valid_page(page: PageId) -> bool {
    [page.x, page.y, page.z]
        .iter()
        .all(|&v| i64::from(v).abs() <= i64::from(crate::MAX_COORDINATE) / 16)
}
impl Frame {
    pub fn validate(&self) -> Result<(), Error> {
        if !valid_page(self.page)
            || self.upserts.len() > MAX_PACKETS
            || self.removes.len() > MAX_PACKETS
        {
            return Err(Error::Bounds);
        }
        if self.revision == 0 || self.base.is_some_and(|b| b == 0 || b >= self.revision) {
            return Err(Error::Revision);
        }
        if self.base.is_none() && !self.removes.is_empty()
            || self.upserts.windows(2).any(|p| p[0].id >= p[1].id)
            || self.removes.windows(2).any(|p| p[0] >= p[1])
            || self.removes.contains(&0)
        {
            return Err(Error::Lifecycle);
        }
        for pose in &self.upserts {
            pose.validate()?;
            if pose.epoch > self.tick || self.removes.binary_search(&pose.id).is_ok() {
                return Err(Error::Lifecycle);
            }
        }
        Ok(())
    }
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        self.validate()?;
        let mut bytes = Vec::with_capacity(
            HEADER_BYTES + self.upserts.len() * ROW_BYTES + self.removes.len() * 8,
        );
        bytes.extend_from_slice(b"MPK1");
        put_page(&mut bytes, self.page);
        put64(&mut bytes, self.view_version.0);
        put64(&mut bytes, self.revision);
        put64(&mut bytes, self.base.unwrap_or(0));
        put64(&mut bytes, self.tick.0);
        put16(&mut bytes, self.upserts.len() as u16);
        put16(&mut bytes, self.removes.len() as u16);
        for p in &self.upserts {
            put64(&mut bytes, p.id);
            put16(&mut bytes, p.index.get());
            put16(&mut bytes, p.item);
            put16(&mut bytes, p.count);
            bytes.extend_from_slice(&[p.color, p.entry, p.exit, p.progress, p.flags, 0]);
            put64(&mut bytes, p.epoch.0);
        }
        for &id in &self.removes {
            put64(&mut bytes, id);
        }
        Ok(bytes)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() < HEADER_BYTES || bytes.len() > MAX_BYTES {
            return Err(Error::Bounds);
        }
        let mut r = Reader { bytes, at: 0 };
        if r.take::<4>()? != *b"MPK1" {
            return Err(Error::Encoding);
        }
        let page = r.page()?;
        let view_version = Version(r.u64()?);
        let revision = r.u64()?;
        let base = match r.u64()? {
            0 => None,
            n => Some(n),
        };
        let tick = Tick(r.u64()?);
        let ups = r.u16()? as usize;
        let rms = r.u16()? as usize;
        if ups > MAX_PACKETS
            || rms > MAX_PACKETS
            || bytes.len() != HEADER_BYTES + ups * ROW_BYTES + rms * 8
        {
            return Err(Error::Bounds);
        }
        let mut upserts = Vec::with_capacity(ups);
        for _ in 0..ups {
            let id = r.u64()?;
            let index = LocalIdx::new(r.u16()?).ok_or(Error::Pose)?;
            let item = r.u16()?;
            let count = r.u16()?;
            let fields = r.take::<6>()?;
            if fields[5] != 0 {
                return Err(Error::Encoding);
            }
            upserts.push(Pose {
                id,
                index,
                item,
                count,
                color: fields[0],
                entry: fields[1],
                exit: fields[2],
                progress: fields[3],
                flags: fields[4],
                epoch: Tick(r.u64()?),
            });
        }
        let mut removes = Vec::with_capacity(rms);
        for _ in 0..rms {
            removes.push(r.u64()?);
        }
        let frame = Self {
            page,
            view_version,
            revision,
            base,
            tick,
            upserts,
            removes,
        };
        frame.validate()?;
        Ok(frame)
    }
}
pub fn resulting_ids(
    frame: &Frame,
    current: impl IntoIterator<Item = u64>,
    maximum: usize,
) -> Result<BTreeSet<u64>, Error> {
    frame.validate()?;
    if maximum == 0 || maximum > MAX_PACKETS {
        return Err(Error::Bounds);
    }
    let mut ids = if frame.base.is_none() {
        BTreeSet::new()
    } else {
        current.into_iter().collect()
    };
    for id in &frame.removes {
        if !ids.remove(id) {
            return Err(Error::Lifecycle);
        }
    }
    for p in &frame.upserts {
        ids.insert(p.id);
    }
    if ids.len() > maximum {
        return Err(Error::Bounds);
    }
    Ok(ids)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    Ack {
        page: PageId,
        view_version: Version,
        revision: u64,
    },
    Full {
        page: PageId,
        view_version: Version,
        have: Option<u64>,
    },
}
impl Request {
    pub fn page(self) -> PageId {
        match self {
            Self::Ack { page, .. } | Self::Full { page, .. } => page,
        }
    }
    pub fn view_version(self) -> Version {
        match self {
            Self::Ack { view_version, .. } | Self::Full { view_version, .. } => view_version,
        }
    }
    pub fn encode(self) -> Result<[u8; 37], Error> {
        let (tag, page, version, revision) = match self {
            Self::Ack {
                page,
                view_version,
                revision,
            } if revision != 0 => (0, page, view_version, revision),
            Self::Full {
                page,
                view_version,
                have,
            } if have != Some(0) => (1, page, view_version, have.unwrap_or(0)),
            _ => return Err(Error::Revision),
        };
        if !valid_page(page) {
            return Err(Error::Bounds);
        }
        let mut bytes = Vec::with_capacity(37);
        bytes.extend_from_slice(b"MPQ1");
        bytes.push(tag);
        put_page(&mut bytes, page);
        put64(&mut bytes, version.0);
        put64(&mut bytes, revision);
        Ok(bytes.try_into().expect("fixed request"))
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() != 37 {
            return Err(Error::Bounds);
        }
        let mut r = Reader { bytes, at: 0 };
        if r.take::<4>()? != *b"MPQ1" {
            return Err(Error::Encoding);
        }
        let tag = r.take::<1>()?[0];
        let page = r.page()?;
        let view_version = Version(r.u64()?);
        let revision = r.u64()?;
        let request = match tag {
            0 => Self::Ack {
                page,
                view_version,
                revision,
            },
            1 => Self::Full {
                page,
                view_version,
                have: if revision == 0 { None } else { Some(revision) },
            },
            _ => return Err(Error::Encoding),
        };
        request.encode()?;
        Ok(request)
    }
}
fn put16(b: &mut Vec<u8>, n: u16) {
    b.extend_from_slice(&n.to_le_bytes());
}
fn put64(b: &mut Vec<u8>, n: u64) {
    b.extend_from_slice(&n.to_le_bytes());
}
fn put_page(b: &mut Vec<u8>, p: PageId) {
    for n in [p.space.0, p.x as u32, p.y as u32, p.z as u32] {
        b.extend_from_slice(&n.to_le_bytes());
    }
}
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        let end = self.at.checked_add(N).ok_or(Error::Bounds)?;
        let bytes = self.bytes.get(self.at..end).ok_or(Error::Encoding)?;
        self.at = end;
        bytes.try_into().map_err(|_| Error::Encoding)
    }
    fn u16(&mut self) -> Result<u16, Error> {
        Ok(u16::from_le_bytes(self.take()?))
    }
    fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(self.take()?))
    }
    fn page(&mut self) -> Result<PageId, Error> {
        let mut word = || self.take().map(u32::from_le_bytes);
        Ok(PageId {
            space: SpaceId(word()?),
            x: word()? as i32,
            y: word()? as i32,
            z: word()? as i32,
        })
    }
}
