//! Loading-only attachment and per-frame budgets for the shared lighting owner.
//! Registration follows successful GPU publication, including both H1 handles.
use crate::{
    ClientScene,
    lighting::{
        ClientLighting, ClientLightingConfig, ClientLightingError, ClientLightingWork, LightSource,
    },
};
use core_types::{BlockPos, PageId, Tick};
use render::{
    shadows::{DynamicShadowCaster, ShadowWork, WorldBounds},
    terrain_gpu::{PageHandle, TerrainGpu, UploadBudget},
};
use world::lighting::LightBudget;
pub const SUN: [f32; 3] = [0.35, 0.8, 0.2];
#[derive(Clone, Copy, Debug, Default)]
pub struct LightingStats {
    pub mask: crate::lighting::LightMaskDiagnostics,
    pub journal_entries: u64,
    pub coalesced_light_changes: u64,
    pub cpu_cells: u64,
    pub neighbors: u64,
    pub upload_bytes: u64,
    pub shadow_draws: u64,
    pub static_shadow_draws: u64,
    pub dynamic_shadow_draws: u64,
    pub dynamic_shadow_copy_bytes: u64,
    pub dynamic_shadow_restore_pixels: u64,
    pub dynamic_shadow_restore_draws: u64,
    pub shadow_updates: u64,
    pub shadow_texels: u64,
    /// Sparse areas are measured by the GPU; these are request counts/bounds,
    /// separate from the CPU-known `shadow_texels` and restore pixel counts.
    pub shadow_gpu_clear_updates: u64,
    pub shadow_gpu_restore_updates: u64,
    pub shadow_gpu_clear_upper_bound: u64,
    pub shadow_gpu_restore_upper_bound: u64,
    pub pending_shadow_cascades: u32,
    pub pending_cells: usize,
    pub pending_pages: usize,
    pub oldest_age_ticks: u64,
    pub overdue: bool,
    pub ready: bool,
    pub authoritative_source: bool,
    pub pending_source_pages: usize,
}
pub struct ViewLighting {
    pub owner: ClientLighting,
    pub stats: LightingStats,
}
impl ViewLighting {
    /// Config is explicit: standalone may prove GeneratedProfile, remote uses
    /// UnknownIsDark until an authoritative sky-boundary protocol exists.
    pub fn prepare(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        terrain: &mut TerrainGpu,
        scene: &mut ClientScene,
        config: ClientLightingConfig,
        center: [f32; 3],
    ) -> Result<Self, ClientLightingError> {
        Ok(Self {
            owner: ClientLighting::prepare_scene(
                device, queue, terrain, scene, config, center, SUN,
            )?,
            stats: LightingStats {
                authoritative_source: config.source == LightSource::Authoritative,
                ..LightingStats::default()
            },
        })
    }
    pub fn register_base(
        &mut self,
        page: PageId,
        handle: PageHandle,
    ) -> Result<(), ClientLightingError> {
        self.owner.register_page(page, handle, page_bounds(page)?)
    }
    pub fn register_overlay(
        &mut self,
        queue: &wgpu::Queue,
        page: PageId,
        handle: PageHandle,
        budget: &mut UploadBudget,
    ) -> Result<(), ClientLightingError> {
        self.owner.register_overlay(queue, page, handle, budget)
    }
    pub fn register_rigid(
        &mut self,
        handle: PageHandle,
        position: BlockPos,
        bounds: WorldBounds,
    ) -> Result<(), ClientLightingError> {
        self.owner.register_instance(handle, position, bounds)
    }
    pub fn update_rigid(
        &mut self,
        handle: PageHandle,
        position: BlockPos,
        bounds: WorldBounds,
    ) -> Result<(), ClientLightingError> {
        self.owner.update_instance(handle, position, bounds)
    }
    pub fn unregister(&mut self, handle: PageHandle) {
        self.owner.unregister_handle(handle)
    }
    pub fn dirty(&mut self, page: PageId) -> Result<(), ClientLightingError> {
        self.owner.invalidate_bounds(page_bounds(page)?);
        Ok(())
    }
    pub fn pump(
        &mut self,
        queue: &wgpu::Queue,
        terrain: &mut TerrainGpu,
        scene: &mut ClientScene,
        tick: Tick,
        budget: &mut UploadBudget,
        warm: bool,
    ) -> Result<ClientLightingWork, ClientLightingError> {
        let cells = if warm { 4096 } else { 1024 };
        self.owner.set_cpu_budget(LightBudget {
            max_cells: cells,
            max_neighbor_reads: cells * 6,
            max_seed_cells: cells,
            max_updates: cells,
        })?;
        let work = self.owner.pump_scene(queue, scene, tick, budget)?;
        self.stats.journal_entries += work.journal_entries as u64;
        self.stats.coalesced_light_changes += work.coalesced_light_changes;
        if work.upload_bytes > 0 {
            terrain.invalidate()
        }
        self.stats.cpu_cells += work.cpu_cells;
        self.stats.neighbors += work.cpu_neighbors;
        self.stats.upload_bytes += work.upload_bytes;
        self.stats.pending_cells = work.pending_light_cells;
        self.stats.pending_pages = work.pending_gpu_pages;
        self.stats.oldest_age_ticks = work.oldest_light_age_ticks;
        self.stats.overdue = work.light_overdue;
        self.stats.pending_source_pages = self.source_pending_pages(scene);
        self.stats.ready = work.ready && self.stats.pending_source_pages == 0;
        Ok(work)
    }
    /// Drains bounded authoritative metadata while the previous GPU image is
    /// still in flight. Queue writes remain confined to the upload phase.
    pub fn advance_cpu(&mut self, scene: &mut ClientScene) -> Result<(), ClientLightingError> {
        if !self.stats.authoritative_source {
            return Ok(());
        }
        let work = self.owner.ingest_scene_cpu(scene)?;
        self.stats.journal_entries += work.journal_entries as u64;
        self.stats.coalesced_light_changes += work.coalesced_light_changes;
        self.stats.pending_pages = work.pending_gpu_pages;
        Ok(())
    }
    pub fn snapshot(&self, scene: &ClientScene) -> LightingStats {
        let mut stats = self.stats;
        stats.mask = self.owner.mask_diagnostics();
        stats.pending_source_pages = self.source_pending_pages(scene);
        stats.pending_shadow_cascades = self.owner.shadows().cache().pending();
        stats.ready = self.is_ready(scene);
        stats
    }
    pub fn is_ready(&self, scene: &ClientScene) -> bool {
        self.source_pending_pages(scene) == 0 && self.owner.presentation_ready(scene.world())
    }
    pub fn inputs_ready(&self, scene: &ClientScene) -> bool {
        self.source_pending_pages(scene) == 0 && self.owner.inputs_ready(scene.world())
    }
    fn source_pending_pages(&self, scene: &ClientScene) -> usize {
        if self.stats.authoritative_source {
            // Visible pages form the configured box. Its one-page expansion is
            // exactly the union of sampled volume halos, with no duplicates.
            let config = scene.config();
            let page = config.center.page();
            let horizontal = i32::from(config.horizontal_radius) + 1;
            let vertical = i32::from(config.vertical_radius) + 1;
            let mut missing = 0;
            for dy in -vertical..=vertical {
                for dz in -horizontal..=horizontal {
                    for dx in -horizontal..=horizontal {
                        let (Some(x), Some(y), Some(z)) = (
                            page.x.checked_add(dx),
                            page.y.checked_add(dy),
                            page.z.checked_add(dz),
                        ) else {
                            continue;
                        };
                        let source = PageId { x, y, z, ..page };
                        if scene.world().page(source).is_some()
                            && scene.authoritative_light_version(source).is_none()
                        {
                            missing += 1;
                        }
                    }
                }
            }
            missing
        } else {
            0
        }
    }
    pub fn encode(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        terrain: &mut TerrainGpu,
        center: [f32; 3],
    ) -> Result<ShadowWork, ClientLightingError> {
        self.encode_with_sun(queue, encoder, terrain, center, SUN)
    }
    pub fn encode_with_sun(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        terrain: &mut TerrainGpu,
        center: [f32; 3],
        sun: [f32; 3],
    ) -> Result<ShadowWork, ClientLightingError> {
        let work = self
            .owner
            .encode_lighting(queue, encoder, terrain, center, sun)?;
        self.record_shadow(work);
        Ok(work)
    }
    pub fn encode_models(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        terrain: &mut TerrainGpu,
        center: [f32; 3],
        casters: &mut impl DynamicShadowCaster,
    ) -> Result<ShadowWork, ClientLightingError> {
        self.encode_models_with_sun(queue, encoder, terrain, center, SUN, casters)
    }
    pub fn encode_models_with_sun(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        terrain: &mut TerrainGpu,
        center: [f32; 3],
        sun: [f32; 3],
        casters: &mut impl DynamicShadowCaster,
    ) -> Result<ShadowWork, ClientLightingError> {
        let work = self
            .owner
            .encode_lighting_dynamic(queue, encoder, terrain, center, sun, casters)?;
        self.record_shadow(work);
        Ok(work)
    }
    fn record_shadow(&mut self, work: ShadowWork) {
        let clear = u64::from(work.gpu_clear_mask.count_ones());
        let restore = u64::from(work.gpu_restore_mask.count_ones());
        let resolution = u64::from(self.owner.shadows().cache().config().resolution);
        self.stats.shadow_gpu_clear_updates += clear;
        self.stats.shadow_gpu_restore_updates += restore;
        self.stats.shadow_gpu_clear_upper_bound += clear * resolution * resolution;
        self.stats.shadow_gpu_restore_upper_bound += restore * resolution * resolution;
        self.stats.shadow_draws +=
            u64::from(work.draw_calls + work.dynamic_draw_calls + work.dynamic_restore_draws);
        self.stats.static_shadow_draws += u64::from(work.draw_calls);
        self.stats.dynamic_shadow_draws += u64::from(work.dynamic_draw_calls);
        self.stats.dynamic_shadow_copy_bytes += work.dynamic_copy_bytes;
        self.stats.dynamic_shadow_restore_pixels += work.dynamic_restore_pixels;
        self.stats.dynamic_shadow_restore_draws += u64::from(work.dynamic_restore_draws);
        self.stats.shadow_updates += u64::from(work.cascade_updates);
        self.stats.shadow_texels += work.clear_pixels;
        self.stats.pending_shadow_cascades = work.pending_cascades;
    }
}
pub fn page_bounds(page: PageId) -> Result<WorldBounds, ClientLightingError> {
    let min = [page.x, page.y, page.z].map(|v| (i64::from(v) * 16) as f32);
    WorldBounds::new(min, min.map(|v| v + 16.)).map_err(Into::into)
}

/// Bounds of one immutable cuboid after its object transform. This transforms
/// eight bounds corners; it never assembles or uploads CPU render vertices.
pub fn rigid_bounds(
    matrix: [[f32; 4]; 4],
    extent: [f32; 3],
) -> Result<WorldBounds, ClientLightingError> {
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for corner in 0..8 {
        let point = std::array::from_fn::<_, 3, _>(|axis| {
            if corner & (1 << axis) == 0 {
                0.
            } else {
                extent[axis]
            }
        });
        for axis in 0..3 {
            let value = matrix[3][axis]
                + matrix[0][axis] * point[0]
                + matrix[1][axis] * point[1]
                + matrix[2][axis] * point[2];
            min[axis] = min[axis].min(value);
            max[axis] = max[axis].max(value);
        }
    }
    WorldBounds::new(min, max).map_err(Into::into)
}
