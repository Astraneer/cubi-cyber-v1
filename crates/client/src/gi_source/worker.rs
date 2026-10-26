//! Full source construction runs only on immutable native/Worker snapshots.
use super::*;
use core_types::Cell;
use world::{LightSample, Page};

pub const GI_RETAINED_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_GI_REQUEST_BYTES: usize = 512 * 1024;
pub const MAX_GI_RESULT_BYTES: usize = 4096 * 52 + 512;
const MAX_LAYERS: usize = 4096;

#[derive(Clone)]
pub struct GiBakeStamp {
    pub id: u64,
    pub page: PageId,
    pub epoch: u64,
    pub inputs: [PageInput; 7],
    versions: [Version; 7],
    albedo: Arc<GiAlbedo>,
    sky: [f32; 3],
    directional: bool,
}
enum InputPage {
    Native(Arc<Page>),
    Owned {
        cells: Box<[Cell; 4096]>,
        lights: Box<[LightSample; 4096]>,
    },
}
impl InputPage {
    fn cell(&self, index: LocalIdx) -> Cell {
        match self {
            Self::Native(page) => page.cell(index),
            Self::Owned { cells, .. } => cells[usize::from(index.get())],
        }
    }
    fn light(&self, index: LocalIdx) -> LightSample {
        match self {
            Self::Native(page) => page.light(index),
            Self::Owned { lights, .. } => lights[usize::from(index.get())],
        }
    }
}
struct SnapshotRead {
    ids: [PageId; 7],
    pages: [Option<InputPage>; 7],
}
impl SnapshotRead {
    fn page(&self, id: PageId) -> Option<&InputPage> {
        self.ids
            .iter()
            .position(|&page| page == id)
            .and_then(|i| self.pages[i].as_ref())
    }
}
impl SourceRead for SnapshotRead {
    fn read(&self, position: BlockPos) -> CellRead {
        self.page(position.page()).map_or(CellRead::Unknown, |p| {
            CellRead::Known(p.cell(position.local_index()))
        })
    }
    fn light(&self, position: BlockPos) -> Option<LightSample> {
        self.page(position.page())
            .map(|p| p.light(position.local_index()))
    }
}
pub struct GiBakeTask {
    stamp: GiBakeStamp,
    tints: Box<[u16; 256]>,
    snapshot: SnapshotRead,
}
impl std::fmt::Debug for GiBakeTask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GiBakeTask")
            .field("id", &self.stamp.id)
            .field("page", &self.stamp.page)
            .finish_non_exhaustive()
    }
}
pub struct GiBakedData {
    pub voxels: Box<[GiVoxel; 4096]>,
    pub surfaces: Option<Box<[GiSurface; 4096]>>,
    pub work: SourceWork,
}
pub struct BuiltGiSource {
    pub id: u64,
    page: PageId,
    inputs: [PageInput; 7],
    epoch: u64,
    pub result: Result<GiBakedData, GiSourceError>,
}
impl GiBakeTask {
    pub fn id(&self) -> u64 {
        self.stamp.id
    }
    pub fn page(&self) -> PageId {
        self.stamp.page
    }
    pub fn stamp(&self) -> GiBakeStamp {
        self.stamp.clone()
    }
    pub fn execute(self, cancel: impl Fn() -> bool) -> BuiltGiSource {
        let mut builder = GiSourceBuilder {
            albedo: self.stamp.albedo.clone(),
            sky_ambient: self.stamp.sky,
            scratch: Box::new([GiVoxel::UNKNOWN; 4096]),
            updates: Vec::new(),
            entries: BTreeMap::new(),
            max_pages: 1,
            revision: 0,
            directional: self.stamp.directional.then(|| DirectionalScratch {
                page: Box::new([GiSurface::EMPTY; 4096]),
                updates: Vec::new(),
            }),
        };
        let result = builder
            .bake_from(&self.snapshot, self.stamp.page, &self.tints, cancel)
            .map(|work| GiBakedData {
                voxels: builder.scratch,
                surfaces: builder.directional.map(|s| s.page),
                work,
            });
        BuiltGiSource {
            id: self.stamp.id,
            page: self.stamp.page,
            inputs: self.stamp.inputs,
            epoch: self.stamp.epoch,
            result,
        }
    }
}
impl GiSourceBuilder {
    /// Seven Arc clones and certificates only: zero foreground cell reads.
    pub fn capture_full(
        &self,
        world: &World,
        page: PageId,
        tints: Box<[u16; 256]>,
        epoch: u64,
        id: u64,
    ) -> Result<GiBakeTask, GiSourceError> {
        if page.space != core_types::SpaceId(0) || id == 0 || id >= 1 << 63 {
            return Err(GiSourceError::InvalidPage);
        }
        if self.albedo.layers.len() > MAX_LAYERS {
            return Err(GiSourceError::InvalidMaterial);
        }
        let ids = input_pages(page)?;
        Ok(GiBakeTask {
            stamp: GiBakeStamp {
                id,
                page,
                epoch,
                inputs: ids.map(|p| world.capture_input(p, CHANNELS)),
                versions: ids.map(|p| world.page_version(p)),
                albedo: self.albedo.clone(),
                sky: self.sky_ambient,
                directional: self.directional.is_some(),
            },
            tints,
            snapshot: SnapshotRead {
                ids,
                pages: ids.map(|p| world.snapshot(p).map(InputPage::Native)),
            },
        })
    }
    /// Caller retains this immutable stamp separately from the worker payload.
    pub fn full_current(&self, world: &World, stamp: &GiBakeStamp, epoch: u64) -> bool {
        stamp.epoch == epoch
            && Arc::ptr_eq(&self.albedo, &stamp.albedo)
            && self.sky_ambient == stamp.sky
            && self.directional.is_some() == stamp.directional
            && world.matches_inputs(&stamp.inputs)
    }
    #[allow(clippy::too_many_arguments)]
    pub fn publish_full(
        &mut self,
        world: &World,
        stamp: &GiBakeStamp,
        built: &BuiltGiSource,
        epoch: u64,
        queue: &wgpu::Queue,
        gi: &mut GiGpu,
        tick: u64,
        budget: &mut UploadBudget,
    ) -> Result<SourceWork, GiSourceError> {
        if !self.full_current(world, stamp, epoch)
            || built.id != stamp.id
            || built.page != stamp.page
            || built.inputs != stamp.inputs
            || built.epoch != stamp.epoch
        {
            return Err(GiSourceError::StaleInput);
        }
        if self.directional.is_some() != gi.directional_enabled() {
            return Err(GiSourceError::InvalidMaterial);
        }
        if !self.entries.contains_key(&stamp.page) && self.entries.len() >= self.max_pages {
            return Err(GiSourceError::Capacity);
        }
        let data = built.result.as_ref().map_err(|error| *error)?;
        if data.surfaces.is_some() != stamp.directional {
            return Err(GiSourceError::InvalidMaterial);
        }
        let revision = self
            .revision
            .checked_add(1)
            .ok_or(GiSourceError::Revision)?;
        let mut work = data.work;
        work.upload_bytes = if let Some(surfaces) = &data.surfaces {
            gi.upload_page_directional(
                queue,
                [stamp.page.x, stamp.page.y, stamp.page.z],
                revision,
                &data.voxels,
                surfaces,
                tick,
                budget,
            )?
        } else {
            gi.upload_page(
                queue,
                [stamp.page.x, stamp.page.y, stamp.page.z],
                revision,
                &data.voxels,
                tick,
                budget,
            )?
        };
        self.entries.insert(
            stamp.page,
            Entry {
                inputs: stamp.inputs,
                epoch,
                revision,
                versions: stamp.inputs.map(|i| world.page_version(i.page)),
                dirty: [0; 128],
                force_full: false,
            },
        );
        self.revision = revision;
        Ok(work)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GiWireError {
    Length,
    Magic,
    Metadata,
    Material,
    Cell,
    Work,
}
impl std::fmt::Display for GiWireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for GiWireError {}
fn u32w(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn u64w(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn pagew(out: &mut Vec<u8>, page: PageId) {
    u32w(out, page.space.0);
    for v in [page.x, page.y, page.z] {
        u32w(out, v as u32);
    }
}
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], GiWireError> {
        let end = self.at.checked_add(N).ok_or(GiWireError::Length)?;
        let bytes = self
            .bytes
            .get(self.at..end)
            .ok_or(GiWireError::Length)?
            .try_into()
            .map_err(|_| GiWireError::Length)?;
        self.at = end;
        Ok(bytes)
    }
    fn u32(&mut self) -> Result<u32, GiWireError> {
        Ok(u32::from_le_bytes(self.take()?))
    }
    fn u64(&mut self) -> Result<u64, GiWireError> {
        Ok(u64::from_le_bytes(self.take()?))
    }
    fn byte(&mut self) -> Result<u8, GiWireError> {
        Ok(self.take::<1>()?[0])
    }
    fn float(&mut self) -> Result<f32, GiWireError> {
        Ok(f32::from_bits(self.u32()?))
    }
    fn page(&mut self) -> Result<PageId, GiWireError> {
        Ok(PageId {
            space: core_types::SpaceId(self.u32()?),
            x: self.u32()? as i32,
            y: self.u32()? as i32,
            z: self.u32()? as i32,
        })
    }
    fn inputs(&mut self, page: PageId) -> Result<[PageInput; 7], GiWireError> {
        let ids = input_pages(page).map_err(|_| GiWireError::Metadata)?;
        let mut inputs = ids.map(|page| PageInput {
            page,
            channels: CHANNELS,
            version: Version(0),
        });
        for input in &mut inputs {
            input.version = Version(self.u64()?);
        }
        Ok(inputs)
    }
    fn end(&self) -> Result<(), GiWireError> {
        if self.at == self.bytes.len() {
            Ok(())
        } else {
            Err(GiWireError::Length)
        }
    }
}
impl GiBakeTask {
    /// Same-origin Worker input: only already-known cells/light and locally
    /// prepared albedo averages. No generation seed, inventories or entities.
    pub fn encode(&self) -> Result<Vec<u8>, GiWireError> {
        let mut out = Vec::with_capacity(
            MAX_GI_REQUEST_BYTES.min(7 * 32768 + self.stamp.albedo.layers.len() * 24 + 1024),
        );
        out.extend_from_slice(b"CGJ1");
        u64w(&mut out, self.stamp.id);
        pagew(&mut out, self.stamp.page);
        u64w(&mut out, self.stamp.epoch);
        for input in self.stamp.inputs {
            u64w(&mut out, input.version.0);
        }
        for version in self.stamp.versions {
            u64w(&mut out, version.0);
        }
        u32w(&mut out, u32::from(self.stamp.directional));
        for value in self.stamp.sky {
            u32w(&mut out, value.to_bits());
        }
        u32w(&mut out, self.stamp.albedo.layers.len() as u32);
        for layer in &self.stamp.albedo.layers {
            for value in layer.linear.into_iter().chain(layer.masked) {
                u32w(&mut out, value.to_bits());
            }
        }
        for tint in self.tints.iter() {
            out.extend_from_slice(&tint.to_le_bytes());
        }
        for page in &self.snapshot.pages {
            out.push(u8::from(page.is_some()));
            if let Some(page) = page {
                for i in 0..4096 {
                    u32w(&mut out, page.cell(LocalIdx::new(i).unwrap()).raw());
                }
                for i in 0..4096 {
                    let light = page.light(LocalIdx::new(i).unwrap());
                    if light.sky > 15 || light.block > 15 {
                        return Err(GiWireError::Cell);
                    }
                    u32w(
                        &mut out,
                        u32::from(light.sky)
                            | (u32::from(light.block) << 4)
                            | (u32::from(light.rgb[0]) << 8)
                            | (u32::from(light.rgb[1]) << 16)
                            | (u32::from(light.rgb[2]) << 24),
                    );
                }
            }
        }
        if out.len() > MAX_GI_REQUEST_BYTES {
            return Err(GiWireError::Length);
        }
        Ok(out)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, GiWireError> {
        if bytes.len() > MAX_GI_REQUEST_BYTES {
            return Err(GiWireError::Length);
        }
        let mut r = Reader { bytes, at: 0 };
        if r.take::<4>()? != *b"CGJ1" {
            return Err(GiWireError::Magic);
        }
        let id = r.u64()?;
        let page = r.page()?;
        let epoch = r.u64()?;
        if id == 0 || id >= 1 << 63 || page.space != core_types::SpaceId(0) {
            return Err(GiWireError::Metadata);
        }
        let inputs = r.inputs(page)?;
        let mut versions = [Version(0); 7];
        for version in &mut versions {
            *version = Version(r.u64()?);
        }
        let directional = match r.u32()? {
            0 => false,
            1 => true,
            _ => return Err(GiWireError::Metadata),
        };
        let mut sky = [0.; 3];
        for channel in &mut sky {
            *channel = r.float()?;
            if !channel.is_finite() || !(0.0..=8.0).contains(channel) {
                return Err(GiWireError::Material);
            }
        }
        let count = r.u32()? as usize;
        if count == 0 || count > MAX_LAYERS {
            return Err(GiWireError::Material);
        }
        let mut layers = Vec::with_capacity(count);
        for _ in 0..count {
            let mut layer = Layer::default();
            for value in layer.linear.iter_mut().chain(layer.masked.iter_mut()) {
                *value = r.float()?;
                if !value.is_finite() || !(0.0..=1.0).contains(value) {
                    return Err(GiWireError::Material);
                }
            }
            if layer
                .masked
                .iter()
                .zip(layer.linear)
                .any(|(m, a)| *m > a + 0.000001)
            {
                return Err(GiWireError::Material);
            }
            layers.push(layer);
        }
        let mut tints = Box::new([0; 256]);
        for tint in tints.iter_mut() {
            *tint = u16::from_le_bytes(r.take()?);
        }
        let mut pages = std::array::from_fn(|_| None);
        for slot in &mut pages {
            match r.byte()? {
                0 => {}
                1 => {
                    let mut cells = Box::new([Cell::AIR; 4096]);
                    let mut lights = Box::new([LightSample::default(); 4096]);
                    for cell in cells.iter_mut() {
                        *cell = Cell::from_raw(r.u32()?);
                    }
                    for light in lights.iter_mut() {
                        let v = r.u32()?;
                        *light = LightSample {
                            sky: (v & 15) as u8,
                            block: ((v >> 4) & 15) as u8,
                            rgb: [(v >> 8) as u8, (v >> 16) as u8, (v >> 24) as u8],
                        };
                    }
                    *slot = Some(InputPage::Owned { cells, lights });
                }
                _ => return Err(GiWireError::Metadata),
            }
        }
        r.end()?;
        Ok(Self {
            stamp: GiBakeStamp {
                id,
                page,
                epoch,
                inputs,
                versions,
                albedo: Arc::new(GiAlbedo { layers }),
                sky,
                directional,
            },
            tints,
            snapshot: SnapshotRead {
                ids: input_pages(page).map_err(|_| GiWireError::Metadata)?,
                pages,
            },
        })
    }
    pub fn wire_samples(&self) -> u64 {
        self.snapshot.pages.iter().filter(|p| p.is_some()).count() as u64 * 4096 * 2
    }
}
impl BuiltGiSource {
    pub fn encode(&self) -> Result<Vec<u8>, GiWireError> {
        let mut out = Vec::with_capacity(self.result.as_ref().map_or(128, |v| {
            4096 * 4 + usize::from(v.surfaces.is_some()) * 4096 * 48 + 128
        }));
        out.extend_from_slice(b"CGR1");
        u64w(&mut out, self.id);
        pagew(&mut out, self.page);
        u64w(&mut out, self.epoch);
        for input in self.inputs {
            u64w(&mut out, input.version.0);
        }
        let data = match &self.result {
            Ok(data) => {
                out.push(0);
                data
            }
            Err(GiSourceError::Cancelled) => {
                out.push(1);
                return Ok(out);
            }
            Err(_) => {
                out.push(2);
                return Ok(out);
            }
        };
        out.push(u8::from(data.surfaces.is_some()));
        for value in [
            data.work.world_reads,
            u64::from(data.work.opaque_voxels),
            u64::from(data.work.reflected_faces),
            u64::from(data.work.unknown_neighbors),
        ] {
            u64w(&mut out, value);
        }
        for voxel in data.voxels.iter() {
            u32w(&mut out, voxel.packed());
        }
        if let Some(surfaces) = &data.surfaces {
            for surface in surfaces.iter() {
                for &word in surface.words() {
                    u32w(&mut out, word);
                }
            }
        }
        if out.len() > MAX_GI_RESULT_BYTES {
            return Err(GiWireError::Length);
        }
        Ok(out)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, GiWireError> {
        if bytes.len() > MAX_GI_RESULT_BYTES {
            return Err(GiWireError::Length);
        }
        let mut r = Reader { bytes, at: 0 };
        if r.take::<4>()? != *b"CGR1" {
            return Err(GiWireError::Magic);
        }
        let id = r.u64()?;
        let page = r.page()?;
        let epoch = r.u64()?;
        let inputs = r.inputs(page)?;
        if id == 0 || id >= 1 << 63 || page.space != core_types::SpaceId(0) {
            return Err(GiWireError::Metadata);
        }
        let status = r.byte()?;
        if status != 0 {
            r.end()?;
            let error = match status {
                1 => GiSourceError::Cancelled,
                2 => GiSourceError::InvalidMaterial,
                _ => return Err(GiWireError::Metadata),
            };
            return Ok(Self {
                id,
                page,
                inputs,
                epoch,
                result: Err(error),
            });
        }
        let directional = match r.byte()? {
            0 => false,
            1 => true,
            _ => return Err(GiWireError::Metadata),
        };
        let reads = r.u64()?;
        let opaque = r.u64()?;
        let faces = r.u64()?;
        let unknown = r.u64()?;
        if reads > 4096 * 7 || opaque > 4096 || faces > 4096 * 6 || unknown > 4096 * 6 {
            return Err(GiWireError::Work);
        }
        let mut voxels = Box::new([GiVoxel::UNKNOWN; 4096]);
        for voxel in voxels.iter_mut() {
            *voxel = GiVoxel::from_packed(r.u32()?).ok_or(GiWireError::Cell)?;
        }
        let surfaces = if directional {
            let mut data = Box::new([GiSurface::EMPTY; 4096]);
            for surface in data.iter_mut() {
                let mut words = [0; 12];
                for word in &mut words {
                    *word = r.u32()?;
                }
                *surface = GiSurface::from_words(words).ok_or(GiWireError::Cell)?;
            }
            Some(data)
        } else {
            None
        };
        r.end()?;
        Ok(Self {
            id,
            page,
            inputs,
            epoch,
            result: Ok(GiBakedData {
                voxels,
                surfaces,
                work: SourceWork {
                    world_reads: reads,
                    opaque_voxels: opaque as u32,
                    reflected_faces: faces as u32,
                    unknown_neighbors: unknown as u32,
                    ..Default::default()
                },
            }),
        })
    }
}
pub fn execute_gi_request(bytes: &[u8], cancel: impl Fn() -> bool) -> Result<Vec<u8>, GiWireError> {
    GiBakeTask::decode(bytes)?.execute(cancel).encode()
}
#[cfg(not(target_arch = "wasm32"))]
pub fn gi_native_pool(
    workers: usize,
    capacity: usize,
) -> Result<jobs::NativePool<GiBakeTask, BuiltGiSource>, jobs::PoolError> {
    jobs::NativePool::new(
        workers,
        capacity,
        |task: GiBakeTask, cancel: &jobs::CancellationToken| {
            Ok(task.execute(|| cancel.is_cancelled()))
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_types::{SpaceId, Tick};
    fn point(x: i32, y: i32, z: i32) -> BlockPos {
        BlockPos {
            space: SpaceId(0),
            x,
            y,
            z,
        }
    }
    fn builder(directional: bool) -> GiSourceBuilder {
        use render::terrain_gpu::AtlasMip;
        let albedo = GiAlbedo::from_atlas(&AtlasImage {
            tile_size: 1,
            layers: 186,
            mips: vec![AtlasMip {
                size: 1,
                rgba: vec![255; 186 * 4],
                tint_mask: vec![255; 186],
            }],
        })
        .unwrap();
        let mut builder = GiSourceBuilder::new(albedo, 8).unwrap();
        builder.set_sky_ambient([0.; 3]).unwrap();
        if directional {
            builder.enable_directional();
        }
        builder
    }
    fn world() -> World {
        let mut world = World::new(rules::properties, 128);
        let mut tx = world::Transaction::new(Tick(1), SpaceId(0));
        for page in input_pages(point(8, 8, 8).page()).unwrap() {
            tx.create_page(page, vec![Cell::AIR; 4096]);
        }
        tx.set_cell(point(8, 8, 8), Cell::from_parts(rules::ids::STONE, 0));
        for face in Face::ALL {
            let n = face.normal();
            tx.set_light(
                point(
                    8 + i32::from(n[0]),
                    8 + i32::from(n[1]),
                    8 + i32::from(n[2]),
                ),
                LightSample {
                    sky: 15,
                    block: 15,
                    rgb: [15, 0, 0],
                },
            );
        }
        world.commit(tx).unwrap();
        world
    }
    #[test]
    fn immutable_full_bake_and_both_wire_directions_preserve_exact_face_sources() {
        for directional in [false, true] {
            let mut world = world();
            let mut builder = builder(directional);
            let task = builder
                .capture_full(
                    &world,
                    point(8, 8, 8).page(),
                    Box::new([u16::MAX; 256]),
                    11,
                    1,
                )
                .unwrap();
            let stamp = task.stamp();
            let request = task.encode().unwrap();
            assert_eq!(request.len(), 234527);
            assert_eq!(task.wire_samples(), 57344);
            let mut tx = world::Transaction::new(Tick(2), SpaceId(0));
            tx.set_light(
                point(8, 9, 8),
                LightSample {
                    sky: 15,
                    block: 0,
                    rgb: [0, 0, 15],
                },
            );
            world.commit(tx).unwrap();
            let native = task.execute(|| false);
            let wire =
                BuiltGiSource::decode(&execute_gi_request(&request, || false).unwrap()).unwrap();
            let n = native.result.as_ref().unwrap();
            let w = wire.result.as_ref().unwrap();
            assert_eq!(n.voxels, w.voxels);
            assert_eq!(n.surfaces, w.surfaces);
            assert_eq!(n.work, w.work);
            assert_eq!(
                (
                    n.work.world_reads,
                    n.work.opaque_voxels,
                    n.work.reflected_faces
                ),
                (4102, 1, 6)
            );
            let index = usize::from(point(8, 8, 8).local_index().get());
            assert_eq!(
                n.voxels[index],
                GiVoxel::opaque_reflected([1., 0., 0.]).unwrap(),
                "snapshot retains original red source after live World changes"
            );
            if let Some(surfaces) = &n.surfaces {
                let mut expected = GiSurface::EMPTY;
                for face in Face::ALL {
                    expected = expected
                        .with_face(face as u8, [1.; 3], 15, [15, 0, 0])
                        .unwrap();
                }
                assert_eq!(surfaces[index], expected);
            }
            assert!(!builder.full_current(&world, &stamp, 11));
            assert!(!builder.full_current(&world, &stamp, 12));
            let task = builder
                .capture_full(&world, stamp.page, Box::new([u16::MAX; 256]), 11, 2)
                .unwrap();
            let stamp = task.stamp();
            assert!(builder.full_current(&world, &stamp, 11));
            let cancelled = task.execute(|| true);
            assert!(matches!(cancelled.result, Err(GiSourceError::Cancelled)));
            assert!(matches!(
                BuiltGiSource::decode(&cancelled.encode().unwrap())
                    .unwrap()
                    .result,
                Err(GiSourceError::Cancelled)
            ));
            builder
                .invalidate_materials(GiAlbedo::from_atlas(&AtlasImage::donor().unwrap()).unwrap());
            assert!(
                !builder.full_current(&world, &stamp, 11),
                "local material import invalidates a captured palette by identity"
            );
            assert!(GiBakeTask::decode(&request[..request.len() - 1]).is_err());
            let mut extra = request.clone();
            extra.push(0);
            assert_eq!(GiBakeTask::decode(&extra).err(), Some(GiWireError::Length));
            let mut bad = native.encode().unwrap();
            bad[126..130].copy_from_slice(&0x40000000u32.to_le_bytes());
            assert_eq!(BuiltGiSource::decode(&bad).err(), Some(GiWireError::Cell));
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn actual_native_source_worker_keeps_result_for_atomic_gpu_budget_retry() {
        use jobs::{Finish, JobKey, JobSpec, Priority, Take, WorkBudget, WorkCost};
        let world = world();
        let mut builder = builder(false);
        let task = builder
            .capture_full(
                &world,
                point(8, 8, 8).page(),
                Box::new([u16::MAX; 256]),
                1,
                7,
            )
            .unwrap();
        let stamp = task.stamp();
        let mut scheduler = jobs::Scheduler::new(jobs::Limits {
            max_jobs: 2,
            max_bytes: GI_RETAINED_BYTES * 2,
            max_age_ticks: 40,
            reserved_interactive_jobs: 0,
            reserved_interactive_bytes: 0,
        })
        .unwrap();
        scheduler
            .enqueue(
                JobSpec::derived(
                    JobKey {
                        page: stamp.page,
                        kind: 0x4749,
                    },
                    Priority::Interactive,
                    WorkCost {
                        cells: 28672,
                        bytes: GI_RETAINED_BYTES as u64,
                        ..WorkCost::ZERO
                    },
                    GI_RETAINED_BYTES,
                    Tick(1),
                ),
                task,
            )
            .map_err(|_| ())
            .unwrap();
        let mut budget = WorkBudget::unrestricted(WorkCost {
            cells: 28672,
            bytes: GI_RETAINED_BYTES as u64,
            ..WorkCost::ZERO
        });
        let Take::Ready(dispatch) = scheduler.take_next(Tick(1), &mut budget, |_| None) else {
            panic!("source task must fit declared logical budget")
        };
        let pool = gi_native_pool(2, 2).unwrap();
        pool.try_submit(dispatch).map_err(|_| ()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let completion = loop {
            if let Some(done) = pool.try_recv() {
                break done;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(1));
        };
        let Finish::Published(built) = scheduler.finish(completion, |_| None) else {
            panic!("fresh worker output")
        };
        let gpu =
            pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
        let mut gi = GiGpu::new(
            &gpu.device,
            &gpu.queue,
            render::gi::GiConfig {
                world_origin_page: [0; 3],
                world_pages: [1; 3],
                probe_counts: [1; 3],
                max_jobs: 1,
                ..Default::default()
            },
            &mut UploadBudget::new(4096),
        )
        .unwrap();
        assert_eq!(
            builder.publish_full(
                &world,
                &stamp,
                &built,
                1,
                &gpu.queue,
                &mut gi,
                1,
                &mut UploadBudget::new(16479)
            ),
            Err(GiSourceError::Gi(GiError::UploadBudget))
        );
        assert_eq!(gi.source_page_version([0; 3]).unwrap(), None);
        let work = builder
            .publish_full(
                &world,
                &stamp,
                &built,
                1,
                &gpu.queue,
                &mut gi,
                1,
                &mut UploadBudget::new(16480),
            )
            .unwrap();
        assert_eq!(work.upload_bytes, 16480);
        assert_eq!(work.world_reads, 4102);
        let reused = builder
            .update_page(
                &world,
                stamp.page,
                &[u16::MAX; 256],
                1,
                &gpu.queue,
                &mut gi,
                2,
                &mut SourceBudget {
                    remaining_world_reads: 0,
                },
                &mut UploadBudget::new(0),
            )
            .unwrap();
        assert!(reused.reused);
    }
}
