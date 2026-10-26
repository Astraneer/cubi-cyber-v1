//! Public height samples and explicit replacement evidence. No cells or seed.
use core_types::{PageId, SpaceId, Version};
pub const MAX_LEVEL: u8 = 10;
pub const MAX_VERTICAL_PAGES: usize = 32;
pub const MAX_HEIGHT_BYTES: usize = 4096;
pub const ATLAS_LAYERS: u16 = 186;
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct HeightKey {
    pub space: u32,
    pub level: u8,
    pub x: i32,
    pub z: i32,
}
impl HeightKey {
    pub fn validate(self) -> Result<(), HeightError> {
        if self.level > MAX_LEVEL {
            return Err(HeightError::Bounds);
        }
        let span = 16_i64 << self.level;
        for coordinate in [self.x, self.z] {
            let start = i64::from(coordinate) * span;
            if start < i64::from(i32::MIN) || start + span > i64::from(i32::MAX) + 1 {
                return Err(HeightError::Bounds);
            }
        }
        Ok(())
    }
    pub fn children(self) -> Result<[Self; 4], HeightError> {
        self.validate()?;
        if self.level == 0 {
            return Err(HeightError::Bounds);
        }
        let child = |dx, dz| Self {
            level: self.level - 1,
            x: self.x * 2 + dx,
            z: self.z * 2 + dz,
            ..self
        };
        Ok([child(0, 0), child(1, 0), child(0, 1), child(1, 1)])
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GeometryStamp {
    pub page: PageId,
    pub source: Version,
    pub view: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Replacement {
    None,
    /// The four exact children must replace this coarse tile as a complete cover.
    Refine,
    /// Empty geometry means unavailable. A nonempty manifest certifies the whole
    /// vertical leaf, and every cached proxy must match both source and view.
    Column {
        min_y: i32,
        max_y: i32,
        geometry: Vec<GeometryStamp>,
    },
}
/// Authority-certified vertical displacement from the canonical voxel surface
/// to this tile's two triangles. Quantization rounds UP, in 1/1024 blocks.
/// It is bound to the same `source` as heights and replacement manifests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeightErrorBound {
    pub max_error_q1024: u64,
    pub min_height: i32,
    pub max_height: i32,
}
impl HeightErrorBound {
    pub fn blocks(self) -> f64 {
        self.max_error_q1024 as f64 / 1024.
    }
    pub fn validate(self) -> Result<(), HeightError> {
        if self.min_height > self.max_height
            || self.max_error_q1024 > (u64::from(u32::MAX) + 1) * 1024
        {
            return Err(HeightError::Bounds);
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeightTile {
    pub key: HeightKey,
    pub source: u64,
    /// None is Unknown error, never zero error inferred from sparse samples.
    pub error: Option<HeightErrorBound>,
    pub heights: [i32; 289],
    pub known: [u8; 37],
    pub atlas: [u16; 256],
    pub tint: [u32; 256],
    pub protected: [u8; 32],
    pub replacement: Replacement,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeightError {
    Bounds,
    Malformed,
    Truncated,
    Trailing,
}
impl HeightTile {
    pub fn validate(&self) -> Result<(), HeightError> {
        self.key.validate()?;
        if let Some(error) = self.error {
            error.validate()?;
            if self.heights.iter().enumerate().any(|(i, &height)| {
                self.known[i / 8] & (1 << (i % 8)) != 0
                    && !(error.min_height..=error.max_height).contains(&height)
            }) {
                return Err(HeightError::Bounds);
            }
        }
        if self.source == 0
            || self.known[36] & !1 != 0
            || self.atlas.iter().any(|&a| a >= ATLAS_LAYERS)
        {
            return Err(HeightError::Bounds);
        }
        for (i, &height) in self.heights.iter().enumerate() {
            if self.known[i / 8] & (1 << (i % 8)) == 0 && height != 0 {
                return Err(HeightError::Malformed);
            }
        }
        let protected = self.protected.iter().any(|&b| b != 0);
        match &self.replacement {
            Replacement::None if !protected => {}
            Replacement::Refine if protected && self.key.level > 0 => {}
            Replacement::Column {
                min_y,
                max_y,
                geometry,
            } if protected && self.key.level == 0 => {
                if self.protected != [255; 32]
                    || *min_y != 0
                    || *max_y < 7
                    || max_y < min_y
                    || i64::from(*max_y) - i64::from(*min_y) >= MAX_VERTICAL_PAGES as i64
                {
                    return Err(HeightError::Bounds);
                }
                if !geometry.is_empty() && geometry.len() != (*max_y - *min_y + 1) as usize {
                    return Err(HeightError::Malformed);
                }
                for (i, stamp) in geometry.iter().enumerate() {
                    if stamp.page
                        != (PageId {
                            space: SpaceId(self.key.space),
                            x: self.key.x,
                            y: *min_y + i as i32,
                            z: self.key.z,
                        })
                        || stamp.source.0 == 0
                        || stamp.view == 0
                    {
                        return Err(HeightError::Malformed);
                    }
                }
            }
            _ => return Err(HeightError::Malformed),
        }
        Ok(())
    }
}
pub fn encode(tile: &HeightTile) -> Result<Vec<u8>, HeightError> {
    tile.validate()?;
    let mut out = Vec::with_capacity(MAX_HEIGHT_BYTES);
    out.extend_from_slice(b"HTI2");
    out.extend_from_slice(&tile.key.space.to_le_bytes());
    out.push(tile.key.level);
    out.extend_from_slice(&tile.key.x.to_le_bytes());
    out.extend_from_slice(&tile.key.z.to_le_bytes());
    out.extend_from_slice(&tile.source.to_le_bytes());
    match tile.error {
        None => out.push(0),
        Some(error) => {
            out.push(1);
            out.extend_from_slice(&error.max_error_q1024.to_le_bytes());
            out.extend_from_slice(&error.min_height.to_le_bytes());
            out.extend_from_slice(&error.max_height.to_le_bytes());
        }
    }
    for v in tile.heights {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&tile.known);
    for v in tile.atlas {
        out.extend_from_slice(&v.to_le_bytes());
    }
    for v in tile.tint {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&tile.protected);
    match &tile.replacement {
        Replacement::None => out.push(0),
        Replacement::Refine => out.push(1),
        Replacement::Column {
            min_y,
            max_y,
            geometry,
        } => {
            out.push(2);
            out.extend_from_slice(&min_y.to_le_bytes());
            out.extend_from_slice(&max_y.to_le_bytes());
            out.push(geometry.len() as u8);
            for stamp in geometry {
                out.extend_from_slice(&stamp.page.space.0.to_le_bytes());
                for v in [stamp.page.x, stamp.page.y, stamp.page.z] {
                    out.extend_from_slice(&v.to_le_bytes());
                }
                out.extend_from_slice(&stamp.source.0.to_le_bytes());
                out.extend_from_slice(&stamp.view.to_le_bytes());
            }
        }
    }
    if out.len() > MAX_HEIGHT_BYTES {
        return Err(HeightError::Bounds);
    }
    Ok(out)
}
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], HeightError> {
        let end = self.at.checked_add(N).ok_or(HeightError::Bounds)?;
        let bytes = self.bytes.get(self.at..end).ok_or(HeightError::Truncated)?;
        self.at = end;
        Ok(bytes.try_into().expect("fixed slice"))
    }
    fn u8(&mut self) -> Result<u8, HeightError> {
        Ok(self.take::<1>()?[0])
    }
    fn u32(&mut self) -> Result<u32, HeightError> {
        Ok(u32::from_le_bytes(self.take()?))
    }
    fn i32(&mut self) -> Result<i32, HeightError> {
        Ok(i32::from_le_bytes(self.take()?))
    }
    fn u64(&mut self) -> Result<u64, HeightError> {
        Ok(u64::from_le_bytes(self.take()?))
    }
}
pub fn decode(bytes: &[u8]) -> Result<HeightTile, HeightError> {
    if bytes.len() > MAX_HEIGHT_BYTES {
        return Err(HeightError::Bounds);
    }
    let mut r = Reader { bytes, at: 0 };
    if r.take::<4>()? != *b"HTI2" {
        return Err(HeightError::Malformed);
    }
    let key = HeightKey {
        space: r.u32()?,
        level: r.u8()?,
        x: r.i32()?,
        z: r.i32()?,
    };
    key.validate()?;
    let source = r.u64()?;
    let error = match r.u8()? {
        0 => None,
        1 => Some(HeightErrorBound {
            max_error_q1024: r.u64()?,
            min_height: r.i32()?,
            max_height: r.i32()?,
        }),
        _ => return Err(HeightError::Malformed),
    };
    let mut heights = [0; 289];
    for v in &mut heights {
        *v = r.i32()?;
    }
    let known = r.take()?;
    let mut atlas = [0; 256];
    for v in &mut atlas {
        *v = u16::from_le_bytes(r.take()?);
    }
    let mut tint = [0; 256];
    for v in &mut tint {
        *v = r.u32()?;
    }
    let protected = r.take()?;
    let replacement = match r.u8()? {
        0 => Replacement::None,
        1 => Replacement::Refine,
        2 => {
            let min_y = r.i32()?;
            let max_y = r.i32()?;
            let n = usize::from(r.u8()?);
            if n > MAX_VERTICAL_PAGES {
                return Err(HeightError::Bounds);
            }
            let mut geometry = Vec::with_capacity(n);
            for _ in 0..n {
                geometry.push(GeometryStamp {
                    page: PageId {
                        space: SpaceId(r.u32()?),
                        x: r.i32()?,
                        y: r.i32()?,
                        z: r.i32()?,
                    },
                    source: Version(r.u64()?),
                    view: r.u64()?,
                });
            }
            Replacement::Column {
                min_y,
                max_y,
                geometry,
            }
        }
        _ => return Err(HeightError::Malformed),
    };
    if r.at != bytes.len() {
        return Err(HeightError::Trailing);
    }
    let tile = HeightTile {
        key,
        source,
        error,
        heights,
        known,
        atlas,
        tint,
        protected,
        replacement,
    };
    tile.validate()?;
    Ok(tile)
}
