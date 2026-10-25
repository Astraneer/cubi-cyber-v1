//! Bounded little-endian worker messages. These are local job messages, not the game protocol.
use crate::{
    GENERATOR_VERSION, GeneratedPage, GenerationError, GenerationTask, GenerationWork, TerrainSeed,
    check_column,
};
use core_types::{Cell, PAGE_VOLUME, PageId, SpaceId, Version};

pub const TASK_BYTES: usize = 33;
pub const RESULT_BYTES: usize = 65 + PAGE_VOLUME * 4 + 256 * 2;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireError {
    Length,
    Magic,
    Tag,
    Coordinates,
    GeneratorVersion,
}
impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid generation worker message: {self:?}")
    }
}
impl std::error::Error for WireError {}
fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(
        bytes[offset..offset + 4]
            .try_into()
            .expect("validated bounded packet"),
    )
}
fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(
        bytes[offset..offset + 8]
            .try_into()
            .expect("validated bounded packet"),
    )
}
fn version(bytes: &[u8], offset: usize) -> Result<Option<Version>, WireError> {
    match bytes[offset] {
        0 if u64_at(bytes, offset + 1) == 0 => Ok(None),
        1 => Ok(Some(Version(u64_at(bytes, offset + 1)))),
        _ => Err(WireError::Tag),
    }
}
fn write_version(out: &mut [u8], offset: usize, value: Option<Version>) {
    out[offset] = u8::from(value.is_some());
    out[offset + 1..offset + 9].copy_from_slice(&value.map_or(0, |v| v.0).to_le_bytes());
}
impl GenerationTask {
    pub fn encode(&self) -> [u8; TASK_BYTES] {
        let mut out = [0u8; TASK_BYTES];
        out[..4].copy_from_slice(b"G4T1");
        for (i, v) in [
            self.terrain_seed.0,
            self.page.space.0,
            self.page.x as u32,
            self.page.y as u32,
            self.page.z as u32,
        ]
        .into_iter()
        .enumerate()
        {
            out[4 + i * 4..8 + i * 4].copy_from_slice(&v.to_le_bytes());
        }
        write_version(&mut out, 24, self.expected);
        out
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, WireError> {
        if bytes.len() != TASK_BYTES {
            return Err(WireError::Length);
        }
        if &bytes[..4] != b"G4T1" {
            return Err(WireError::Magic);
        }
        let page = PageId {
            space: SpaceId(u32_at(bytes, 8)),
            x: u32_at(bytes, 12) as i32,
            y: u32_at(bytes, 16) as i32,
            z: u32_at(bytes, 20) as i32,
        };
        check_column(page.x, page.z).map_err(|_: GenerationError| WireError::Coordinates)?;
        Ok(Self {
            terrain_seed: TerrainSeed(u32_at(bytes, 4)),
            page,
            expected: version(bytes, 24)?,
        })
    }
}
impl GeneratedPage {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = vec![0u8; RESULT_BYTES];
        out[..4].copy_from_slice(b"G4P1");
        for (i, v) in [
            self.page.space.0,
            self.page.x as u32,
            self.page.y as u32,
            self.page.z as u32,
        ]
        .into_iter()
        .enumerate()
        {
            out[4 + i * 4..8 + i * 4].copy_from_slice(&v.to_le_bytes());
        }
        write_version(&mut out, 20, self.expected);
        out[29..33].copy_from_slice(&self.generator_version.to_le_bytes());
        for (i, v) in [
            self.work.scratch_reads,
            self.work.scratch_writes,
            self.work.sampled_columns,
            self.work.copied_cells,
        ]
        .into_iter()
        .enumerate()
        {
            out[33 + i * 8..41 + i * 8].copy_from_slice(&v.to_le_bytes());
        }
        for (i, cell) in self.cells.iter().enumerate() {
            out[65 + i * 4..69 + i * 4].copy_from_slice(&cell.raw().to_le_bytes());
        }
        let tint_start = 65 + PAGE_VOLUME * 4;
        for (i, tint) in self.tints.iter().enumerate() {
            out[tint_start + i * 2..tint_start + 2 + i * 2].copy_from_slice(&tint.to_le_bytes());
        }
        out
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, WireError> {
        if bytes.len() != RESULT_BYTES {
            return Err(WireError::Length);
        }
        if &bytes[..4] != b"G4P1" {
            return Err(WireError::Magic);
        }
        let generator_version = u32_at(bytes, 29);
        if generator_version != GENERATOR_VERSION {
            return Err(WireError::GeneratorVersion);
        }
        let page = PageId {
            space: SpaceId(u32_at(bytes, 4)),
            x: u32_at(bytes, 8) as i32,
            y: u32_at(bytes, 12) as i32,
            z: u32_at(bytes, 16) as i32,
        };
        check_column(page.x, page.z).map_err(|_| WireError::Coordinates)?;
        let expected = version(bytes, 20)?;
        let mut cells = Box::new([Cell::AIR; PAGE_VOLUME]);
        for (i, cell) in cells.iter_mut().enumerate() {
            *cell = Cell::from_raw(u32_at(bytes, 65 + i * 4));
        }
        let mut tints = Box::new([0u16; 256]);
        let start = 65 + PAGE_VOLUME * 4;
        for (i, tint) in tints.iter_mut().enumerate() {
            *tint = u16::from_le_bytes(
                bytes[start + i * 2..start + 2 + i * 2]
                    .try_into()
                    .expect("bounded packet"),
            );
        }
        Ok(Self {
            page,
            expected,
            generator_version,
            cells,
            tints,
            work: GenerationWork {
                scratch_reads: u64_at(bytes, 33),
                scratch_writes: u64_at(bytes, 41),
                sampled_columns: u64_at(bytes, 49),
                copied_cells: u64_at(bytes, 57),
            },
        })
    }
}
