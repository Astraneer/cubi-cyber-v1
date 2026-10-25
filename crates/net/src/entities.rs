//! Bounded public entity poses. Private AI, health, inventories and RNG state
//! have no representation in this protocol. Positions retain f64 precision.
use core_types::{SpaceId, Tick};
use std::collections::BTreeSet;

pub const MAX_ENTITIES: usize = 2048;
pub const POSE_BYTES: usize = 80;
pub const MAX_FRAME_BYTES: usize = 36 + MAX_ENTITIES * POSE_BYTES + MAX_ENTITIES * 8;
pub const PUBLIC_FLAGS: u16 = 0x2ff;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EntityPose {
    pub id: u64,
    pub space: SpaceId,
    pub position: [f64; 3],
    pub body_yaw: f32,
    pub head_yaw: f32,
    pub pitch: f32,
    pub limb_phase: f32,
    pub limb_amount: f32,
    pub death_roll: f32,
    pub scale: f32,
    pub extra: f32,
    pub kind: u8,
    pub flags: u16,
    /// Wool color for sheep; wing-phase bits for chickens; ItemId for drops.
    pub color: u32,
    pub count: u16,
    pub damage: u16,
}
impl EntityPose {
    pub fn validate(&self) -> Result<(), EntityCodecError> {
        if self
            .position
            .iter()
            .any(|v| !v.is_finite() || v.abs() >= 2_000_000_000.0)
            || [
                self.body_yaw,
                self.head_yaw,
                self.pitch,
                self.limb_phase,
                self.limb_amount,
                self.death_roll,
                self.scale,
                self.extra,
            ]
            .iter()
            .any(|v| !v.is_finite())
            || self.kind > 7
            || self.flags & !PUBLIC_FLAGS != 0
            || !(0.0..=2.0).contains(&self.limb_amount)
            || !(0.0..=90.0).contains(&self.death_roll)
            || !(0.01..=4.0).contains(&self.scale)
            || (self.kind == 2 && self.color > 5)
            || (self.kind == 3
                && (!f32::from_bits(self.color).is_finite() || !(0.0..=1.0).contains(&self.extra)))
            || (self.kind < 6 && (self.count != 0 || self.damage != 0 || self.flags & 32 != 0))
            || (self.kind >= 6
                && (self.color == 0
                    || self.color > u32::from(u16::MAX)
                    || self.count == 0
                    || self.count > 64
                    || self.flags & 32 == 0))
        {
            return Err(EntityCodecError::Invalid);
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct EntityFrame {
    pub revision: u64,
    /// None is a full image; Some is a delta against that confirmed revision.
    pub base: Option<u64>,
    pub tick: Tick,
    pub upserts: Vec<EntityPose>,
    pub removes: Vec<u64>,
}
impl EntityFrame {
    pub fn validate(&self) -> Result<(), EntityCodecError> {
        if self.revision == 0
            || self
                .base
                .is_some_and(|base| base == 0 || base >= self.revision)
            || self.upserts.len() > MAX_ENTITIES
            || self.removes.len() > MAX_ENTITIES
            || (self.base.is_none() && !self.removes.is_empty())
            || self.upserts.windows(2).any(|p| p[0].id >= p[1].id)
            || self.removes.windows(2).any(|p| p[0] >= p[1])
        {
            return Err(EntityCodecError::Invalid);
        }
        for pose in &self.upserts {
            pose.validate()?;
            if self.removes.binary_search(&pose.id).is_ok() {
                return Err(EntityCodecError::Invalid);
            }
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntityRequest {
    Ack { revision: u64 },
    Full { have: Option<u64> },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntityCodecError {
    Bounds,
    Invalid,
    Truncated,
}

pub fn encode_frame(frame: &EntityFrame) -> Result<Vec<u8>, EntityCodecError> {
    frame.validate()?;
    let mut out =
        Vec::with_capacity(36 + POSE_BYTES * frame.upserts.len() + 8 * frame.removes.len());
    out.extend_from_slice(b"ENF1");
    for value in [frame.revision, frame.base.unwrap_or(0), frame.tick.0] {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out.extend_from_slice(&(frame.upserts.len() as u32).to_le_bytes());
    out.extend_from_slice(&(frame.removes.len() as u32).to_le_bytes());
    for p in &frame.upserts {
        out.extend_from_slice(&p.id.to_le_bytes());
        out.extend_from_slice(&p.space.0.to_le_bytes());
        for value in p.position {
            out.extend_from_slice(&value.to_bits().to_le_bytes());
        }
        for value in [
            p.body_yaw,
            p.head_yaw,
            p.pitch,
            p.limb_phase,
            p.limb_amount,
            p.death_roll,
            p.scale,
            p.extra,
        ] {
            out.extend_from_slice(&value.to_bits().to_le_bytes());
        }
        out.push(p.kind);
        out.extend_from_slice(&p.flags.to_le_bytes());
        out.push(0);
        out.extend_from_slice(&p.color.to_le_bytes());
        out.extend_from_slice(&p.count.to_le_bytes());
        out.extend_from_slice(&p.damage.to_le_bytes());
    }
    for id in &frame.removes {
        out.extend_from_slice(&id.to_le_bytes());
    }
    Ok(out)
}
struct Reader<'a> {
    bytes: &'a [u8],
    cursor: usize,
}
impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], EntityCodecError> {
        let end = self.cursor.checked_add(N).ok_or(EntityCodecError::Bounds)?;
        let value = self
            .bytes
            .get(self.cursor..end)
            .ok_or(EntityCodecError::Truncated)?
            .try_into()
            .map_err(|_| EntityCodecError::Truncated)?;
        self.cursor = end;
        Ok(value)
    }
    fn u16(&mut self) -> Result<u16, EntityCodecError> {
        Ok(u16::from_le_bytes(self.take()?))
    }
    fn u32(&mut self) -> Result<u32, EntityCodecError> {
        Ok(u32::from_le_bytes(self.take()?))
    }
    fn u64(&mut self) -> Result<u64, EntityCodecError> {
        Ok(u64::from_le_bytes(self.take()?))
    }
    fn f32(&mut self) -> Result<f32, EntityCodecError> {
        Ok(f32::from_bits(self.u32()?))
    }
}
pub fn decode_frame(bytes: &[u8]) -> Result<EntityFrame, EntityCodecError> {
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(EntityCodecError::Bounds);
    }
    let mut r = Reader { bytes, cursor: 0 };
    if r.take::<4>()? != *b"ENF1" {
        return Err(EntityCodecError::Invalid);
    }
    let revision = r.u64()?;
    let base = match r.u64()? {
        0 => None,
        value => Some(value),
    };
    let tick = Tick(r.u64()?);
    let count = r.u32()? as usize;
    let removed = r.u32()? as usize;
    if count > MAX_ENTITIES
        || removed > MAX_ENTITIES
        || bytes.len() != 36 + count * POSE_BYTES + removed * 8
    {
        return Err(EntityCodecError::Bounds);
    }
    let mut upserts = Vec::with_capacity(count);
    for _ in 0..count {
        let id = r.u64()?;
        let space = SpaceId(r.u32()?);
        let mut position = [0.0; 3];
        for value in &mut position {
            *value = f64::from_bits(r.u64()?);
        }
        let mut angles = [0.0; 8];
        for value in &mut angles {
            *value = r.f32()?;
        }
        let [kind] = r.take()?;
        let flags = r.u16()?;
        if r.take::<1>()? != [0] {
            return Err(EntityCodecError::Invalid);
        }
        upserts.push(EntityPose {
            id,
            space,
            position,
            body_yaw: angles[0],
            head_yaw: angles[1],
            pitch: angles[2],
            limb_phase: angles[3],
            limb_amount: angles[4],
            death_roll: angles[5],
            scale: angles[6],
            extra: angles[7],
            kind,
            flags,
            color: r.u32()?,
            count: r.u16()?,
            damage: r.u16()?,
        });
    }
    let mut removes = Vec::with_capacity(removed);
    for _ in 0..removed {
        removes.push(r.u64()?);
    }
    let frame = EntityFrame {
        revision,
        base,
        tick,
        upserts,
        removes,
    };
    frame.validate()?;
    Ok(frame)
}
pub fn encode_request(request: EntityRequest) -> Result<Vec<u8>, EntityCodecError> {
    let (kind, value) = match request {
        EntityRequest::Ack { revision: 0 } | EntityRequest::Full { have: Some(0) } => {
            return Err(EntityCodecError::Invalid);
        }
        EntityRequest::Ack { revision } => (0, revision),
        EntityRequest::Full { have } => (1, have.unwrap_or(0)),
    };
    let mut out = Vec::with_capacity(13);
    out.extend_from_slice(b"ENQ1");
    out.push(kind);
    out.extend_from_slice(&value.to_le_bytes());
    Ok(out)
}
pub fn decode_request(bytes: &[u8]) -> Result<EntityRequest, EntityCodecError> {
    if bytes.len() != 13 || &bytes[..4] != b"ENQ1" {
        return Err(EntityCodecError::Invalid);
    }
    let value = u64::from_le_bytes(
        bytes[5..]
            .try_into()
            .map_err(|_| EntityCodecError::Truncated)?,
    );
    match bytes[4] {
        0 if value > 0 => Ok(EntityRequest::Ack { revision: value }),
        1 => Ok(EntityRequest::Full {
            have: (value > 0).then_some(value),
        }),
        _ => Err(EntityCodecError::Invalid),
    }
}

/// Complete lifecycle validation also used by the client before publication.
pub fn resulting_ids(
    frame: &EntityFrame,
    existing: impl IntoIterator<Item = u64>,
    maximum: usize,
) -> Result<BTreeSet<u64>, EntityCodecError> {
    frame.validate()?;
    let mut ids = if frame.base.is_none() {
        BTreeSet::new()
    } else {
        existing.into_iter().collect()
    };
    for id in &frame.removes {
        if !ids.remove(id) {
            return Err(EntityCodecError::Invalid);
        }
    }
    ids.extend(frame.upserts.iter().map(|pose| pose.id));
    if ids.len() > maximum {
        return Err(EntityCodecError::Bounds);
    }
    Ok(ids)
}
