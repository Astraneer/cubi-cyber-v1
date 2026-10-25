//! Immutable native/browser worker tasks. No DOM, network, private ore seed or authority dependency.
use core_types::{Cell, ChannelMask, LocalIdx, PageId, Version};
use render::terrain_mesh::{
    CellMaterial, HALO_VOLUME, MAX_PAGE_QUADS, MeshClass, MeshScratch, MeshWork, PageHalo,
    PageMesh, QuadDescriptor, mesh_page_indexed,
};
use std::{cell::RefCell, sync::Arc};
use telemetry::WorkCounts;
use terrain_gen::{GeneratedPage, GenerationTask};
use world::{Page, PageRegion, SampledPageInput as PageInput};

pub const MESH_CHANNELS: ChannelMask = ChannelMask(ChannelMask::SHAPE.0 | ChannelMask::OPACITY.0);
pub const INPUT_PAGES: usize = 27;
pub const MESH_RETAINED_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_MESH_RESULT_BYTES: usize = 512 * 1024;
const MESH_KIND: u8 = 2;
const GENERATION_KIND: u8 = 1;

pub(crate) fn neighbor(page: PageId, dx: i32, dy: i32, dz: i32) -> Option<PageId> {
    Some(PageId {
        space: page.space,
        x: page.x.checked_add(dx)?,
        y: page.y.checked_add(dy)?,
        z: page.z.checked_add(dz)?,
    })
}
pub(crate) fn neighbor_index(dx: i32, dy: i32, dz: i32) -> usize {
    ((dx + 1) + 3 * (dz + 1) + 9 * (dy + 1)) as usize
}
fn local_index(x: i8, y: i8, z: i8) -> LocalIdx {
    LocalIdx::new(
        x.rem_euclid(16) as u16 + z.rem_euclid(16) as u16 * 16 + y.rem_euclid(16) as u16 * 256,
    )
    .expect("16³ local coordinates")
}

enum MeshSource {
    Snapshots(Box<[Arc<Page>; INPUT_PAGES]>),
    Halo(Box<PageHalo>),
}
/// Native retains only immutable Arc snapshots. Browser encoding copies exactly
/// the centre page and one-cell rim, never all 27 full neighbouring pages.
pub struct MeshTask {
    pub page: PageId,
    pub version: Version,
    pub inputs: [PageInput; INPUT_PAGES],
    source: MeshSource,
    tints: Box<[u16; 256]>,
}
impl std::fmt::Debug for MeshTask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MeshTask")
            .field("page", &self.page)
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}
impl MeshTask {
    pub(crate) fn snapshots(
        page: PageId,
        inputs: [PageInput; INPUT_PAGES],
        snapshots: [Arc<Page>; INPUT_PAGES],
        tints: Box<[u16; 256]>,
    ) -> Self {
        Self {
            page,
            version: inputs[13].version,
            inputs,
            source: MeshSource::Snapshots(Box::new(snapshots)),
            tints,
        }
    }
    fn raw(&self, x: i8, y: i8, z: i8) -> u32 {
        match &self.source {
            MeshSource::Halo(halo) => halo.get(x, y, z),
            MeshSource::Snapshots(pages) => pages[neighbor_index(
                i32::from(x.div_euclid(16)),
                i32::from(y.div_euclid(16)),
                i32::from(z.div_euclid(16)),
            )]
            .cell(local_index(x, y, z))
            .raw(),
        }
    }
    fn halo(&self) -> Box<PageHalo> {
        match &self.source {
            MeshSource::Halo(halo) => halo.clone(),
            MeshSource::Snapshots(_) => {
                let mut halo = Box::new(PageHalo::filled(0));
                for y in -1..=16 {
                    for z in -1..=16 {
                        for x in -1..=16 {
                            halo.set(x, y, z, self.raw(x, y, z));
                        }
                    }
                }
                halo
            }
        }
    }
    pub fn execute(self, cancel: impl Fn() -> bool) -> Result<CompletedMesh, String> {
        if cancel() {
            return Err("mesh job cancelled".into());
        }
        let halo = self.halo();
        if cancel() {
            return Err("mesh job cancelled".into());
        }
        let mut mesh = PageMesh::with_capacity([1024, 256, 256]);
        let work = MESH_SCRATCH.with(|scratch| {
            mesh_page_indexed(
                &halo,
                |raw, xyz| {
                    material(
                        raw,
                        self.tints
                            [(xyz[0].clamp(0, 15) as usize) + 16 * (xyz[2].clamp(0, 15) as usize)],
                    )
                },
                &mut scratch.borrow_mut(),
                &mut mesh,
            )
        });
        if cancel() {
            return Err("mesh job cancelled".into());
        }
        Ok(CompletedMesh {
            page: self.page,
            version: self.version,
            inputs: self.inputs,
            mesh,
            work,
            copied_halo_cells: HALO_VOLUME as u64,
        })
    }
}
thread_local! { static MESH_SCRATCH: RefCell<MeshScratch> = RefCell::new(MeshScratch::new()); }

/// Converts registry render metadata without allocation. Models/cross vegetation
/// are handled by later instanced-model passes; terrain here meshes cube buckets.
pub fn material(raw: u32, tint: u16) -> CellMaterial {
    use rules::blocks::Render;
    let cell = Cell::from_raw(raw);
    let Some(definition) = rules::block(cell.id()) else {
        return CellMaterial::opaque(0);
    };
    let class = match definition.render {
        Render::None | Render::Model(_) | Render::Cross => return CellMaterial::EMPTY,
        Render::Cube => MeshClass::Opaque,
        Render::Leaves | Render::Glass => MeshClass::Cutout,
        Render::Water => MeshClass::Water,
    };
    let face_materials = std::array::from_fn(|face| {
        // Registry face arrays use its preserved legacy mesh order.
        rules::blocks::layer_cell(
            cell.id(),
            cell.state(),
            rules::blocks::mesh_face(face as u8),
        )
        .0 as u16
    });
    let rgba = if definition.tint != 0 {
        let r = u32::from((tint >> 11) & 31) * 255 / 31;
        let g = u32::from((tint >> 5) & 63) * 255 / 63;
        let b = u32::from(tint & 31) * 255 / 31;
        r | (g << 8) | (b << 16) | 0xff00_0000
    } else {
        u32::MAX
    };
    CellMaterial {
        face_materials,
        face_uv_rotations: std::array::from_fn(|face| {
            u8::from(
                rules::blocks::layer_cell(
                    cell.id(),
                    cell.state(),
                    rules::blocks::mesh_face(face as u8),
                )
                .1,
            )
        }),
        tint: rgba,
        tint_faces: (0..6).fold(0, |mask, face| {
            mask | if rules::blocks::tinted(cell.id(), rules::blocks::mesh_face(face)) {
                1 << face
            } else {
                0
            }
        }),
        class,
        occludes: rules::properties(cell).opaque,
        casts_ao: definition.suffocates && !matches!(definition.render, Render::Water),
        cull_group: if matches!(definition.render, Render::Water | Render::Glass) {
            cell.id()
        } else {
            0
        },
    }
}

#[derive(Debug)]
pub struct CompletedMesh {
    pub page: PageId,
    pub version: Version,
    pub inputs: [PageInput; INPUT_PAGES],
    pub mesh: PageMesh,
    pub work: MeshWork,
    pub copied_halo_cells: u64,
}
impl CompletedMesh {
    pub fn retained_bytes(&self) -> usize {
        (self.mesh.opaque.capacity() + self.mesh.cutout.capacity() + self.mesh.water.capacity())
            * 16
            + std::mem::size_of::<Self>()
    }
}
#[derive(Debug)]
pub enum SceneTask {
    Generation(GenerationTask),
    Mesh(Box<MeshTask>),
}
#[derive(Debug)]
pub enum SceneOutput {
    Generation(GeneratedPage),
    Mesh(Box<CompletedMesh>),
}
impl SceneTask {
    pub fn execute(self, cancel: &jobs::CancellationToken) -> Result<SceneOutput, String> {
        self.execute_with_cancellation(|| cancel.is_cancelled())
    }
    pub fn execute_with_cancellation(
        self,
        cancel: impl Fn() -> bool,
    ) -> Result<SceneOutput, String> {
        match self {
            Self::Generation(task) => task
                .execute_with_cancellation(cancel)
                .map(SceneOutput::Generation),
            Self::Mesh(task) => (*task)
                .execute(cancel)
                .map(|mesh| SceneOutput::Mesh(Box::new(mesh))),
        }
    }
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(match self {
            Self::Generation(_) => 5 + terrain_gen::TASK_BYTES,
            Self::Mesh(_) => 5 + 16 + 8 + INPUT_PAGES * 26 + 512 + HALO_VOLUME * 4,
        });
        out.extend_from_slice(b"CST3");
        match self {
            Self::Generation(task) => {
                out.push(GENERATION_KIND);
                out.extend_from_slice(&task.encode());
            }
            Self::Mesh(task) => {
                out.push(MESH_KIND);
                put_page(&mut out, task.page);
                put_u64(&mut out, task.version.0);
                put_inputs(&mut out, &task.inputs);
                for tint in task.tints.iter() {
                    out.extend_from_slice(&tint.to_le_bytes());
                }
                for y in -1..=16 {
                    for z in -1..=16 {
                        for x in -1..=16 {
                            out.extend_from_slice(&task.raw(x, y, z).to_le_bytes());
                        }
                    }
                }
            }
        }
        out
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, WireError> {
        let mut r = Reader::new(bytes, b"CST3")?;
        match r.byte()? {
            GENERATION_KIND => GenerationTask::decode(r.rest())
                .map(Self::Generation)
                .map_err(|_| WireError::Generation),
            MESH_KIND => {
                let page = r.page()?;
                let version = Version(r.u64()?);
                let inputs = r.inputs(page)?;
                if version != inputs[13].version {
                    return Err(WireError::Inputs);
                }
                let mut tints = Box::new([0; 256]);
                for tint in tints.iter_mut() {
                    *tint = r.u16()?;
                }
                let mut halo = Box::new(PageHalo::filled(0));
                for y in -1..=16 {
                    for z in -1..=16 {
                        for x in -1..=16 {
                            halo.set(x, y, z, r.u32()?);
                        }
                    }
                }
                r.end()?;
                Ok(Self::Mesh(Box::new(MeshTask {
                    page,
                    version,
                    inputs,
                    source: MeshSource::Halo(halo),
                    tints,
                })))
            }
            _ => Err(WireError::Kind),
        }
    }
}
impl SceneOutput {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"CSO3");
        match self {
            Self::Generation(page) => {
                out.push(GENERATION_KIND);
                out.extend_from_slice(&page.encode());
            }
            Self::Mesh(result) => {
                out.push(MESH_KIND);
                put_page(&mut out, result.page);
                put_u64(&mut out, result.version.0);
                put_inputs(&mut out, &result.inputs);
                let w = result.work;
                for value in [
                    w.work.cells_scanned,
                    w.work.quads_emitted,
                    w.work.upload_bytes,
                    w.work.pipeline_compilations,
                    w.work.synchronous_generations,
                    w.generated_bytes,
                    u64::from(w.rim_cells_scanned),
                    u64::from(w.visibility_words),
                    w.cached_material_lookups,
                    w.ao_samples,
                    result.copied_halo_cells,
                ] {
                    put_u64(&mut out, value);
                }
                for bucket in [&result.mesh.opaque, &result.mesh.cutout, &result.mesh.water] {
                    out.extend_from_slice(&(bucket.len() as u32).to_le_bytes());
                }
                for quad in result.mesh.iter() {
                    out.extend_from_slice(&quad.to_le_bytes());
                }
            }
        }
        out
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, WireError> {
        if bytes.len() > MAX_MESH_RESULT_BYTES {
            return Err(WireError::Length);
        }
        let mut r = Reader::new(bytes, b"CSO3")?;
        match r.byte()? {
            GENERATION_KIND => GeneratedPage::decode(r.rest())
                .map(Self::Generation)
                .map_err(|_| WireError::Generation),
            MESH_KIND => {
                let page = r.page()?;
                let version = Version(r.u64()?);
                let inputs = r.inputs(page)?;
                if version != inputs[13].version {
                    return Err(WireError::Inputs);
                }
                let mut numbers = [0_u64; 11];
                for value in &mut numbers {
                    *value = r.u64()?;
                }
                let mut lengths = [0; 3];
                for length in &mut lengths {
                    *length = r.u32()? as usize;
                }
                let count = lengths
                    .iter()
                    .try_fold(0_usize, |sum, next| sum.checked_add(*next))
                    .ok_or(WireError::Length)?;
                if count > MAX_PAGE_QUADS || r.rest().len() != count * 16 {
                    return Err(WireError::Length);
                }
                let mut mesh = PageMesh::with_capacity(lengths);
                for (bucket, count) in [&mut mesh.opaque, &mut mesh.cutout, &mut mesh.water]
                    .into_iter()
                    .zip(lengths)
                {
                    for _ in 0..count {
                        bucket.push(
                            QuadDescriptor::from_le_bytes(r.array()?)
                                .map_err(|_| WireError::Descriptor)?,
                        );
                    }
                }
                r.end()?;
                if numbers[0] != HALO_VOLUME as u64
                    || numbers[1] != count as u64
                    || numbers[5] != (count * 16) as u64
                    || numbers[10] != HALO_VOLUME as u64
                    || numbers[2..5] != [0; 3]
                {
                    return Err(WireError::Work);
                }
                let work = MeshWork {
                    work: WorkCounts {
                        cells_scanned: numbers[0],
                        quads_emitted: numbers[1],
                        upload_bytes: numbers[2],
                        pipeline_compilations: numbers[3],
                        synchronous_generations: numbers[4],
                    },
                    generated_bytes: numbers[5],
                    rim_cells_scanned: u32::try_from(numbers[6]).map_err(|_| WireError::Work)?,
                    visibility_words: u32::try_from(numbers[7]).map_err(|_| WireError::Work)?,
                    cached_material_lookups: numbers[8],
                    ao_samples: numbers[9],
                };
                Ok(Self::Mesh(Box::new(CompletedMesh {
                    page,
                    version,
                    inputs,
                    mesh,
                    work,
                    copied_halo_cells: numbers[10],
                })))
            }
            _ => Err(WireError::Kind),
        }
    }
}
/// Worker entry point used by the thin WASM shell. Both task kinds stay off its render thread.
pub fn execute_task(request: &[u8], cancel: impl Fn() -> bool) -> Result<Vec<u8>, String> {
    SceneTask::decode(request)
        .map_err(|error| error.to_string())?
        .execute_with_cancellation(cancel)
        .map(|output| output.encode())
}
#[cfg(not(target_arch = "wasm32"))]
pub fn native_pool(
    workers: usize,
    capacity: usize,
) -> Result<jobs::NativePool<SceneTask, SceneOutput>, jobs::PoolError> {
    jobs::NativePool::new(workers, capacity, SceneTask::execute)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireError {
    Length,
    Magic,
    Kind,
    Inputs,
    Generation,
    Descriptor,
    Work,
}
impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid client worker message: {self:?}")
    }
}
impl std::error::Error for WireError {}
fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}
fn put_page(out: &mut Vec<u8>, page: PageId) {
    for n in [page.space.0, page.x as u32, page.y as u32, page.z as u32] {
        out.extend_from_slice(&n.to_le_bytes());
    }
}
fn put_inputs(out: &mut Vec<u8>, inputs: &[PageInput; INPUT_PAGES]) {
    for input in inputs {
        put_page(out, input.page);
        out.extend_from_slice(&input.channels.0.to_le_bytes());
        put_u64(out, input.version.0);
    }
}
struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}
impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8], magic: &[u8; 4]) -> Result<Self, WireError> {
        if bytes.len() < 5 {
            return Err(WireError::Length);
        }
        if &bytes[..4] != magic {
            return Err(WireError::Magic);
        }
        Ok(Self { bytes, offset: 4 })
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N], WireError> {
        let end = self.offset.checked_add(N).ok_or(WireError::Length)?;
        let source = self.bytes.get(self.offset..end).ok_or(WireError::Length)?;
        self.offset = end;
        Ok(source.try_into().expect("bounded exact array"))
    }
    fn byte(&mut self) -> Result<u8, WireError> {
        Ok(self.array::<1>()?[0])
    }
    fn u16(&mut self) -> Result<u16, WireError> {
        Ok(u16::from_le_bytes(self.array()?))
    }
    fn u32(&mut self) -> Result<u32, WireError> {
        Ok(u32::from_le_bytes(self.array()?))
    }
    fn u64(&mut self) -> Result<u64, WireError> {
        Ok(u64::from_le_bytes(self.array()?))
    }
    fn page(&mut self) -> Result<PageId, WireError> {
        Ok(PageId {
            space: core_types::SpaceId(self.u32()?),
            x: self.u32()? as i32,
            y: self.u32()? as i32,
            z: self.u32()? as i32,
        })
    }
    fn inputs(&mut self, page: PageId) -> Result<[PageInput; INPUT_PAGES], WireError> {
        let mut inputs = [PageInput {
            page,
            channels: MESH_CHANNELS,
            region: PageRegion::WHOLE,
            version: Version(0),
        }; INPUT_PAGES];
        for dy in -1..=1 {
            for dz in -1..=1 {
                for dx in -1..=1 {
                    let index = neighbor_index(dx, dy, dz);
                    let input = PageInput {
                        page: self.page()?,
                        channels: ChannelMask(self.u16()?),
                        // The fixed 27-source order defines the sampled rim.
                        // Worker-supplied bytes cannot weaken this region.
                        region: PageRegion::new([-dx as i8, -dy as i8, -dz as i8])
                            .expect("bounded neighbour offsets"),
                        version: Version(self.u64()?),
                    };
                    if Some(input.page) != neighbor(page, dx, dy, dz)
                        || input.channels != MESH_CHANNELS
                    {
                        return Err(WireError::Inputs);
                    }
                    inputs[index] = input;
                }
            }
        }
        Ok(inputs)
    }
    fn rest(&self) -> &'a [u8] {
        &self.bytes[self.offset..]
    }
    fn end(&self) -> Result<(), WireError> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err(WireError::Length)
        }
    }
}
