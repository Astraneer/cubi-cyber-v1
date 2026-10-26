//! Minimal public block visual state, bound to one confirmed public cell view.
//! No inventories, item/tank/energy counts, payload refs or component bytes exist.
use core_types::{LocalIdx, PageId, SpaceId, Tick, Version};
pub const MAX_HINTS: usize = 4096;
pub const HEADER_BYTES: usize = 54;
pub const HINT_BYTES: usize = 14;
pub const MAX_BYTES: usize = HEADER_BYTES + MAX_HINTS * HINT_BYTES;
pub const ACTIVE: u8 = 1;
pub const PORTS: u8 = 2;
pub const LID: u8 = 4;
pub const ROTOR: u8 = 8;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    None = 0,
    Chest = 1,
    Machine = 2,
    Wind = 3,
    Connector = 4,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hint {
    pub index: LocalIdx,
    pub kind: Kind,
    pub flags: u8,
    /// Machine/Wind: six3-bit modes0..5. Connector: six2-bit modes0..3.
    /// Face order is Down Up North South West East, without inventory fields.
    pub ports: u32,
    pub lid: u16,
    /// Turns, UNORM16, at Frame.tick. No per-frame server traffic is needed.
    pub rotor: u16,
    /// Signed1/65536 turn per20Hz server tick (radians/tick = value*TAU/65536).
    pub rotor_velocity: i16,
}
impl Hint {
    pub fn validate(&self) -> Result<(), Error> {
        let allowed = match self.kind {
            Kind::None => 0,
            Kind::Chest => LID,
            Kind::Machine | Kind::Connector => ACTIVE | PORTS,
            Kind::Wind => ACTIVE | PORTS | ROTOR,
        };
        if self.flags & !allowed != 0
            || (matches!(self.kind, Kind::Machine | Kind::Wind)
                && (self.ports > 0x3ffff || (0..6).any(|face| (self.ports >> (face * 3) & 7) > 5)))
            || (self.kind == Kind::Connector && self.ports > 4095)
            || (self.flags & PORTS == 0 && self.ports != 0)
            || (self.flags & LID == 0 && self.lid != 0)
            || (self.flags & ROTOR == 0 && (self.rotor != 0 || self.rotor_velocity != 0))
        {
            return Err(Error::Hint);
        }
        Ok(())
    }
    pub fn from_visual(
        index: LocalIdx,
        kind: Kind,
        ports: Option<u32>,
        active: bool,
        lid: Option<f32>,
        rotor: Option<(f32, f32)>,
    ) -> Result<Self, Error> {
        let mut hint = Self {
            index,
            kind,
            flags: if active { ACTIVE } else { 0 },
            ports: 0,
            lid: 0,
            rotor: 0,
            rotor_velocity: 0,
        };
        if let Some(ports) = ports {
            hint.flags |= PORTS;
            hint.ports = ports;
        }
        if let Some(lid) = lid {
            if !lid.is_finite() || !(0.0..=1.0).contains(&lid) {
                return Err(Error::Hint);
            }
            hint.flags |= LID;
            hint.lid = (lid * 65535.).round() as u16;
        }
        if let Some((phase, velocity)) = rotor {
            if !phase.is_finite()
                || !velocity.is_finite()
                || !(-std::f32::consts::PI..std::f32::consts::PI).contains(&velocity)
            {
                return Err(Error::Hint);
            }
            hint.flags |= ROTOR;
            hint.rotor = (phase.rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU * 65536.)
                .round() as u32 as u16;
            hint.rotor_velocity = (velocity / std::f32::consts::TAU * 65536.)
                .round()
                .clamp(-32768., 32767.) as i16;
        }
        hint.validate()?;
        Ok(hint)
    }
    pub fn lid_amount(self) -> f32 {
        f32::from(self.lid) / 65535.
    }
    pub fn rotor_at(self, epoch: Tick, now: Tick, alpha: f64) -> f32 {
        let elapsed = (i128::from(now.0) - i128::from(epoch.0)) as f64
            + if alpha.is_finite() {
                alpha.clamp(0., 1.)
            } else {
                0.
            };
        ((f64::from(self.rotor) + elapsed * f64::from(self.rotor_velocity)).rem_euclid(65536.)
            * (std::f64::consts::TAU / 65536.)) as f32
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub page: PageId,
    pub cell_view: u64,
    pub view: u64,
    pub source: Version,
    pub tick: Tick,
    pub hints: Vec<Hint>,
}
impl Frame {
    pub fn validate(&self) -> Result<(), Error> {
        if self.cell_view == 0
            || self.view == 0
            || self.source.0 == 0
            || self.hints.len() > MAX_HINTS
            || self.hints.windows(2).any(|h| h[0].index >= h[1].index)
        {
            return Err(Error::Frame);
        }
        for hint in &self.hints {
            hint.validate()?;
        }
        Ok(())
    }
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        self.validate()?;
        let mut out = Vec::with_capacity(HEADER_BYTES + self.hints.len() * HINT_BYTES);
        out.extend_from_slice(b"PRP1");
        page_write(&mut out, self.page);
        for n in [self.cell_view, self.view, self.source.0, self.tick.0] {
            out.extend_from_slice(&n.to_le_bytes());
        }
        out.extend_from_slice(&(self.hints.len() as u16).to_le_bytes());
        for h in &self.hints {
            out.extend_from_slice(&h.index.get().to_le_bytes());
            out.push(h.kind as u8);
            out.push(h.flags);
            out.extend_from_slice(&h.ports.to_le_bytes());
            for n in [h.lid, h.rotor, h.rotor_velocity as u16] {
                out.extend_from_slice(&n.to_le_bytes());
            }
        }
        Ok(out)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() < HEADER_BYTES || bytes.len() > MAX_BYTES || &bytes[..4] != b"PRP1" {
            return Err(Error::Length);
        }
        let mut r = Reader { bytes, at: 4 };
        let page = page_read(&mut r)?;
        let cell_view = r.u64()?;
        let view = r.u64()?;
        let source = Version(r.u64()?);
        let tick = Tick(r.u64()?);
        let n = r.u16()? as usize;
        if n > MAX_HINTS
            || bytes.len() != HEADER_BYTES + n * HINT_BYTES
            || cell_view == 0
            || view == 0
            || source.0 == 0
        {
            return Err(Error::Length);
        }
        let mut hints = Vec::with_capacity(n);
        for _ in 0..n {
            let index = LocalIdx::new(r.u16()?).ok_or(Error::Hint)?;
            let kind = match r.u8()? {
                0 => Kind::None,
                1 => Kind::Chest,
                2 => Kind::Machine,
                3 => Kind::Wind,
                4 => Kind::Connector,
                _ => return Err(Error::Hint),
            };
            let flags = r.u8()?;
            let hint = Hint {
                index,
                kind,
                flags,
                ports: r.u32()?,
                lid: r.u16()?,
                rotor: r.u16()?,
                rotor_velocity: r.u16()? as i16,
            };
            hint.validate()?;
            if hints.last().is_some_and(|h: &Hint| h.index >= hint.index) {
                return Err(Error::Frame);
            }
            hints.push(hint);
        }
        Ok(Self {
            page,
            cell_view,
            view,
            source,
            tick,
            hints,
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request {
    pub page: PageId,
    pub known: Option<u64>,
}
impl Request {
    pub fn encode(self) -> [u8; 28] {
        let mut out = [0; 28];
        out[..4].copy_from_slice(b"PRQ1");
        for (i, n) in [
            self.page.space.0,
            self.page.x as u32,
            self.page.y as u32,
            self.page.z as u32,
        ]
        .into_iter()
        .enumerate()
        {
            out[4 + i * 4..8 + i * 4].copy_from_slice(&n.to_le_bytes());
        }
        out[20..].copy_from_slice(&self.known.unwrap_or(0).to_le_bytes());
        out
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() != 28 || &bytes[..4] != b"PRQ1" {
            return Err(Error::Length);
        }
        let mut r = Reader { bytes, at: 4 };
        let page = page_read(&mut r)?;
        let known = r.u64()?;
        Ok(Self {
            page,
            known: (known != 0).then_some(known),
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Length,
    Hint,
    Frame,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}
fn page_write(out: &mut Vec<u8>, page: PageId) {
    for n in [page.space.0, page.x as u32, page.y as u32, page.z as u32] {
        out.extend_from_slice(&n.to_le_bytes());
    }
}
fn page_read(r: &mut Reader<'_>) -> Result<PageId, Error> {
    Ok(PageId {
        space: SpaceId(r.u32()?),
        x: r.u32()? as i32,
        y: r.u32()? as i32,
        z: r.u32()? as i32,
    })
}
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        let end = self.at.checked_add(N).ok_or(Error::Length)?;
        let value = self
            .bytes
            .get(self.at..end)
            .ok_or(Error::Length)?
            .try_into()
            .map_err(|_| Error::Length)?;
        self.at = end;
        Ok(value)
    }
    fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take::<1>()?[0])
    }
    fn u16(&mut self) -> Result<u16, Error> {
        Ok(u16::from_le_bytes(self.take()?))
    }
    fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(self.take()?))
    }
    fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(self.take()?))
    }
}
