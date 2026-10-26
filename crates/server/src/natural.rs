//! Private, replay-guarded natural population metadata. No field is a wire DTO.
use core_types::SpaceId;
use std::collections::BTreeSet;
pub const MAX_COLUMNS: usize = 65_536;
const MAX_ID: u64 = (1u64 << 62) - 1;
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Column {
    pub space: SpaceId,
    pub x: i32,
    pub z: i32,
}
impl Column {
    fn valid(self) -> bool {
        [self.x, self.z]
            .iter()
            .all(|&v| (i64::from(v) * 16).abs() <= 29_999_936)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Bounds,
    Encoding,
    Stale,
    Capacity,
    Overflow,
}
pub struct State {
    revision: u64,
    map_seed: u64,
    rng: u64,
    next_id: u64,
    columns: BTreeSet<Column>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Delta {
    revision: u64,
    rng_before: u64,
    rng_after: u64,
    next_before: u64,
    next_after: u64,
    column: Option<Column>,
}
impl State {
    pub fn new(map_seed: u64, rng: u64) -> Result<Self, Error> {
        if rng == 0 {
            return Err(Error::Bounds);
        }
        Ok(Self {
            revision: 0,
            map_seed,
            rng,
            next_id: 1,
            columns: BTreeSet::new(),
        })
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn map_seed(&self) -> u64 {
        self.map_seed
    }
    pub fn rng(&self) -> u64 {
        self.rng
    }
    pub fn next_id(&self) -> u64 {
        self.next_id
    }
    pub fn len(&self) -> usize {
        self.columns.len()
    }
    pub fn is_empty(&self) -> bool {
        self.columns.is_empty()
    }
    pub fn populated(&self, column: Column) -> bool {
        self.columns.contains(&column)
    }
    pub fn plan(
        &self,
        rng_before: u64,
        rng_after: u64,
        next_after: u64,
        column: Option<Column>,
    ) -> Result<Option<Delta>, Error> {
        if rng_before != self.rng {
            return Err(Error::Stale);
        }
        if rng_after == self.rng && next_after == self.next_id && column.is_none() {
            return Ok(None);
        }
        let delta = Delta {
            revision: self.revision,
            rng_before,
            rng_after,
            next_before: self.next_id,
            next_after,
            column,
        };
        self.validate(delta)?;
        Ok(Some(delta))
    }
    pub fn validate(&self, delta: Delta) -> Result<(), Error> {
        delta.validate()?;
        if delta.revision != self.revision
            || delta.rng_before != self.rng
            || delta.next_before != self.next_id
        {
            return Err(Error::Stale);
        }
        self.revision.checked_add(1).ok_or(Error::Overflow)?;
        if let Some(column) = delta.column {
            if self.populated(column) {
                return Err(Error::Stale);
            }
            if self.columns.len() == MAX_COLUMNS {
                return Err(Error::Capacity);
            }
        }
        Ok(())
    }
    pub fn apply(&mut self, delta: Delta) -> Result<(), Error> {
        self.validate(delta)?;
        if let Some(column) = delta.column {
            self.columns.insert(column);
        }
        self.revision += 1;
        self.rng = delta.rng_after;
        self.next_id = delta.next_after;
        Ok(())
    }
    /// Checkpoints are explicit loading work. Live operations encode only Delta.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(40 + 12 * self.columns.len());
        out.extend_from_slice(b"NAT1");
        for v in [self.revision, self.map_seed, self.rng, self.next_id] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&(self.columns.len() as u32).to_le_bytes());
        for column in &self.columns {
            write_column(&mut out, *column);
        }
        out
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() < 40 || bytes.len() > 40 + 12 * MAX_COLUMNS || &bytes[..4] != b"NAT1" {
            return Err(Error::Encoding);
        }
        let revision = u64_at(bytes, 4)?;
        let map_seed = u64_at(bytes, 12)?;
        let rng = u64_at(bytes, 20)?;
        let next_id = u64_at(bytes, 28)?;
        let count =
            u32::from_le_bytes(bytes[36..40].try_into().map_err(|_| Error::Encoding)?) as usize;
        if count > MAX_COLUMNS
            || bytes.len() != 40 + 12 * count
            || rng == 0
            || next_id == 0
            || next_id > MAX_ID
        {
            return Err(Error::Bounds);
        }
        let mut columns = BTreeSet::new();
        let mut previous = None;
        for raw in bytes[40..].chunks_exact(12) {
            let column = read_column(raw)?;
            if previous.is_some_and(|p| p >= column) {
                return Err(Error::Encoding);
            }
            columns.insert(column);
            previous = Some(column);
        }
        Ok(Self {
            revision,
            map_seed,
            rng,
            next_id,
            columns,
        })
    }
}
impl Delta {
    fn validate(self) -> Result<(), Error> {
        if self.rng_before == 0
            || self.rng_after == 0
            || self.next_before == 0
            || self.next_after > MAX_ID
            || self.next_after < self.next_before
            || self.next_after - self.next_before > 1024
            || self.column.is_some_and(|c| !c.valid())
        {
            return Err(Error::Bounds);
        }
        if self.rng_before == self.rng_after
            && self.next_before == self.next_after
            && self.column.is_none()
        {
            return Err(Error::Encoding);
        }
        Ok(())
    }
    pub fn encode(self) -> Vec<u8> {
        let mut out = Vec::with_capacity(57);
        out.extend_from_slice(b"NAD1");
        for value in [
            self.revision,
            self.rng_before,
            self.rng_after,
            self.next_before,
            self.next_after,
        ] {
            out.extend_from_slice(&value.to_le_bytes());
        }
        out.push(u8::from(self.column.is_some()));
        if let Some(column) = self.column {
            write_column(&mut out, column);
        }
        out
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if !matches!(bytes.len(), 45 | 57) || &bytes[..4] != b"NAD1" {
            return Err(Error::Encoding);
        }
        let column = match (bytes[44], bytes.len()) {
            (0, 45) => None,
            (1, 57) => Some(read_column(&bytes[45..])?),
            _ => return Err(Error::Encoding),
        };
        let out = Self {
            revision: u64_at(bytes, 4)?,
            rng_before: u64_at(bytes, 12)?,
            rng_after: u64_at(bytes, 20)?,
            next_before: u64_at(bytes, 28)?,
            next_after: u64_at(bytes, 36)?,
            column,
        };
        out.validate()?;
        Ok(out)
    }
}
fn write_column(out: &mut Vec<u8>, column: Column) {
    out.extend_from_slice(&column.space.0.to_le_bytes());
    out.extend_from_slice(&column.x.to_le_bytes());
    out.extend_from_slice(&column.z.to_le_bytes());
}
fn read_column(raw: &[u8]) -> Result<Column, Error> {
    if raw.len() != 12 {
        return Err(Error::Encoding);
    }
    let out = Column {
        space: SpaceId(u32::from_le_bytes(
            raw[..4].try_into().map_err(|_| Error::Encoding)?,
        )),
        x: i32::from_le_bytes(raw[4..8].try_into().map_err(|_| Error::Encoding)?),
        z: i32::from_le_bytes(raw[8..12].try_into().map_err(|_| Error::Encoding)?),
    };
    if !out.valid() {
        return Err(Error::Bounds);
    }
    Ok(out)
}
fn u64_at(bytes: &[u8], at: usize) -> Result<u64, Error> {
    Ok(u64::from_le_bytes(
        bytes
            .get(at..at + 8)
            .ok_or(Error::Encoding)?
            .try_into()
            .map_err(|_| Error::Encoding)?,
    ))
}
