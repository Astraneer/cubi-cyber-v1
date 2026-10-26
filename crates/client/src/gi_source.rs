//! Local reflected-light source bridge. Unknown never generates light or terrain.
//! The scalar gameplay block channel and direct emitter radiance are excluded.
use core_types::{BlockPos, ChannelMask, LocalIdx, PageId, Version};
use render::{
    gi::{CellUpdate, GiError, GiGpu, GiVoxel},
    gi_directional::{GiSurface, SurfaceUpdate},
    terrain_gpu::{AtlasImage, UploadBudget},
    terrain_mesh::Face,
};
use std::{collections::BTreeMap, sync::Arc};
use world::{CellRead, PageInput, World, WorldChange};
mod worker;
#[cfg(not(target_arch = "wasm32"))]
pub use worker::gi_native_pool;
pub use worker::{
    BuiltGiSource, GI_RETAINED_BYTES, GiBakeStamp, GiBakeTask, GiBakedData, GiWireError,
    MAX_GI_REQUEST_BYTES, MAX_GI_RESULT_BYTES, execute_gi_request,
};
const CHANNELS: ChannelMask = ChannelMask(
    ChannelMask::SHAPE.0
        | ChannelMask::OPACITY.0
        | ChannelMask::LIGHTING.0
        | ChannelMask::EMISSION.0,
);
trait SourceRead {
    fn read(&self, position: BlockPos) -> CellRead;
    fn light(&self, position: BlockPos) -> Option<world::LightSample>;
}
impl SourceRead for World {
    fn read(&self, position: BlockPos) -> CellRead {
        World::read(self, position)
    }
    fn light(&self, position: BlockPos) -> Option<world::LightSample> {
        self.page(position.page())
            .map(|page| page.light(position.local_index()))
    }
}
#[derive(Clone, Copy, Debug, Default)]
struct Layer {
    linear: [f32; 3],
    masked: [f32; 3],
}
/// Texture averages are computed during local loading/import, in linear light.
/// Mask-weighted colors preserve grass metadata rather than tinting dirt faces.
pub struct GiAlbedo {
    layers: Vec<Layer>,
}
impl GiAlbedo {
    pub fn from_atlas(atlas: &AtlasImage) -> Result<Self, GiSourceError> {
        let mip = atlas.mips.first().ok_or(GiSourceError::InvalidMaterial)?;
        let pixels = (atlas.tile_size as usize)
            .checked_mul(atlas.tile_size as usize)
            .ok_or(GiSourceError::InvalidMaterial)?;
        let count = pixels
            .checked_mul(atlas.layers as usize)
            .ok_or(GiSourceError::InvalidMaterial)?;
        let rgba = count.checked_mul(4).ok_or(GiSourceError::InvalidMaterial)?;
        if pixels == 0
            || atlas.layers == 0
            || mip.size != atlas.tile_size
            || mip.rgba.len() != rgba
            || mip.tint_mask.len() != count
        {
            return Err(GiSourceError::InvalidMaterial);
        }
        let mut layers = Vec::with_capacity(atlas.layers as usize);
        for layer in 0..atlas.layers as usize {
            let mut result = Layer::default();
            let mut weight = 0.;
            for pixel in 0..pixels {
                let index = layer * pixels + pixel;
                let alpha = f32::from(mip.rgba[index * 4 + 3]) / 255.;
                let mask = f32::from(mip.tint_mask[index]) / 255.;
                weight += alpha;
                for channel in 0..3 {
                    let value = f32::from(mip.rgba[index * 4 + channel]) / 255.;
                    let linear = if value <= 0.04045 {
                        value / 12.92
                    } else {
                        libm::powf((value + 0.055) / 1.055, 2.4)
                    };
                    result.linear[channel] += linear * alpha;
                    result.masked[channel] += linear * alpha * mask;
                }
            }
            if weight > 0. {
                for channel in 0..3 {
                    result.linear[channel] /= weight;
                    result.masked[channel] /= weight;
                }
            }
            layers.push(result);
        }
        Ok(Self { layers })
    }
    fn face(
        &self,
        material: &render::terrain_mesh::CellMaterial,
        face: Face,
    ) -> Result<[f32; 3], GiSourceError> {
        let index = face as usize;
        let layer = self
            .layers
            .get(usize::from(material.face_materials[index]))
            .ok_or(GiSourceError::InvalidMaterial)?;
        if material.tint_faces & (1 << index) == 0 {
            return Ok(layer.linear);
        }
        let tint = [
            material.tint & 255,
            (material.tint >> 8) & 255,
            (material.tint >> 16) & 255,
        ]
        .map(|value| {
            let value = value as f32 / 255.;
            value * value
        });
        Ok(std::array::from_fn(|channel| {
            layer.linear[channel] + (tint[channel] - 1.) * layer.masked[channel]
        }))
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GiSourceError {
    InvalidMaterial,
    InvalidPage,
    Capacity,
    ReadBudget,
    StaleInput,
    Revision,
    Cancelled,
    Gi(GiError),
}
impl std::fmt::Display for GiSourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for GiSourceError {}
impl From<GiError> for GiSourceError {
    fn from(value: GiError) -> Self {
        Self::Gi(value)
    }
}
#[derive(Clone, Copy, Debug)]
pub struct SourceBudget {
    pub remaining_world_reads: u64,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SourceWork {
    pub world_reads: u64,
    pub opaque_voxels: u32,
    pub reflected_faces: u32,
    pub unknown_neighbors: u32,
    pub upload_bytes: u64,
    pub reused: bool,
    pub incremental: bool,
}
struct Entry {
    inputs: [PageInput; 7],
    epoch: u64,
    revision: u64,
    versions: [Version; 7],
    dirty: [u32; 128],
    force_full: bool,
}
/// One retained scratch page and bounded certificate map. Building a page reads
/// at most 4096 centers + six adjacent cells per center, and never scans a world.
pub struct GiSourceBuilder {
    albedo: Arc<GiAlbedo>,
    sky_ambient: [f32; 3],
    scratch: Box<[GiVoxel; 4096]>,
    updates: Vec<CellUpdate>,
    entries: BTreeMap<PageId, Entry>,
    max_pages: usize,
    revision: u64,
    directional: Option<DirectionalScratch>,
}
struct DirectionalScratch {
    page: Box<[GiSurface; 4096]>,
    updates: Vec<SurfaceUpdate>,
}
impl GiSourceBuilder {
    pub fn new(albedo: GiAlbedo, max_pages: usize) -> Result<Self, GiSourceError> {
        if max_pages == 0 || max_pages > 8192 {
            return Err(GiSourceError::Capacity);
        }
        Ok(Self {
            albedo: Arc::new(albedo),
            sky_ambient: [0.15; 3],
            scratch: Box::new([GiVoxel::UNKNOWN; 4096]),
            updates: Vec::with_capacity(4096),
            entries: BTreeMap::new(),
            max_pages,
            revision: 0,
            directional: None,
        })
    }
    /// Allocate the optional face source only during client loading.
    pub fn enable_directional(&mut self) {
        if self.directional.is_none() {
            self.directional = Some(DirectionalScratch {
                page: Box::new([GiSurface::EMPTY; 4096]),
                updates: Vec::with_capacity(4096),
            });
            self.entries.clear();
        }
    }
    pub fn forget(&mut self, page: PageId) {
        self.entries.remove(&page);
    }
    /// Explicit source model. A physical atmosphere profile uses RGB-only
    /// bounce until dynamic directional sky transport is supplied separately.
    pub fn set_sky_ambient(&mut self, rgb: [f32; 3]) -> Result<(), GiSourceError> {
        if rgb
            .iter()
            .any(|v| !v.is_finite() || !(0.0..=8.0).contains(v))
        {
            return Err(GiSourceError::InvalidMaterial);
        }
        if self.sky_ambient != rgb {
            self.sky_ambient = rgb;
            self.entries.clear();
        }
        Ok(())
    }
    pub fn invalidate_materials(&mut self, albedo: GiAlbedo) {
        self.albedo = Arc::new(albedo);
        self.entries.clear();
    }
    pub fn retained_pages(&self) -> usize {
        self.entries.len()
    }
    pub fn page_revision(&self, page: PageId) -> Option<u64> {
        self.entries.get(&page).map(|entry| entry.revision)
    }
    /// Call for each canonical commit. Missing or reordered commits cause a
    /// full rebuild, rather than advancing a certificate over incomplete data.
    pub fn mark_change(&mut self, change: &WorldChange) {
        if change.space != core_types::SpaceId(0) {
            return;
        }
        for changed in &change.pages {
            self.mark_page_change(changed);
        }
        for changed in &change.cells {
            self.mark_cell_change(changed);
        }
        for changed in &change.lights {
            self.mark_light_change(changed);
        }
    }
    /// Narrow row methods let an owner bound journal ingestion per frame.
    pub fn mark_page_change(&mut self, changed: &world::PageChange) {
        if changed.page.space != core_types::SpaceId(0) {
            return;
        }
        let Ok(sources) = input_pages(changed.page) else {
            return;
        };
        for page in sources {
            if let Some(entry) = self.entries.get_mut(&page) {
                for (index, input) in entry.inputs.iter().enumerate() {
                    if input.page == changed.page {
                        if entry.versions[index] != changed.before
                            || changed.created
                            || changed.removed
                        {
                            entry.force_full = true;
                        }
                        entry.versions[index] = changed.after;
                    }
                }
            }
        }
    }
    pub fn mark_cell_change(&mut self, changed: &world::CellChange) {
        self.mark_owners(changed.page, changed.index);
    }
    pub fn mark_light_change(&mut self, changed: &world::LightChange) {
        if changed.old.rgb != changed.new.rgb || changed.old.sky != changed.new.sky {
            self.mark_owners(changed.page, changed.index);
        }
    }
    pub fn invalidate_sources(&mut self) {
        self.entries.clear();
    }
    pub fn retain_aoi(&mut self, origin: [i32; 3], counts: [u32; 3]) {
        self.entries.retain(|page, _| {
            [page.x, page.y, page.z]
                .into_iter()
                .enumerate()
                .all(|(axis, value)| {
                    (0..i64::from(counts[axis]))
                        .contains(&(i64::from(value) - i64::from(origin[axis])))
                })
        });
    }
    fn mark_owners(&mut self, page: PageId, index: LocalIdx) {
        let Ok(center) = position_of(page, index) else {
            return;
        };
        self.mark_owner(center);
        for face in Face::ALL {
            let n = face.normal();
            if let (Some(x), Some(y), Some(z)) = (
                center.x.checked_add(i32::from(n[0])),
                center.y.checked_add(i32::from(n[1])),
                center.z.checked_add(i32::from(n[2])),
            ) {
                self.mark_owner(BlockPos {
                    space: center.space,
                    x,
                    y,
                    z,
                });
            }
        }
    }
    fn mark_owner(&mut self, position: BlockPos) {
        if let Some(entry) = self.entries.get_mut(&position.page()) {
            let index = position.local_index().get();
            entry.dirty[usize::from(index) / 32] |= 1u32 << (index % 32);
        }
    }
    /// CPU RGB/skylight are normalized diffuse terms in M4. Therefore their
    /// physical irradiance is PI*term and reflected outgoing L=albedo*term;
    /// applying /PI here again would dim the source twice. Directional sunlight
    /// is intentionally omitted until a shadow-certified source is supplied.
    #[allow(clippy::too_many_arguments)]
    pub fn update_page(
        &mut self,
        world: &World,
        page: PageId,
        tints: &[u16; 256],
        epoch: u64,
        queue: &wgpu::Queue,
        gi: &mut GiGpu,
        tick: u64,
        read_budget: &mut SourceBudget,
        upload_budget: &mut UploadBudget,
    ) -> Result<SourceWork, GiSourceError> {
        if page.space != core_types::SpaceId(0) {
            return Err(GiSourceError::InvalidPage);
        }
        if gi.directional_enabled() != self.directional.is_some() {
            return Err(GiSourceError::InvalidMaterial);
        }
        let pages = input_pages(page)?;
        let inputs = pages.map(|page| world.capture_input(page, CHANNELS));
        let gpu_version = gi.source_page_version([page.x, page.y, page.z])?;
        let versions = pages.map(|page| world.page_version(page));
        let exact = self.entries.get(&page).is_some_and(|entry| {
            entry.inputs == inputs && entry.epoch == epoch && gpu_version == Some(entry.revision)
        });
        let certified = self.entries.get(&page).is_some_and(|entry| {
            entry.versions == versions
                && !entry.force_full
                && entry.epoch == epoch
                && gpu_version == Some(entry.revision)
        });
        let dirty = self
            .entries
            .get(&page)
            .map_or([0; 128], |entry| entry.dirty);
        if exact || (certified && dirty.iter().all(|&word| word == 0)) {
            let entry = self.entries.get_mut(&page).expect("existing certificate");
            entry.inputs = inputs;
            entry.versions = versions;
            entry.force_full = false;
            entry.dirty = [0; 128];
            return Ok(SourceWork {
                reused: true,
                ..SourceWork::default()
            });
        }
        if !self.entries.contains_key(&page) && self.entries.len() >= self.max_pages {
            return Err(GiSourceError::Capacity);
        }
        let owners = if certified {
            dirty.iter().map(|word| word.count_ones()).sum::<u32>()
        } else {
            4096
        };
        if read_budget.remaining_world_reads < u64::from(owners) * 7 {
            return Err(GiSourceError::ReadBudget);
        }
        let revision = self
            .revision
            .checked_add(1)
            .ok_or(GiSourceError::Revision)?;
        let mut work = if certified {
            self.updates.clear();
            if let Some(directional) = &mut self.directional {
                directional.updates.clear();
            }
            let mut work = SourceWork {
                incremental: true,
                ..SourceWork::default()
            };
            for (word, &mask) in dirty.iter().enumerate() {
                let mut bits = mask;
                while bits != 0 {
                    let index = (word as u16) * 32 + bits.trailing_zeros() as u16;
                    bits &= bits - 1;
                    let local = LocalIdx::new(index).expect("owner mask index");
                    let (x, _, z) = local.xyz();
                    let (value, surface) = self.sample_voxel(
                        world,
                        position_of(page, local)?,
                        tints[usize::from(x) + 16 * usize::from(z)],
                        &mut work,
                    )?;
                    self.updates.push(CellUpdate {
                        local: index,
                        value,
                    });
                    if let Some(directional) = &mut self.directional {
                        directional.updates.push(SurfaceUpdate {
                            local: index,
                            value: surface,
                        });
                    }
                }
            }
            work
        } else {
            self.bake(world, page, tints)?
        };
        read_budget.remaining_world_reads -= work.world_reads;
        if !world.matches_inputs(&inputs) {
            return Err(GiSourceError::StaleInput);
        }
        work.upload_bytes = if let Some(directional) = &self.directional {
            if certified {
                gi.upload_cells_directional(
                    queue,
                    [page.x, page.y, page.z],
                    revision,
                    &self.updates,
                    &directional.updates,
                    tick,
                    upload_budget,
                )?
            } else {
                gi.upload_page_directional(
                    queue,
                    [page.x, page.y, page.z],
                    revision,
                    &self.scratch,
                    &directional.page,
                    tick,
                    upload_budget,
                )?
            }
        } else if certified {
            gi.upload_cells(
                queue,
                [page.x, page.y, page.z],
                revision,
                &self.updates,
                tick,
                upload_budget,
            )?
        } else {
            gi.upload_page(
                queue,
                [page.x, page.y, page.z],
                revision,
                &self.scratch,
                tick,
                upload_budget,
            )?
        };
        self.entries.insert(
            page,
            Entry {
                inputs,
                epoch,
                revision,
                versions,
                dirty: [0; 128],
                force_full: false,
            },
        );
        self.revision = revision;
        Ok(work)
    }
    #[allow(clippy::too_many_arguments)]
    pub fn update_scene_page(
        &mut self,
        scene: &crate::ClientScene,
        page: PageId,
        epoch: u64,
        queue: &wgpu::Queue,
        gi: &mut GiGpu,
        tick: u64,
        read_budget: &mut SourceBudget,
        upload_budget: &mut UploadBudget,
    ) -> Result<SourceWork, GiSourceError> {
        let tints = std::array::from_fn(|index| {
            scene
                .biome_tint(page, (index % 16) as u8, (index / 16) as u8)
                .unwrap_or(u16::MAX)
        });
        self.update_page(
            scene.world(),
            page,
            &tints,
            epoch,
            queue,
            gi,
            tick,
            read_budget,
            upload_budget,
        )
    }
    fn bake(
        &mut self,
        world: &World,
        page: PageId,
        tints: &[u16; 256],
    ) -> Result<SourceWork, GiSourceError> {
        self.bake_from(world, page, tints, || false)
    }
    fn bake_from(
        &mut self,
        world: &impl SourceRead,
        page: PageId,
        tints: &[u16; 256],
        cancel: impl Fn() -> bool,
    ) -> Result<SourceWork, GiSourceError> {
        let mut work = SourceWork::default();
        for index in 0..4096 {
            if index % 64 == 0 && cancel() {
                return Err(GiSourceError::Cancelled);
            }
            let local = LocalIdx::new(index as u16).expect("page index");
            let (x, _, z) = local.xyz();
            let (value, surface) = self.sample_voxel(
                world,
                position_of(page, local)?,
                tints[usize::from(x) + 16 * usize::from(z)],
                &mut work,
            )?;
            self.scratch[index] = value;
            if let Some(directional) = &mut self.directional {
                directional.page[index] = surface;
            }
        }
        Ok(work)
    }
    fn sample_voxel(
        &self,
        world: &impl SourceRead,
        position: BlockPos,
        tint: u16,
        work: &mut SourceWork,
    ) -> Result<(GiVoxel, GiSurface), GiSourceError> {
        work.world_reads += 1;
        let CellRead::Known(cell) = world.read(position) else {
            return Ok((GiVoxel::UNKNOWN, GiSurface::EMPTY));
        };
        if rules::block(cell.id()).is_none() {
            return Ok((GiVoxel::UNKNOWN, GiSurface::EMPTY));
        }
        let properties = rules::properties(cell);
        if !properties.opaque {
            return Ok((GiVoxel::EMPTY, GiSurface::EMPTY));
        }
        work.opaque_voxels += 1;
        if properties.emitter() {
            return Ok((
                GiVoxel::opaque_reflected([0.; 3]).expect("zero reflection"),
                GiSurface::EMPTY,
            ));
        }
        let material = crate::material(cell.raw(), tint);
        let mut radiance = [0.; 3];
        let mut visible = 0;
        let mut surface = GiSurface::EMPTY;
        for face in Face::ALL {
            let n = face.normal();
            let neighbor = BlockPos {
                space: position.space,
                x: position
                    .x
                    .checked_add(i32::from(n[0]))
                    .ok_or(GiSourceError::InvalidPage)?,
                y: position
                    .y
                    .checked_add(i32::from(n[1]))
                    .ok_or(GiSourceError::InvalidPage)?,
                z: position
                    .z
                    .checked_add(i32::from(n[2]))
                    .ok_or(GiSourceError::InvalidPage)?,
            };
            work.world_reads += 1;
            let CellRead::Known(neighbor_cell) = world.read(neighbor) else {
                work.unknown_neighbors += 1;
                continue;
            };
            if rules::block(neighbor_cell.id()).is_none() {
                work.unknown_neighbors += 1;
                continue;
            }
            if rules::properties(neighbor_cell).opaque {
                continue;
            }
            let light = world.light(neighbor).ok_or(GiSourceError::StaleInput)?;
            let albedo = self.albedo.face(&material, face)?;
            if self.directional.is_some() {
                surface = surface
                    .with_face(face as u8, albedo, light.sky, light.rgb)
                    .ok_or(GiSourceError::InvalidMaterial)?;
            }
            visible += 1;
            work.reflected_faces += 1;
            for channel in 0..3 {
                radiance[channel] += albedo[channel]
                    * (factor(light.rgb[channel]) + self.sky_ambient[channel] * factor(light.sky));
            }
        }
        if visible > 0 {
            for channel in &mut radiance {
                *channel /= visible as f32;
            }
        }
        // Compact first-bounce cache: isotropic face average, no fake sun term.
        GiVoxel::opaque_reflected(radiance)
            .map(|voxel| (voxel, surface))
            .ok_or(GiSourceError::InvalidMaterial)
    }
}
fn position_of(page: PageId, index: LocalIdx) -> Result<BlockPos, GiSourceError> {
    let (x, y, z) = index.xyz();
    Ok(BlockPos {
        space: page.space,
        x: page
            .x
            .checked_mul(16)
            .and_then(|v| v.checked_add(i32::from(x)))
            .ok_or(GiSourceError::InvalidPage)?,
        y: page
            .y
            .checked_mul(16)
            .and_then(|v| v.checked_add(i32::from(y)))
            .ok_or(GiSourceError::InvalidPage)?,
        z: page
            .z
            .checked_mul(16)
            .and_then(|v| v.checked_add(i32::from(z)))
            .ok_or(GiSourceError::InvalidPage)?,
    })
}

fn factor(level: u8) -> f32 {
    if level == 0 {
        0.
    } else if level > 15 {
        f32::from(level) / 15.
    } else {
        libm::powf(0.8, 15. - f32::from(level))
    }
}
fn input_pages(page: PageId) -> Result<[PageId; 7], GiSourceError> {
    let mut pages = [page; 7];
    for (face, index) in Face::ALL.into_iter().zip(1..) {
        let n = face.normal();
        pages[index] = PageId {
            space: page.space,
            x: page
                .x
                .checked_add(i32::from(n[0]))
                .ok_or(GiSourceError::InvalidPage)?,
            y: page
                .y
                .checked_add(i32::from(n[1]))
                .ok_or(GiSourceError::InvalidPage)?,
            z: page
                .z
                .checked_add(i32::from(n[2]))
                .ok_or(GiSourceError::InvalidPage)?,
        };
    }
    Ok(pages)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_types::{Cell, SpaceId, Tick};
    use render::terrain_gpu::AtlasMip;
    use world::{LightSample, Transaction};
    fn white_albedo() -> GiAlbedo {
        GiAlbedo::from_atlas(&AtlasImage {
            tile_size: 1,
            layers: 186,
            mips: vec![AtlasMip {
                size: 1,
                rgba: vec![255; 186 * 4],
                tint_mask: vec![255; 186],
            }],
        })
        .unwrap()
    }
    fn position() -> BlockPos {
        BlockPos {
            space: SpaceId(0),
            x: 8,
            y: 8,
            z: 8,
        }
    }
    fn source_world() -> World {
        let mut world = World::new(rules::properties, 64);
        let mut cells = vec![Cell::AIR; 4096];
        cells[usize::from(position().local_index().get())] =
            Cell::from_parts(rules::block_by_name("stone").unwrap().0, 0);
        let mut tx = Transaction::new(Tick(1), SpaceId(0));
        tx.create_page(position().page(), cells);
        world.commit(tx).unwrap();
        world
    }
    fn light_neighbors(world: &mut World, light: LightSample, tick: u64) {
        let p = position();
        let mut tx = Transaction::new(Tick(tick), p.space);
        for face in Face::ALL {
            let n = face.normal();
            tx.set_light(
                BlockPos {
                    space: p.space,
                    x: p.x + i32::from(n[0]),
                    y: p.y + i32::from(n[1]),
                    z: p.z + i32::from(n[2]),
                },
                light,
            );
        }
        world.commit(tx).unwrap();
    }
    #[test]
    fn legal_rgb8_hdr_and_maximum_sky_ambient_fit_the_reflection_cache() {
        let mut world = source_world();
        let mut builder = GiSourceBuilder::new(white_albedo(), 4).unwrap();
        builder.set_sky_ambient([8.; 3]).unwrap();
        light_neighbors(
            &mut world,
            LightSample {
                sky: 15,
                block: 15,
                rgb: [231, 17, 255],
            },
            2,
        );
        builder
            .bake(&world, position().page(), &[u16::MAX; 256])
            .unwrap();
        let reflected = builder.scratch[usize::from(position().local_index().get())].reflected();
        let expected = [231. / 15. + 8., 17. / 15. + 8., 255. / 15. + 8.];
        for c in 0..3 {
            assert!(
                (reflected[c] - expected[c]).abs() <= render::gi::MAX_REFLECTED_RADIANCE / 2046.,
                "HDR source {reflected:?} != {expected:?}"
            );
        }
        assert!(
            reflected[0] > 23. && reflected[2] > 24.9,
            "legal HDR must not saturate at old8-unit limit"
        );
    }
    #[test]
    fn directional_records_preserve_faces_and_rgb8_and_exclude_gameplay_scalar() {
        let mut world = source_world();
        let light = LightSample {
            sky: 12,
            block: 15,
            rgb: [231, 17, 255],
        };
        light_neighbors(&mut world, light, 2);
        let mut builder = GiSourceBuilder::new(white_albedo(), 4).unwrap();
        builder.set_sky_ambient([0.; 3]).unwrap();
        builder.enable_directional();
        let work = builder
            .bake(&world, position().page(), &[u16::MAX; 256])
            .unwrap();
        assert_eq!(work.world_reads, 4102);
        let mut expected = GiSurface::EMPTY;
        for face in Face::ALL {
            expected = expected
                .with_face(face as u8, [1.; 3], 12, light.rgb)
                .unwrap();
        }
        assert_eq!(
            builder.directional.as_ref().unwrap().page[usize::from(position().local_index().get())],
            expected
        );
        assert_eq!(std::mem::size_of::<GiSurface>(), 48);
        let mut tx = Transaction::new(Tick(3), SpaceId(0));
        tx.set_cell(
            BlockPos {
                y: position().y + 1,
                ..position()
            },
            Cell::from_parts(rules::ids::STONE, 0),
        );
        world.commit(tx).unwrap();
        builder
            .bake(&world, position().page(), &[u16::MAX; 256])
            .unwrap();
        let mut expected = GiSurface::EMPTY;
        for face in Face::ALL {
            if face != Face::Up {
                expected = expected
                    .with_face(face as u8, [1.; 3], 12, light.rgb)
                    .unwrap();
            }
        }
        assert_eq!(
            builder.directional.as_ref().unwrap().page[usize::from(position().local_index().get())],
            expected,
            "covered face cannot receive another surface's sky"
        );
    }
    #[test]
    fn source_uses_linear_masked_albedo_and_excludes_unknown_scalar_and_emitters() {
        let mut world = source_world();
        let mut builder = GiSourceBuilder::new(white_albedo(), 4).unwrap();
        light_neighbors(
            &mut world,
            LightSample {
                sky: 0,
                block: 15,
                rgb: [15, 0, 0],
            },
            2,
        );
        let work = builder
            .bake(&world, position().page(), &[u16::MAX; 256])
            .unwrap();
        assert_eq!(
            (work.opaque_voxels, work.reflected_faces, work.world_reads),
            (1, 6, 4102)
        );
        assert_eq!(
            builder.scratch[usize::from(position().local_index().get())],
            GiVoxel::opaque_reflected([1., 0., 0.]).unwrap()
        );
        light_neighbors(
            &mut world,
            LightSample {
                sky: 0,
                block: 15,
                rgb: [0; 3],
            },
            3,
        );
        builder
            .bake(&world, position().page(), &[u16::MAX; 256])
            .unwrap();
        assert_eq!(
            builder.scratch[usize::from(position().local_index().get())],
            GiVoxel::opaque_reflected([0.; 3]).unwrap()
        );
        let mut tx = Transaction::new(Tick(4), SpaceId(0));
        tx.set_cell(
            position(),
            rules::all_blocks()
                .map(|(id, _)| Cell::from_parts(id, rules::blocks::ST_LIT))
                .find(|&cell| {
                    let p = rules::properties(cell);
                    p.opaque && p.emitter()
                })
                .expect("registry has an opaque lit emitter"),
        );
        world.commit(tx).unwrap();
        light_neighbors(
            &mut world,
            LightSample {
                sky: 15,
                block: 15,
                rgb: [15; 3],
            },
            5,
        );
        builder
            .bake(&world, position().page(), &[u16::MAX; 256])
            .unwrap();
        assert_eq!(
            builder.scratch[usize::from(position().local_index().get())],
            GiVoxel::opaque_reflected([0.; 3]).unwrap(),
            "direct emission is already M4's separate term"
        );
        builder
            .bake(
                &world,
                PageId {
                    space: SpaceId(0),
                    x: 1,
                    y: 0,
                    z: 0,
                },
                &[u16::MAX; 256],
            )
            .unwrap();
        assert!(
            builder
                .scratch
                .iter()
                .all(|&voxel| voxel == GiVoxel::UNKNOWN)
        );
        let palette = white_albedo();
        let material = render::terrain_mesh::CellMaterial {
            tint: 0xff40_ff80,
            tint_faces: 1 << 1,
            ..render::terrain_mesh::CellMaterial::opaque(0)
        };
        assert_eq!(palette.face(&material, Face::Down).unwrap(), [1.; 3]);
        let tinted = palette.face(&material, Face::Up).unwrap();
        assert!(
            (tinted[0] - (128f32 / 255.).powi(2)).abs() < 1e-6
                && tinted[1] == 1.
                && (tinted[2] - (64f32 / 255.).powi(2)).abs() < 1e-6
        );
    }
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn world_light_certificate_updates_actual_gpu_reflection_and_static_reuses_zero_work() {
        use render::gi::GiConfig;
        let gpu =
            pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
        let mut world = source_world();
        light_neighbors(
            &mut world,
            LightSample {
                sky: 0,
                block: 15,
                rgb: [15, 0, 0],
            },
            2,
        );
        let mut builder = GiSourceBuilder::new(white_albedo(), 4).unwrap();
        let mut upload = UploadBudget::new(1_000_000);
        let mut gi = GiGpu::new(
            &gpu.device,
            &gpu.queue,
            GiConfig {
                world_origin_page: [0; 3],
                world_pages: [1; 3],
                probe_origin: [8.5, 9.5, 8.5],
                probe_counts: [1; 3],
                max_jobs: 1,
                ..GiConfig::default()
            },
            &mut upload,
        )
        .unwrap();
        let page = position().page();
        let mut budget = SourceBudget {
            remaining_world_reads: 100_000,
        };
        let first = builder
            .update_page(
                &world,
                page,
                &[u16::MAX; 256],
                0,
                &gpu.queue,
                &mut gi,
                0,
                &mut budget,
                &mut upload,
            )
            .unwrap();
        assert_eq!((first.world_reads, first.upload_bytes), (4102, 16480));
        let read_probe = |gi: &mut GiGpu, tick| {
            let mut encoder = gpu.device.create_command_encoder(&Default::default());
            assert_eq!(
                gi.trace_tick(
                    &gpu.queue,
                    &mut encoder,
                    tick,
                    &mut UploadBudget::new(1024),
                    &mut 1000
                )
                .updated_probes,
                1
            );
            let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("actual World→GI reflection oracle"),
                size: 128,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            encoder.copy_buffer_to_buffer(gi.probe_buffer(), 0, &buffer, 0, 128);
            gpu.queue.submit([encoder.finish()]);
            let (tx, rx) = std::sync::mpsc::channel();
            buffer
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
            gpu.device.poll(wgpu::PollType::Wait).unwrap();
            rx.recv().unwrap().unwrap();
            let raw = buffer.slice(..).get_mapped_range().to_vec();
            buffer.unmap();
            raw
        };
        let red = read_probe(&mut gi, 0);
        let coefficient =
            |raw: &[u8], offset| f32::from_le_bytes(raw[offset..offset + 4].try_into().unwrap());
        assert!(coefficient(&red, 16) > 0.);
        assert_eq!(coefficient(&red, 48), 0.);
        let cached = builder
            .update_page(
                &world,
                page,
                &[u16::MAX; 256],
                0,
                &gpu.queue,
                &mut gi,
                1,
                &mut SourceBudget {
                    remaining_world_reads: 0,
                },
                &mut UploadBudget::new(0),
            )
            .unwrap();
        assert!(cached.reused);
        assert_eq!((cached.world_reads, cached.upload_bytes), (0, 0));
        let old_revision = builder.page_revision(page).unwrap();
        light_neighbors(
            &mut world,
            LightSample {
                sky: 0,
                block: 15,
                rgb: [0, 0, 15],
            },
            3,
        );
        let next = builder
            .update_page(
                &world,
                page,
                &[u16::MAX; 256],
                0,
                &gpu.queue,
                &mut gi,
                1,
                &mut budget,
                &mut upload,
            )
            .unwrap();
        assert_eq!(next.world_reads, 4102);
        assert!(builder.page_revision(page).unwrap() > old_revision);
        let blue = read_probe(&mut gi, 1);
        assert_eq!(coefficient(&blue, 16), 0.);
        assert!(coefficient(&blue, 48) > 0.);
        let above = BlockPos {
            y: position().y + 1,
            ..position()
        };
        let mut scalar = Transaction::new(Tick(4), above.space);
        scalar.set_light(
            above,
            LightSample {
                sky: 0,
                block: 7,
                rgb: [0, 0, 15],
            },
        );
        builder.mark_change(&world.commit(scalar).unwrap());
        let scalar = builder
            .update_page(
                &world,
                page,
                &[u16::MAX; 256],
                0,
                &gpu.queue,
                &mut gi,
                2,
                &mut SourceBudget {
                    remaining_world_reads: 0,
                },
                &mut UploadBudget::new(0),
            )
            .unwrap();
        assert!(scalar.reused && scalar.upload_bytes == 0 && scalar.world_reads == 0);
        let mut changed = Transaction::new(Tick(5), above.space);
        changed.set_light(
            above,
            LightSample {
                sky: 0,
                block: 7,
                rgb: [15, 0, 0],
            },
        );
        builder.mark_change(&world.commit(changed).unwrap());
        let patch = builder
            .update_page(
                &world,
                page,
                &[u16::MAX; 256],
                0,
                &gpu.queue,
                &mut gi,
                2,
                &mut SourceBudget {
                    remaining_world_reads: 49,
                },
                &mut upload,
            )
            .unwrap();
        assert!(patch.incremental);
        assert_eq!((patch.world_reads, patch.upload_bytes), (13, 124));
        let patched = read_probe(&mut gi, 2);
        let mut canonical = GiGpu::new(
            &gpu.device,
            &gpu.queue,
            GiConfig {
                world_origin_page: [0; 3],
                world_pages: [1; 3],
                probe_origin: [8.5, 9.5, 8.5],
                probe_counts: [1; 3],
                max_jobs: 1,
                ..GiConfig::default()
            },
            &mut upload,
        )
        .unwrap();
        let mut fresh = GiSourceBuilder::new(white_albedo(), 4).unwrap();
        fresh
            .update_page(
                &world,
                page,
                &[u16::MAX; 256],
                0,
                &gpu.queue,
                &mut canonical,
                2,
                &mut budget,
                &mut upload,
            )
            .unwrap();
        let full = read_probe(&mut canonical, 2);
        assert_eq!(
            &patched[16..112],
            &full[16..112],
            "incremental sources must produce bitwise full-source irradiance and visibility"
        );
        let mut missing = Transaction::new(Tick(6), above.space);
        missing.set_light(
            above,
            LightSample {
                sky: 0,
                block: 7,
                rgb: [0, 15, 0],
            },
        );
        world.commit(missing).unwrap();
        assert_eq!(
            builder.update_page(
                &world,
                page,
                &[u16::MAX; 256],
                0,
                &gpu.queue,
                &mut gi,
                3,
                &mut SourceBudget {
                    remaining_world_reads: 49
                },
                &mut upload
            ),
            Err(GiSourceError::ReadBudget),
            "missing journal event must force a full rebuild"
        );
        let fallback = builder
            .update_page(
                &world,
                page,
                &[u16::MAX; 256],
                0,
                &gpu.queue,
                &mut gi,
                3,
                &mut budget,
                &mut upload,
            )
            .unwrap();
        assert!(!fallback.incremental);
        assert_eq!(fallback.world_reads, 4102);
        gi.recenter(&gpu.queue, [1, 0, 0], [0; 3], 2, &mut upload)
            .unwrap();
        gi.recenter(&gpu.queue, [0; 3], [0; 3], 3, &mut upload)
            .unwrap();
        assert_eq!(gi.source_page_version([0; 3]).unwrap(), None);
        let recycled = builder
            .update_page(
                &world,
                page,
                &[u16::MAX; 256],
                0,
                &gpu.queue,
                &mut gi,
                3,
                &mut budget,
                &mut upload,
            )
            .unwrap();
        assert_eq!(
            (recycled.world_reads, recycled.upload_bytes),
            (4102, 16480),
            "a CPU-cached page must repopulate a recycled GPU source slot"
        );
        println!(
            "GI_WORLD_BRIDGE_GPU_OK reflected_faces=6 cell_reads=4102 upload_bytes=16480 red_to_blue=true scalar_block15_excluded=true static_reads=0 static_upload_bytes=0 source_version_certificate=true recycled_GPU_slot_repopulated=true incremental_reads=13 incremental_upload_bytes=124 scalar_only_reads=0 missing_journal_forces_full=true"
        );
    }
}
