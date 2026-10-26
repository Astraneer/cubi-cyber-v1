//! Classified far-water CPU work, shared by native and browser workers. The
//! foreground constructor reads only fixed header fields and retains one Arc.
use crate::water::{self, SurfaceCell, WaterMesh, WaterMeshError, WaterMeshWork};
use core_types::{PageId, SpaceId, Version};
use net::water::{Request, WaterCell, WaterHalo};
use render::{
    terrain_mesh::{Face, QuadDescriptor},
    water_gpu::WaterQuad,
};
use std::sync::Arc;
pub const MAX_QUADS: usize = 6 * 4096;
pub const REQUEST_BYTES: usize = 8 + net::water::MAX_BYTES;
const RESULT_HEADER: usize = 128;
pub const MAX_RESULT_BYTES: usize = RESULT_HEADER + MAX_QUADS * 16;
pub const MAX_RETAINED_RESULT_BYTES: usize = std::mem::size_of::<BuiltFarWater>() + MAX_QUADS * 16;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Length,
    Magic,
    Bounds,
    Cell,
    Work,
    Descriptor,
    Cancelled,
    Capacity,
    Mesh,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    pub page: PageId,
    pub geometry_source: Version,
    pub geometry_view: u64,
    pub column_source: u64,
    pub water_view: u64,
}
impl Stamp {
    pub fn from_halo(halo: &WaterHalo) -> Self {
        Self {
            page: halo.page,
            geometry_source: halo.geometry_source,
            geometry_view: halo.geometry_view,
            column_source: halo.column_source,
            water_view: halo.water_view,
        }
    }
    fn validate(self) -> Result<(), Error> {
        Request {
            page: self.page,
            geometry_source: self.geometry_source,
            geometry_view: self.geometry_view,
            column_source: self.column_source,
            acknowledged: None,
        }
        .validate()
        .map_err(|_| Error::Bounds)?;
        if self.water_view == 0 {
            return Err(Error::Bounds);
        }
        Ok(())
    }
    pub fn matches(self, halo: &WaterHalo) -> bool {
        self == Self::from_halo(halo)
    }
}
pub struct FarWaterTask {
    input: Arc<WaterHalo>,
    max_quads: usize,
}
impl std::fmt::Debug for FarWaterTask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FarWaterTask")
            .field("stamp", &self.stamp())
            .field("max_quads", &self.max_quads)
            .finish()
    }
}
pub struct BuiltFarWater {
    pub stamp: Stamp,
    pub mesh: WaterMesh,
    pub classified_cells: u64,
    pub dependency_samples: u64,
    /// Unknown core cells or water cells depending on any Unknown neighbor. This
    /// additionally guards empty certificates and withheld bottom/flow surfaces.
    pub unknown_dependency_cells: u64,
}
impl std::fmt::Debug for BuiltFarWater {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BuiltFarWater")
            .field("stamp", &self.stamp)
            .field("quads", &self.mesh.quads.len())
            .field("unknown_dependency_cells", &self.unknown_dependency_cells)
            .finish()
    }
}
impl FarWaterTask {
    pub fn new(input: Arc<WaterHalo>, max_quads: usize) -> Result<Self, Error> {
        Stamp::from_halo(&input).validate()?;
        if max_quads == 0 || max_quads > MAX_QUADS {
            return Err(Error::Bounds);
        }
        Ok(Self { input, max_quads })
    }
    pub fn stamp(&self) -> Stamp {
        Stamp::from_halo(&self.input)
    }
    pub fn execute(self, cancel: impl Fn() -> bool) -> Result<Box<BuiltFarWater>, Error> {
        if cancel() {
            return Err(Error::Cancelled);
        }
        let mut invalid = false;
        let mut cancelled = false;
        let mut classified_cells = 0;
        let halo = water::WaterHalo::from_fn(|x, y, z| {
            if x == -1 && z == -1 && cancel() {
                cancelled = true;
            }
            if cancelled {
                return SurfaceCell::Unknown;
            }
            let index = (x + 1) as usize + 18 * ((z + 1) as usize + 18 * (y + 1) as usize);
            classified_cells += 1;
            match WaterCell::decode(self.input.cells[index]) {
                Ok(WaterCell::Unknown) => SurfaceCell::Unknown,
                Ok(WaterCell::Air) => SurfaceCell::Air,
                Ok(WaterCell::Solid) => SurfaceCell::Solid,
                Ok(WaterCell::Water { amount, falling }) => SurfaceCell::Water { amount, falling },
                Err(_) => {
                    invalid = true;
                    SurfaceCell::Unknown
                }
            }
        });
        if cancelled {
            return Err(Error::Cancelled);
        }
        if invalid {
            return Err(Error::Cell);
        }
        let tints = self.input.tints.map(|tint| {
            let r = u32::from(tint >> 11 & 31) * 255 / 31;
            let g = u32::from(tint >> 5 & 63) * 255 / 63;
            let b = u32::from(tint & 31) * 255 / 31;
            r | g << 8 | b << 16 | 0xff000000
        });
        let mut unknown_dependency_cells = 0;
        let mut dependency_samples = 0;
        for y in 0..16i8 {
            if cancel() {
                return Err(Error::Cancelled);
            }
            for z in 0..16i8 {
                for x in 0..16i8 {
                    match halo.get(x, y, z) {
                        SurfaceCell::Unknown => unknown_dependency_cells += 1,
                        SurfaceCell::Water { .. } => {
                            let mut unknown = false;
                            // Conservative dependency closure for corner heights,
                            // every face and the flow samples one cell below it.
                            for dy in -1..=1 {
                                for dz in -1..=1 {
                                    for dx in -1..=1 {
                                        dependency_samples += 1;
                                        unknown |= halo.get(x + dx, y + dy, z + dz)
                                            == SurfaceCell::Unknown;
                                    }
                                }
                            }
                            unknown_dependency_cells += u64::from(unknown);
                        }
                        _ => {}
                    }
                }
            }
        }
        let mut mesh =
            water::mesh_water(&halo, &tints, self.max_quads, cancel).map_err(|e| match e {
                WaterMeshError::Cancelled => Error::Cancelled,
                WaterMeshError::Capacity => Error::Capacity,
                _ => Error::Mesh,
            })?;
        mesh.quads.shrink_to_fit();
        Ok(Box::new(BuiltFarWater {
            stamp: self.stamp(),
            mesh,
            classified_cells,
            dependency_samples,
            unknown_dependency_cells,
        }))
    }
    /// Fixed public byte copy for browser dispatch; semantic cell/tint decoding
    /// and all mesh/dependency work stay inside execute().
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        let input = net::water::encode(&self.input).map_err(|_| Error::Cell)?;
        let mut bytes = Vec::with_capacity(REQUEST_BYTES);
        bytes.extend_from_slice(b"FWJ1");
        bytes.extend_from_slice(&(self.max_quads as u32).to_le_bytes());
        bytes.extend_from_slice(&input);
        Ok(bytes)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() != REQUEST_BYTES {
            return Err(Error::Length);
        }
        if &bytes[..4] != b"FWJ1" {
            return Err(Error::Magic);
        }
        let max_quads = u32::from_le_bytes(bytes[4..8].try_into().expect("fixed size")) as usize;
        if max_quads == 0 || max_quads > MAX_QUADS {
            return Err(Error::Bounds);
        }
        let input = net::water::decode(&bytes[8..]).map_err(|_| Error::Cell)?;
        Self::new(Arc::from(input), max_quads)
    }
}
impl BuiltFarWater {
    pub fn matches(&self, halo: &WaterHalo) -> bool {
        self.stamp.matches(halo)
    }
    pub fn complete(&self) -> bool {
        self.unknown_dependency_cells == 0 && self.mesh.work.unknown_surface_cells == 0
    }
    fn validate(&self) -> Result<(), Error> {
        self.stamp.validate()?;
        let w = self.mesh.work;
        if self.mesh.quads.len() > MAX_QUADS {
            return Err(Error::Length);
        }
        if self.classified_cells != 5832
            || self.dependency_samples > 4096 * 27
            || self.unknown_dependency_cells > 4096
            || w.snapshot_lookups != 0
            || w.halo_samples != 5832
            || w.cells_scanned != 4096
            || w.quads != self.mesh.quads.len() as u64
            || w.cpu_frame_vertices != 0
            || w.unknown_surface_cells > self.unknown_dependency_cells
        {
            return Err(Error::Work);
        }
        for quad in &self.mesh.quads {
            validate_quad(*quad)?;
        }
        Ok(())
    }
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        self.validate()?;
        let mut bytes = Vec::with_capacity(RESULT_HEADER + self.mesh.quads.len() * 16);
        bytes.extend_from_slice(b"FWR1");
        write_stamp(&mut bytes, self.stamp);
        for value in [
            self.classified_cells,
            self.dependency_samples,
            self.unknown_dependency_cells,
            self.mesh.work.snapshot_lookups,
            self.mesh.work.halo_samples,
            self.mesh.work.cells_scanned,
            self.mesh.work.quads,
            self.mesh.work.unknown_surface_cells,
            self.mesh.work.cpu_frame_vertices,
        ] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes.extend_from_slice(&(self.mesh.quads.len() as u32).to_le_bytes());
        for quad in &self.mesh.quads {
            for word in quad.words {
                bytes.extend_from_slice(&word.to_le_bytes());
            }
        }
        Ok(bytes)
    }
    pub fn decode(bytes: &[u8]) -> Result<Box<Self>, Error> {
        if !(RESULT_HEADER..=MAX_RESULT_BYTES).contains(&bytes.len()) {
            return Err(Error::Length);
        }
        let mut r = Reader { bytes, at: 0 };
        if r.take::<4>()? != *b"FWR1" {
            return Err(Error::Magic);
        }
        let stamp = Stamp {
            page: PageId {
                space: SpaceId(r.u32()?),
                x: r.i32()?,
                y: r.i32()?,
                z: r.i32()?,
            },
            geometry_source: Version(r.u64()?),
            geometry_view: r.u64()?,
            column_source: r.u64()?,
            water_view: r.u64()?,
        };
        stamp.validate()?;
        let mut work = [0; 9];
        for v in &mut work {
            *v = r.u64()?;
        }
        let count = r.u32()? as usize;
        if count > MAX_QUADS || r.at + count * 16 != bytes.len() {
            return Err(Error::Length);
        }
        // Validate the fixed counters before reserving a potentially large mesh.
        if work[0] != 5832
            || work[1] > 4096 * 27
            || work[2] > 4096
            || work[3] != 0
            || work[4] != 5832
            || work[5] != 4096
            || work[6] != count as u64
            || work[7] > work[2]
            || work[8] != 0
        {
            return Err(Error::Work);
        }
        let mut quads = Vec::with_capacity(count);
        for _ in 0..count {
            let mut words = [0; 4];
            for word in &mut words {
                *word = r.u32()?;
            }
            let quad = WaterQuad { words };
            validate_quad(quad)?;
            quads.push(quad);
        }
        Ok(Box::new(Self {
            stamp,
            classified_cells: work[0],
            dependency_samples: work[1],
            unknown_dependency_cells: work[2],
            mesh: WaterMesh {
                quads,
                work: WaterMeshWork {
                    snapshot_lookups: work[3],
                    halo_samples: work[4],
                    cells_scanned: work[5],
                    quads: work[6],
                    unknown_surface_cells: work[7],
                    cpu_frame_vertices: work[8],
                },
            },
        }))
    }
}
fn validate_quad(quad: WaterQuad) -> Result<(), Error> {
    let b = quad.words[0];
    let face = Face::from_index((b >> 15 & 7) as u8).ok_or(Error::Descriptor)?;
    if b >> 26 != 0
        || quad.words[3] & 65535 != 0
        || quad.words[3] >> 16 & 255 == 128
        || quad.words[3] >> 24 == 128
        || QuadDescriptor::new(
            [(b & 31) as u8, (b >> 5 & 31) as u8, (b >> 10 & 31) as u8],
            face,
            [(b >> 18 & 15) as u8 + 1, (b >> 22 & 15) as u8 + 1],
            0,
            [3; 4],
            0,
        )
        .is_err()
    {
        return Err(Error::Descriptor);
    }
    Ok(())
}
fn write_stamp(bytes: &mut Vec<u8>, s: Stamp) {
    bytes.extend_from_slice(&s.page.space.0.to_le_bytes());
    for c in [s.page.x, s.page.y, s.page.z] {
        bytes.extend_from_slice(&c.to_le_bytes());
    }
    for v in [
        s.geometry_source.0,
        s.geometry_view,
        s.column_source,
        s.water_view,
    ] {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
}
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        let end = self.at.checked_add(N).ok_or(Error::Length)?;
        let slice = self.bytes.get(self.at..end).ok_or(Error::Length)?;
        self.at = end;
        Ok(slice.try_into().expect("fixed slice"))
    }
    fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(self.take()?))
    }
    fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(self.take()?))
    }
    fn i32(&mut self) -> Result<i32, Error> {
        Ok(i32::from_le_bytes(self.take()?))
    }
}
pub fn execute_far_water_request(
    bytes: &[u8],
    cancel: impl Fn() -> bool,
) -> Result<Vec<u8>, Error> {
    FarWaterTask::decode(bytes)?.execute(cancel)?.encode()
}
#[cfg(not(target_arch = "wasm32"))]
pub fn native_pool(
    workers: usize,
    capacity: usize,
) -> Result<jobs::NativePool<FarWaterTask, Box<BuiltFarWater>>, jobs::PoolError> {
    jobs::NativePool::new(workers, capacity, |task: FarWaterTask, cancel| {
        task.execute(|| cancel.is_cancelled())
            .map_err(|e| e.to_string())
    })
}
