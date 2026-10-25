//! Loading-only attachment and per-frame budgets for the shared lighting owner.
//! Registration follows successful GPU publication, including both H1 handles.
use crate::{
    ClientScene,
    lighting::{ClientLighting, ClientLightingConfig, ClientLightingError, ClientLightingWork},
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
    pub cpu_cells: u64,
    pub neighbors: u64,
    pub upload_bytes: u64,
    pub shadow_draws: u64,
    pub shadow_updates: u64,
    pub shadow_texels: u64,
    pub pending_shadow_cascades: u32,
    pub pending_cells: usize,
    pub pending_pages: usize,
    pub oldest_age_ticks: u64,
    pub overdue: bool,
    pub ready: bool,
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
            stats: LightingStats::default(),
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
        self.stats.ready = work.ready;
        Ok(work)
    }
    pub fn encode(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        terrain: &mut TerrainGpu,
        center: [f32; 3],
    ) -> Result<ShadowWork, ClientLightingError> {
        let work = self
            .owner
            .encode_lighting(queue, encoder, terrain, center, SUN)?;
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
        let work = self
            .owner
            .encode_lighting_dynamic(queue, encoder, terrain, center, SUN, casters)?;
        self.record_shadow(work);
        Ok(work)
    }
    fn record_shadow(&mut self, work: ShadowWork) {
        self.stats.shadow_draws += u64::from(work.draw_calls + work.dynamic_draw_calls);
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
