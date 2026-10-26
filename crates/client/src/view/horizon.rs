//! Loading-prepared public horizon. Exact source/view manifests become ready only
//! after immutable proxy jobs complete AND their payloads reside on GPU.
use crate::{
    height_cache::{self, HeightCache},
    lodcache::{BuiltProxy, LodBuildTask, LodCacheError},
};
use core_types::{PageId, SpaceId};
use net::height::{GeometryStamp, HeightKey, Replacement};
use render::{
    height_gpu::{HeightGpu, HeightGpuConfig},
    height_tile::{HeightPlan, HeightTile, HeightTileKey, PlannedHeightTile},
    lod::LodKey,
    lod_gpu::{LodProxyGpu, ProxyCamera, ProxyConfig, ProxyError, ProxyHandle},
    shadows::ShadowGpu,
    terrain_gpu::{Camera, TerrainGpu, UploadBudget, translation},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
pub const MAX_PLAN_PAIRS: u64 = 4_000_000;
#[path = "horizon_selection.rs"]
mod selection;
pub use selection::{Certificate, Projection, SelectionQuality};
#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub max_tiles: usize,
    pub max_sources: usize,
    pub max_proxy_pages: usize,
    pub atlas_layers: u32,
    pub horizon_level: u8,
    pub screen_error_pixels: f64,
    pub hysteresis_pixels: f64,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            max_tiles: 512,
            max_sources: 2048,
            max_proxy_pages: 512,
            atlas_layers: 186,
            horizon_level: 9,
            screen_error_pixels: 32.,
            hysteresis_pixels: 8.,
        }
    }
}
#[derive(Debug)]
pub enum Error {
    InvalidConfig,
    InvalidWire,
    Capacity,
    Plan,
    StalePlan,
    Cache(height_cache::Error),
    Lod(LodCacheError),
    Proxy(ProxyError),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}
impl From<height_cache::Error> for Error {
    fn from(e: height_cache::Error) -> Self {
        Self::Cache(e)
    }
}
impl From<LodCacheError> for Error {
    fn from(e: LodCacheError) -> Self {
        Self::Lod(e)
    }
}
impl From<ProxyError> for Error {
    fn from(e: ProxyError) -> Self {
        Self::Proxy(e)
    }
}
#[derive(Clone, Copy, Debug, Default)]
pub struct Work {
    pub upload_bytes: u64,
    pub height_tiles: usize,
    pub proxy_pages: usize,
    pub retired: usize,
    pub published: bool,
    pub held_previous: bool,
    pub cpu_vertices: u64,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    pub desired_height_tiles: usize,
    pub cached_height_tiles: usize,
    pub received_desired_heights: usize,
    pub wanted_refine_tiles: usize,
    pub unavailable_columns: usize,
    pub known_geometry_sources: usize,
    pub near_guard_ready: bool,
    pub plan_id: u64,
    pub accepted_plan_id: u64,
    pub plan_generation: u64,
    pub missing_error_certificates: u64,
    pub selection_capacity_limited: bool,
    pub selection_leaf_limited: u64,
    pub maximum_error_pixels: f64,
    pub sse_refinements: u64,
    pub pending_height_confirmations: usize,
    pub pending_exact_columns: usize,
    pub height_front: usize,
    pub height_staged: usize,
    pub proxy_front: usize,
    pub proxy_staged: usize,
    pub wanted_geometry: usize,
    pub pipeline_compilations: u32,
    pub startup_bytes: u64,
    pub enabled: bool,
    pub failed_proxy_pages: usize,
    pub proxy_jobs: usize,
    pub pending_proxy_pages: usize,
}
#[derive(Clone)]
struct Resident {
    stamp: GeometryStamp,
    handle: ProxyHandle,
}
struct GeometryMeta {
    stamp: GeometryStamp,
    revision: u64,
    input: Option<Arc<net::PublicGeometry>>,
    ready: Option<Arc<render::lod_gpu::ProxyPayload>>,
    failures: u8,
}
/// Immutable planning snapshot. Execute on a worker; no World or GPU access.
pub struct PlanTask {
    pub id: u64,
    generation: u64,
    config: Config,
    space: u32,
    camera: [i64; 2],
    refine: Vec<HeightKey>,
    near_radius: i64,
    projection: Option<Projection>,
    certificates: Vec<Certificate>,
    previous: Vec<HeightTileKey>,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct PlanWork {
    pub base_nodes: u64,
    pub refinements: u64,
    pub near_tiles_removed: u64,
    pub adjacency_tests: u64,
    pub seam_tests: u64,
}
pub struct Planned {
    pub id: u64,
    generation: u64,
    pub plan: HeightPlan,
    pub work: PlanWork,
    pub quality: SelectionQuality,
    pub exact_leaves: Vec<HeightKey>,
}
impl PlanTask {
    pub fn execute(self) -> Result<Planned, Error> {
        if let Some(projection) = self.projection {
            return selection::execute_sse(&self, projection);
        }
        let mut plan = HeightPlan::new(
            self.space,
            self.camera,
            self.config.horizon_level,
            self.config.max_tiles,
        )
        .map_err(|_| Error::Plan)?;
        let mut work = PlanWork {
            base_nodes: u64::from(plan.visited_nodes),
            ..PlanWork::default()
        };
        let near = [
            (self.camera[0] - self.near_radius) * 16,
            (self.camera[1] - self.near_radius) * 16,
            (self.camera[0] + self.near_radius + 1) * 16,
            (self.camera[1] + self.near_radius + 1) * 16,
        ];
        loop {
            let mut split = None;
            let mut remove = None;
            for (i, tile) in plan.tiles.iter().enumerate() {
                let b = tile.key.bounds().ok_or(Error::Plan)?;
                if contains(near, b) {
                    remove = Some(i);
                    break;
                }
                let exact = self.refine.iter().any(|key| {
                    key.space == tile.key.space
                        && key.level <= tile.key.level
                        && render_key(*key)
                            .bounds()
                            .is_some_and(|hint| contains(b, hint))
                });
                if tile.key.level > 0 && (exact || intersects(near, b)) {
                    split = Some(i);
                    break;
                }
            }
            if let Some(index) = remove {
                plan.tiles.swap_remove(index);
                work.near_tiles_removed += 1;
                continue;
            }
            if split.is_none() {
                'outer: for i in 0..plan.tiles.len() {
                    for j in i + 1..plan.tiles.len() {
                        if work.adjacency_tests + work.seam_tests >= MAX_PLAN_PAIRS {
                            return Err(Error::Capacity);
                        }
                        work.adjacency_tests += 1;
                        let a = plan.tiles[i].key;
                        let b = plan.tiles[j].key;
                        if adjacent(a, b) && a.level.abs_diff(b.level) > 1 {
                            split = Some(if a.level > b.level { i } else { j });
                            break 'outer;
                        }
                    }
                }
            }
            let Some(index) = split else { break };
            if plan.tiles.len() + 3 > self.config.max_tiles {
                return Err(Error::Capacity);
            }
            if work.refinements >= 8192 {
                return Err(Error::Capacity);
            }
            work.refinements += 1;
            let parent = plan.tiles.swap_remove(index).key;
            for z in 0..2 {
                for x in 0..2 {
                    plan.tiles.push(PlannedHeightTile {
                        key: HeightTileKey {
                            space: parent.space,
                            level: parent.level - 1,
                            x: parent.x * 2 + x,
                            z: parent.z * 2 + z,
                        },
                        seams: 0,
                    });
                }
            }
        }
        for tile in &mut plan.tiles {
            tile.seams = 0;
        }
        for i in 0..plan.tiles.len() {
            for j in i + 1..plan.tiles.len() {
                if work.adjacency_tests + work.seam_tests >= MAX_PLAN_PAIRS {
                    return Err(Error::Capacity);
                }
                work.seam_tests += 1;
                let a = plan.tiles[i].key;
                let b = plan.tiles[j].key;
                if !adjacent(a, b) || a.level == b.level {
                    continue;
                }
                if a.level.abs_diff(b.level) != 1 {
                    return Err(Error::Plan);
                }
                let (fine, coarse) = if a.level < b.level { (i, b) } else { (j, a) };
                let f = plan.tiles[fine].key.bounds().ok_or(Error::Plan)?;
                let c = coarse.bounds().ok_or(Error::Plan)?;
                plan.tiles[fine].seams |= if f[0] == c[2] {
                    1
                } else if f[2] == c[0] {
                    2
                } else if f[1] == c[3] {
                    4
                } else {
                    8
                };
            }
        }
        plan.tiles.sort_unstable_by_key(|t| t.key);
        Ok(Planned {
            id: self.id,
            generation: self.generation,
            plan,
            work,
            quality: SelectionQuality::default(),
            exact_leaves: Vec::new(),
        })
    }
}
pub struct ProxyTask {
    pub stamp: GeometryStamp,
    pub revision: u64,
    atlas_layers: u32,
    input: Arc<net::PublicGeometry>,
}
pub struct ProxyBuilt {
    pub stamp: GeometryStamp,
    pub built: BuiltProxy,
}
impl ProxyTask {
    /// Decode + all4096 public samples/protection/packing stay on the worker.
    pub fn execute(self) -> Result<ProxyBuilt, ProxyError> {
        self.input
            .validate()
            .map_err(|_| ProxyError::InvalidMaterial)?;
        let palette = self
            .input
            .palette
            .iter()
            .map(|surface| render::lod::PublicVoxel::Solid {
                material: surface.atlas_layer,
                tint: surface.tint,
                kind: match surface.kind {
                    net::PublicSurfaceKind::Opaque => render::lod::SurfaceKind::Opaque,
                    net::PublicSurfaceKind::Cutout => render::lod::SurfaceKind::Cutout,
                    net::PublicSurfaceKind::Water => render::lod::SurfaceKind::Water,
                },
            })
            .collect::<Vec<_>>();
        let source = crate::lodcache::decode_geometry(
            &crate::lodcache::GeometryInput {
                page: self.input.page,
                source: self.input.source_version,
                view: self.input.view_version,
                known: &self.input.known,
                occupied: &self.input.occupied,
                palette: &palette,
                index_bits: self.input.index_bits,
                indices: &self.input.indices,
            },
            self.atlas_layers,
        )
        .map_err(|_| ProxyError::InvalidMaterial)?;
        let task = LodBuildTask::from_public_snapshot(
            lod_key(self.stamp.page),
            self.revision,
            source,
            self.atlas_layers,
        )
        .map_err(|_| ProxyError::InvalidMaterial)?;
        Ok(ProxyBuilt {
            stamp: self.stamp,
            built: task.execute()?,
        })
    }
}
pub struct ViewHorizon {
    config: Config,
    pub height: HeightGpu,
    pub proxies: LodProxyGpu,
    cache: HeightCache,
    proxy_running: BTreeMap<PageId, u64>,
    next_proxy_revision: u64,
    wire: BTreeMap<HeightKey, Arc<net::height::HeightTile>>,
    certificates: BTreeMap<HeightKey, Certificate>,
    geometry: BTreeMap<PageId, GeometryMeta>,
    wanted: BTreeMap<PageId, GeometryStamp>,
    front: BTreeMap<PageId, Resident>,
    staged: BTreeMap<PageId, Resident>,
    retired: Vec<ProxyHandle>,
    removed: BTreeSet<PageId>,
    changes: Vec<(ProxyHandle, bool)>,
    keys: Vec<PageId>,
    models: Vec<(ProxyHandle, [[f32; 4]; 4])>,
    origin: [i64; 3],
    plan_id: u64,
    accepted_plan_id: u64,
    plan_generation: u64,
    last_request: Option<(u32, [i64; 2], u64)>,
    projection: Option<Projection>,
    quality: SelectionQuality,
    fresh_heights: BTreeSet<HeightKey>,
    exact_leaves: BTreeSet<HeightKey>,
    near_radius: Option<i64>,
    near_bounds: Option<(u32, [i64; 4])>,
    guard_ready: bool,
    dirty: bool,
    enabled: bool,
    startup: Stats,
}
impl ViewHorizon {
    pub fn prepare(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        terrain: &TerrainGpu,
        shadows: Option<&ShadowGpu>,
        config: Config,
    ) -> Result<Self, Error> {
        Self::prepare_with_environment(device, queue, terrain, shadows, config, None)
    }
    pub fn prepare_with_environment(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        terrain: &TerrainGpu,
        shadows: Option<&ShadowGpu>,
        config: Config,
        atmosphere: Option<&render::atmosphere::AtmosphereGpu>,
    ) -> Result<Self, Error> {
        if config.max_tiles == 0
            || config.max_tiles > 512
            || config.max_sources < config.max_tiles
            || config.max_sources > 4096
            || config.max_proxy_pages == 0
            || config.max_proxy_pages > 512
            || config.horizon_level > 10
            || !config.screen_error_pixels.is_finite()
            || config.screen_error_pixels <= 0.
            || !config.hysteresis_pixels.is_finite()
            || config.hysteresis_pixels < 0.
            || config.hysteresis_pixels >= config.screen_error_pixels
        {
            return Err(Error::InvalidConfig);
        }
        let height_config = HeightGpuConfig {
            max_tiles: config.max_tiles as u32 * 2,
            atlas_layers: config.atlas_layers,
        };
        let mut height = if let Some(atmosphere) = atmosphere {
            HeightGpu::new_with_atmosphere(
                device,
                terrain.geometry(),
                height_config,
                shadows,
                atmosphere,
            )
        } else {
            HeightGpu::new(device, terrain.geometry(), height_config, shadows)
        }
        .map_err(height_cache::Error::Gpu)?;
        let proxy_config = ProxyConfig {
            max_objects: config.max_proxy_pages as u32 * 2,
            atlas_layers: config.atlas_layers,
            ..ProxyConfig::default()
        };
        let mut proxies = if let Some(atmosphere) = atmosphere {
            LodProxyGpu::new_with_atmosphere(
                device,
                queue,
                terrain.geometry(),
                proxy_config,
                shadows,
                atmosphere,
            )
        } else {
            LodProxyGpu::new(device, queue, terrain.geometry(), proxy_config, shadows)
        }?;
        let h = height.take_work();
        let p = proxies.take_work();
        Ok(Self {
            config,
            height,
            proxies,
            cache: HeightCache::new(height_cache::Config {
                max_tiles: config.max_tiles,
                max_sources: config.max_sources,
                atlas_layers: config.atlas_layers,
            })?,
            proxy_running: BTreeMap::new(),
            next_proxy_revision: 1,
            wire: BTreeMap::new(),
            certificates: BTreeMap::new(),
            geometry: BTreeMap::new(),
            wanted: BTreeMap::new(),
            front: BTreeMap::new(),
            staged: BTreeMap::new(),
            retired: Vec::with_capacity(config.max_proxy_pages * 2),
            removed: BTreeSet::new(),
            changes: Vec::with_capacity(config.max_proxy_pages * 2),
            keys: Vec::with_capacity(config.max_proxy_pages * 2),
            models: Vec::with_capacity(config.max_proxy_pages * 2),
            origin: [0; 3],
            plan_id: 0,
            accepted_plan_id: 0,
            plan_generation: 0,
            last_request: None,
            projection: None,
            quality: SelectionQuality::default(),
            fresh_heights: BTreeSet::new(),
            exact_leaves: BTreeSet::new(),
            near_radius: None,
            near_bounds: None,
            guard_ready: false,
            dirty: false,
            enabled: false,
            startup: Stats {
                pipeline_compilations: h.pipeline_compilations + p.pipeline_compilations,
                startup_bytes: h.upload_bytes + p.upload_bytes,
                ..Stats::default()
            },
        })
    }
    /// Must cover the ENTIRE near terrain draw region. It remains an Unknown
    /// cutout where exact current meshes are unavailable; distance is no proof.
    /// Enabling waits until the first guarded plan has actually been published.
    pub fn reserve_near_cutout(&mut self, radius_pages: u32) -> Result<(), Error> {
        if radius_pages > 128 {
            return Err(Error::InvalidConfig);
        }
        self.near_radius = Some(i64::from(radius_pages));
        self.plan_generation = self.plan_generation.checked_add(1).ok_or(Error::Capacity)?;
        self.enabled = false;
        self.guard_ready = false;
        self.near_bounds = None;
        Ok(())
    }
    /// Call before EVERY camera-page transition can draw. Budget refusal makes
    /// draw() return zero until the full guard batch succeeds. Old distant cover
    /// remains allocated; newly near tiles/proxies are conservatively suppressed.
    pub fn guard_near(
        &mut self,
        queue: &wgpu::Queue,
        space: u32,
        camera: [i64; 2],
        budget: &mut UploadBudget,
    ) -> Result<Option<u64>, Error> {
        let Some(radius) = self.near_radius else {
            return Ok(None);
        };
        if camera
            .iter()
            .any(|v| *v < i64::from(i32::MIN) / 16 || *v > i64::from(i32::MAX) / 16)
        {
            return Err(Error::Plan);
        }
        let bounds = [
            (camera[0] - radius) * 16,
            (camera[1] - radius) * 16,
            (camera[0] + radius + 1) * 16,
            (camera[1] + radius + 1) * 16,
        ];
        if self.guard_ready && self.near_bounds == Some((space, bounds)) {
            return Ok(Some(0));
        }
        let worst = self.cache.front_len() as u64 * 4 + self.front.len() as u64 * 4;
        if budget.remaining_bytes < worst {
            self.guard_ready = false;
            return Ok(None);
        }
        let before = budget.remaining_bytes;
        let result = self
            .cache
            .suppress_region(&mut self.height, queue, space, bounds, budget);
        if let Err(error) = result {
            self.guard_ready = false;
            return Err(error.into());
        }
        self.changes.clear();
        for (&page, r) in &self.front {
            self.changes.push((
                r.handle,
                !page_near(page, Some((space, bounds))) && !self.removed.contains(&page),
            ));
        }
        self.changes.sort_unstable_by_key(|c| c.0.slot);
        self.proxies
            .set_enabled_batch(queue, &self.changes, budget)?;
        self.near_bounds = Some((space, bounds));
        self.guard_ready = true;
        self.dirty = true;
        Ok(Some(before - budget.remaining_bytes))
    }
    pub fn plan_task(
        &mut self,
        space: u32,
        camera_page: [i64; 2],
    ) -> Result<Option<PlanTask>, Error> {
        let Some(radius) = self.near_radius else {
            return Ok(None);
        };
        if self.last_request == Some((space, camera_page, self.plan_generation)) {
            return Ok(None);
        }
        self.plan_id = self.plan_id.checked_add(1).ok_or(Error::Capacity)?;
        self.last_request = Some((space, camera_page, self.plan_generation));
        let refine = self
            .wire
            .values()
            .filter(|t| matches!(t.replacement, Replacement::Refine))
            .map(|t| t.key)
            .collect::<Vec<_>>();
        if refine.len() > 512 {
            self.last_request = None;
            return Err(Error::Capacity);
        }
        Ok(Some(PlanTask {
            id: self.plan_id,
            generation: self.plan_generation,
            config: self.config,
            space,
            camera: camera_page,
            refine,
            near_radius: radius,
            projection: self.projection,
            certificates: self.certificates.values().copied().collect(),
            previous: self.cache.wanted().collect(),
        }))
    }
    pub fn complete_plan(&mut self, result: Planned) -> Result<bool, Error> {
        if result.id != self.plan_id || result.generation != self.plan_generation {
            return Ok(false);
        }
        let previous = self.cache.wanted().map(wire_key).collect::<BTreeSet<_>>();
        self.cache.set_plan(&result.plan)?;
        // Cached ancestor metadata can guide selection but cannot certify a new
        // interest period: edits may have occurred while it was unsubscribed.
        self.fresh_heights.retain(|key| {
            previous.contains(key)
                && result
                    .plan
                    .tiles
                    .iter()
                    .any(|tile| wire_key(tile.key) == *key)
        });
        self.quality = result.quality;
        self.exact_leaves = result.exact_leaves.into_iter().collect();
        let keys = self.cache.wanted().map(wire_key).collect::<Vec<_>>();
        for key in keys {
            if let Some(tile) = self.wire.get(&key) {
                self.cache.ingest(Arc::new(render_tile(tile)))?;
            }
        }
        self.rebuild_wanted()?;
        // Keep only current leaves and ancestors useful for future coarsening.
        // A long route cannot fill the fixed cache with obsolete fine tiles.
        let mut keep = BTreeSet::new();
        for tile in &result.plan.tiles {
            let mut key = wire_key(tile.key);
            loop {
                keep.insert(key);
                if key.level == self.config.horizon_level {
                    break;
                }
                key = HeightKey {
                    level: key.level + 1,
                    x: key.x.div_euclid(2),
                    z: key.z.div_euclid(2),
                    ..key
                };
            }
        }
        self.wire.retain(|key, _| keep.contains(key));
        self.certificates.retain(|key, _| keep.contains(key));
        self.accepted_plan_id = result.id;
        self.dirty = true;
        Ok(true)
    }
    pub fn wanted_height(&self) -> impl Iterator<Item = HeightKey> + '_ {
        self.cache.wanted().map(wire_key)
    }
    pub fn wanted_exact_height(&self) -> impl Iterator<Item = HeightKey> + '_ {
        self.exact_leaves.iter().copied()
    }
    pub fn ingest_height(&mut self, tile: Box<net::height::HeightTile>) -> Result<bool, Error> {
        tile.validate().map_err(|_| Error::InvalidWire)?;
        let selected = self.cache.wanted().any(|key| wire_key(key) == tile.key);
        let mut exact_promotion = false;
        if let Some(old) = self.wire.get(&tile.key) {
            if tile.source < old.source {
                return Ok(false);
            }
            if tile.source == old.source && **old == *tile {
                if selected && self.fresh_heights.insert(tile.key) {
                    self.dirty = true;
                }
                return Ok(false);
            }
            if tile.source == old.source
                && (render_tile(old) != render_tile(&tile) || old.error != tile.error)
            {
                exact_promotion = old.protected == [0; 32]
                    && matches!(old.replacement, Replacement::None)
                    && tile.key.level == 0
                    && tile.protected == [255; 32]
                    && matches!(&tile.replacement,Replacement::Column{geometry,..}if !geometry.is_empty())
                    && old.heights == tile.heights
                    && old.known == tile.known
                    && old.atlas == tile.atlas
                    && old.tint == tile.tint
                    && old.error == tile.error;
                if !exact_promotion {
                    return Err(Error::InvalidWire);
                }
            }
        } else if self.wire.len() >= self.config.max_sources {
            return Err(Error::Capacity);
        }
        let refine = matches!(tile.replacement, Replacement::Refine);
        let certified = tile.error.is_some();
        let tile: Arc<net::height::HeightTile> = Arc::from(tile);
        let rendered = Arc::new(render_tile(&tile));
        if exact_promotion {
            self.cache.ingest_exact_promotion(rendered)?;
        } else {
            self.cache.ingest(rendered)?;
        }
        if selected {
            self.fresh_heights.insert(tile.key);
        }
        if let Some(bound) = tile.error {
            self.certificates.insert(tile.key,Certificate{key:tile.key,bound,
                seam_error_q1024:selection::seam_error_q1024(&tile.heights),
                exact_replacement:matches!(&tile.replacement,Replacement::Column{geometry,..}if !geometry.is_empty())});
        } else {
            self.certificates.remove(&tile.key);
        }
        self.wire.insert(tile.key, tile);
        if refine || certified {
            self.plan_generation = self.plan_generation.checked_add(1).ok_or(Error::Capacity)?;
        }
        self.rebuild_wanted()?;
        self.dirty = true;
        Ok(true)
    }
    /// The eye box is quantized by the View, so head bob and camera interpolation
    /// never replan at display frequency. Resizing or FOV changes do replan.
    pub fn set_projection(&mut self, mut projection: Projection) -> Result<(), Error> {
        projection.target_pixels = self.config.screen_error_pixels;
        projection.hysteresis_pixels = self.config.hysteresis_pixels;
        projection.validate()?;
        if self.projection != Some(projection) {
            self.plan_generation = self.plan_generation.checked_add(1).ok_or(Error::Capacity)?;
            self.projection = Some(projection);
        }
        Ok(())
    }
    fn rebuild_wanted(&mut self) -> Result<(), Error> {
        self.wanted.clear();
        for key in self.cache.wanted() {
            if let Some(tile) = self.wire.get(&wire_key(key))
                && let Replacement::Column { geometry, .. } = &tile.replacement
            {
                for &stamp in geometry {
                    if !self.wanted.contains_key(&stamp.page)
                        && self.wanted.len() >= self.config.max_proxy_pages
                    {
                        return Err(Error::Capacity);
                    }
                    if let Some(old) = self.wanted.insert(stamp.page, stamp)
                        && old != stamp
                    {
                        return Err(Error::InvalidWire);
                    }
                }
            }
        }
        Ok(())
    }
    /// Queue immutable packed public data. The owner does no voxel reconstruction
    /// or forest build; format/body validation runs in ProxyTask on a worker.
    pub fn ingest_geometry(&mut self, geometry: &net::PublicGeometry) -> Result<bool, Error> {
        self.ingest_geometry_owned(Box::new(geometry.clone()))
    }
    pub fn ingest_geometry_owned(
        &mut self,
        geometry: Box<net::PublicGeometry>,
    ) -> Result<bool, Error> {
        if geometry.source_version.0 == 0
            || geometry.view_version == 0
            || geometry.palette.len() > 4096
            || geometry.indices.len() > 6144
        {
            return Err(Error::InvalidWire);
        }
        if self.geometry.get(&geometry.page).is_some_and(|old| {
            old.stamp.source > geometry.source_version
                || (old.stamp.source == geometry.source_version
                    && old.stamp.view >= geometry.view_version)
        }) {
            return Ok(false);
        }
        if !self.geometry.contains_key(&geometry.page)
            && self.geometry.len() >= self.config.max_proxy_pages
        {
            return Err(Error::Capacity);
        }
        let revision = self.next_proxy_revision;
        self.next_proxy_revision = self
            .next_proxy_revision
            .checked_add(1)
            .ok_or(Error::Capacity)?;
        let stamp = GeometryStamp {
            page: geometry.page,
            source: geometry.source_version,
            view: geometry.view_version,
        };
        self.removed.remove(&geometry.page);
        self.geometry.insert(
            geometry.page,
            GeometryMeta {
                stamp,
                revision,
                input: Some(Arc::from(geometry)),
                ready: None,
                failures: 0,
            },
        );
        self.dirty = true;
        Ok(true)
    }
    pub fn take_proxy_task(&mut self) -> Option<ProxyTask> {
        if self.proxy_running.len() >= 4 {
            return None;
        }
        let (&page, meta) = self.geometry.iter_mut().find(|(page, meta)| {
            meta.input.is_some() && meta.failures < 3 && !self.proxy_running.contains_key(page)
        })?;
        let input = meta.input.as_ref().expect("pending public DTO").clone();
        self.proxy_running.insert(page, meta.revision);
        Some(ProxyTask {
            stamp: meta.stamp,
            revision: meta.revision,
            atlas_layers: self.config.atlas_layers,
            input,
        })
    }
    pub fn complete_proxy(&mut self, result: ProxyBuilt) -> Result<bool, Box<ProxyBuilt>> {
        if self.proxy_running.get(&result.stamp.page) == Some(&result.built.revision) {
            self.proxy_running.remove(&result.stamp.page);
        }
        let Some(meta) = self
            .geometry
            .get_mut(&result.stamp.page)
            .filter(|meta| meta.stamp == result.stamp && meta.revision == result.built.revision)
        else {
            return Ok(false);
        };
        if result.built.key != lod_key(result.stamp.page)
            || !result
                .built
                .payload
                .materials_within(self.config.atlas_layers)
        {
            return Ok(false);
        }
        meta.ready = Some(result.built.payload);
        meta.input = None;
        meta.failures = 0;
        self.dirty = true;
        Ok(true)
    }
    pub fn fail_plan(&mut self, id: u64) {
        if id == self.plan_id {
            self.last_request = None;
        }
    }
    /// Revocation is applied before the next draw. A budget refusal suppresses
    /// the whole horizon until this pending invalidation can be published safely.
    pub fn remove_geometry(&mut self, page: PageId) -> Result<(), Error> {
        self.geometry.remove(&page);
        self.removed.insert(page);
        self.dirty = true;
        Ok(())
    }
    pub fn fail_proxy(&mut self, key: LodKey, revision: u64) {
        let page = PageId {
            space: SpaceId(key.space),
            x: key.x as i32,
            y: key.y as i32,
            z: key.z as i32,
        };
        if self.proxy_running.get(&page) == Some(&revision) {
            self.proxy_running.remove(&page);
        }
        if let Some(meta) = self
            .geometry
            .get_mut(&page)
            .filter(|meta| meta.revision == revision)
        {
            meta.failures = meta.failures.saturating_add(1);
        }
    }
    pub fn pump(
        &mut self,
        queue: &wgpu::Queue,
        budget: &mut UploadBudget,
        max_height: usize,
        max_proxy: usize,
    ) -> Result<Work, Error> {
        if !self.dirty && self.retired.is_empty() && self.cache.pending_retirements() == 0 {
            return Ok(Work::default());
        }
        let before = budget.remaining_bytes;
        if !self.removed.is_empty() {
            self.changes.clear();
            for page in &self.removed {
                if let Some(r) = self.front.get(page) {
                    self.changes.push((r.handle, false));
                }
            }
            self.changes.sort_unstable_by_key(|c| c.0.slot);
            if budget.remaining_bytes < self.changes.len() as u64 * 4 {
                self.guard_ready = false;
                return Ok(Work {
                    held_previous: true,
                    ..Work::default()
                });
            }
            self.proxies
                .set_enabled_batch(queue, &self.changes, budget)?;
            for page in &self.removed {
                if let Some(r) = self.front.remove(page) {
                    self.retired.push(r.handle);
                }
                if let Some(r) = self.staged.remove(page) {
                    self.retired.push(r.handle);
                }
            }
            self.removed.clear();
        }
        let mut work = Work::default();
        while work.retired < max_proxy
            && let Some(&handle) = self.retired.last()
        {
            if budget.remaining_bytes < 256 {
                break;
            }
            self.proxies.release(queue, handle, budget)?;
            self.retired.pop();
            work.retired += 1;
        }
        if self
            .cache
            .wanted()
            .any(|key| !self.fresh_heights.contains(&wire_key(key)))
            || self.exact_leaves.iter().any(|key|self.wire.get(key).is_none_or(|tile|
                !matches!(&tile.replacement,Replacement::Column{geometry,..}if !geometry.is_empty())))
        {
            work.held_previous = true;
            work.upload_bytes = before - budget.remaining_bytes;
            return Ok(work);
        }
        self.keys.clear();
        for (&page, resident) in &self.staged {
            if self.wanted.get(&page) != Some(&resident.stamp) {
                self.keys.push(page);
            }
        }
        for page in &self.keys {
            let resident = self.staged.remove(page).expect("staged key");
            self.retired.push(resident.handle);
        }
        for (&page, &stamp) in &self.wanted {
            if work.proxy_pages >= max_proxy {
                break;
            }
            if self
                .front
                .get(&page)
                .or_else(|| self.staged.get(&page))
                .is_some_and(|resident| resident.stamp == stamp)
            {
                continue;
            }
            let Some(meta) = self.geometry.get(&page).filter(|meta| meta.stamp == stamp) else {
                continue;
            };
            let Some(payload) = meta.ready.as_ref() else {
                continue;
            };
            if budget.remaining_bytes < payload.upload_bytes() + 4 {
                break;
            }
            let handle = self.proxies.upload_inactive(
                queue,
                meta.revision,
                page_model(page, self.origin),
                [0., 0., 0., 1.],
                payload,
                budget,
            )?;
            self.staged.insert(page, Resident { stamp, handle });
            work.proxy_pages += 1;
        }
        let all_proxy_ready = self.wanted.iter().all(|(page, stamp)| {
            self.staged
                .get(page)
                .filter(|r| r.stamp == *stamp)
                .or_else(|| self.front.get(page).filter(|r| r.stamp == *stamp))
                .is_some()
        });
        self.changes.clear();
        if all_proxy_ready {
            for (&page, resident) in &self.front {
                if self.wanted.get(&page) != Some(&resident.stamp) {
                    self.changes.push((resident.handle, false));
                }
            }
            for (&page, resident) in &self.staged {
                if self.wanted.get(&page) == Some(&resident.stamp) {
                    self.changes
                        .push((resident.handle, !page_near(page, self.near_bounds)));
                }
            }
        }
        self.changes.sort_unstable_by_key(|c| c.0.slot);
        let reserve = self.changes.len() as u64 * 4;
        if budget.remaining_bytes < reserve {
            work.held_previous = true;
            work.upload_bytes = before - budget.remaining_bytes;
            return Ok(work);
        }
        let mut height_budget = UploadBudget::new(budget.remaining_bytes - reserve);
        let height_before = height_budget.remaining_bytes;
        let h = self.cache.pump(
            &mut self.height,
            queue,
            &mut height_budget,
            max_height,
            |tile| {
                let Some(wire) = self
                    .wire
                    .get(&wire_key(tile.key))
                    .filter(|wire| wire.source == tile.source)
                else {
                    return false;
                };
                match &wire.replacement {
                    Replacement::None => true,
                    Replacement::Refine => false,
                    Replacement::Column { geometry, .. } => {
                        !geometry.is_empty()
                            && geometry.iter().all(|stamp| {
                                self.staged
                                    .get(&stamp.page)
                                    .filter(|r| r.stamp == *stamp)
                                    .or_else(|| {
                                        self.front.get(&stamp.page).filter(|r| r.stamp == *stamp)
                                    })
                                    .is_some()
                            })
                    }
                }
            },
        );
        budget.remaining_bytes -= height_before - height_budget.remaining_bytes;
        let h = h?;
        work.height_tiles = h.staged;
        work.retired += h.released;
        work.held_previous = h.held_previous || !all_proxy_ready;
        if !h.held_previous && all_proxy_ready {
            self.proxies
                .set_enabled_batch(queue, &self.changes, budget)?;
            self.keys.clear();
            for (&page, resident) in &self.front {
                if self.wanted.get(&page) != Some(&resident.stamp) {
                    self.keys.push(page);
                }
            }
            for page in &self.keys {
                self.retired
                    .push(self.front.remove(page).expect("old proxy").handle);
            }
            self.keys.clear();
            for (&page, resident) in &self.staged {
                if self.wanted.get(&page) == Some(&resident.stamp) {
                    self.keys.push(page);
                }
            }
            for page in &self.keys {
                self.front
                    .insert(*page, self.staged.remove(page).expect("current staging"));
            }
            work.published = h.published || !self.changes.is_empty();
            self.dirty = false;
            self.enabled = self.near_radius.is_some() && self.guard_ready;
        }
        work.upload_bytes = before - budget.remaining_bytes;
        Ok(work)
    }
    pub fn rebase(
        &mut self,
        queue: &wgpu::Queue,
        origin: [i64; 3],
        budget: &mut UploadBudget,
    ) -> Result<Option<u64>, Error> {
        if origin == self.origin {
            return Ok(Some(0));
        }
        let worst = self.config.max_tiles as u64 * 2 * 64
            + (self.front.len() + self.staged.len()) as u64 * 256;
        if budget.remaining_bytes < worst {
            return Ok(None);
        }
        self.models.clear();
        for (&page, r) in self.front.iter().chain(&self.staged) {
            self.models.push((r.handle, page_model(page, origin)));
        }
        self.models.sort_unstable_by_key(|c| c.0.slot);
        let before = budget.remaining_bytes;
        self.cache.rebase(&mut self.height, queue, origin, budget)?;
        self.proxies.set_models_batch(queue, &self.models, budget)?;
        self.origin = origin;
        Ok(Some(before - budget.remaining_bytes))
    }
    pub fn prepare_camera(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        camera: Camera,
        eye: [f32; 3],
        viewport_height: u32,
    ) -> Result<(), Error> {
        if !self.enabled || !self.guard_ready {
            return Ok(());
        }
        self.height
            .prepare_frame(queue, encoder, camera)
            .map_err(height_cache::Error::Gpu)?;
        self.proxies.prepare_frame(
            queue,
            encoder,
            ProxyCamera {
                camera,
                eye,
                viewport_height,
                orthographic_direction: None,
            },
        )?;
        Ok(())
    }
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) -> u32 {
        if self.enabled && self.guard_ready {
            self.height.draw(pass) + self.proxies.draw(pass)
        } else {
            0
        }
    }
    pub fn front_source(&self, key: HeightKey) -> Option<u64> {
        self.cache.front_source(render_key(key))
    }
    pub fn stats(&self) -> Stats {
        Stats {
            desired_height_tiles:self.cache.wanted().count(),
            cached_height_tiles:self.wire.len(),
            received_desired_heights:self.cache.wanted().filter(|key|self.wire.contains_key(&wire_key(*key))).count(),
            wanted_refine_tiles:self.cache.wanted().filter_map(|key|self.wire.get(&wire_key(key))).filter(|tile|matches!(tile.replacement,Replacement::Refine)).count(),
            unavailable_columns:self.cache.wanted().filter_map(|key|self.wire.get(&wire_key(key))).filter(|tile|matches!(&tile.replacement,Replacement::Column{geometry,..}if geometry.is_empty())).count(),
            known_geometry_sources:self.geometry.len(),
            near_guard_ready:self.guard_ready,
            plan_id:self.plan_id,
            accepted_plan_id:self.accepted_plan_id,
            plan_generation:self.plan_generation,
            missing_error_certificates:self.quality.missing_certificates,
            selection_capacity_limited:self.quality.capacity_limited,
            selection_leaf_limited:self.quality.leaf_limited,
            maximum_error_pixels:self.quality.maximum_error_pixels,
            sse_refinements:self.quality.sse_refinements,
            pending_height_confirmations:self.cache.wanted().filter(|key|!self.fresh_heights.contains(&wire_key(*key))).count(),
            pending_exact_columns:self.exact_leaves.iter().filter(|key|self.wire.get(key).is_none_or(|tile|
                !matches!(&tile.replacement,Replacement::Column{geometry,..}if !geometry.is_empty()))).count(),
            height_front: self.cache.front_len(),
            height_staged: self.cache.staged_len(),
            proxy_front: self.front.len(),
            proxy_staged: self.staged.len(),
            wanted_geometry: self.wanted.len(),
            failed_proxy_pages: self
                .geometry
                .values()
                .filter(|meta| meta.failures >= 3)
                .count(),
            proxy_jobs: self.proxy_running.len(),
            pending_proxy_pages: self
                .geometry
                .values()
                .filter(|meta| meta.input.is_some())
                .count(),
            enabled: self.enabled && self.guard_ready,
            ..self.startup
        }
    }
}
fn page_model(page: PageId, origin: [i64; 3]) -> [[f32; 4]; 4] {
    translation([
        (i64::from(page.x) * 16 - origin[0]) as f32,
        (i64::from(page.y) * 16 - origin[1]) as f32,
        (i64::from(page.z) * 16 - origin[2]) as f32,
    ])
}
fn lod_key(page: PageId) -> LodKey {
    LodKey::page(
        page.space.0,
        i64::from(page.x),
        i64::from(page.y),
        i64::from(page.z),
    )
}
fn wire_key(key: HeightTileKey) -> HeightKey {
    HeightKey {
        space: key.space,
        level: key.level,
        x: key.x,
        z: key.z,
    }
}
fn render_key(key: HeightKey) -> HeightTileKey {
    HeightTileKey {
        space: key.space,
        level: key.level,
        x: key.x,
        z: key.z,
    }
}
fn render_tile(tile: &net::height::HeightTile) -> HeightTile {
    HeightTile {
        key: render_key(tile.key),
        source: tile.source,
        heights: tile.heights,
        known: tile.known,
        atlas: tile.atlas,
        tint: tile.tint,
        protected: tile.protected,
    }
}
fn intersects(a: [i64; 4], b: [i64; 4]) -> bool {
    a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3]
}
fn contains(a: [i64; 4], b: [i64; 4]) -> bool {
    a[0] <= b[0] && a[1] <= b[1] && a[2] >= b[2] && a[3] >= b[3]
}
fn adjacent(a: HeightTileKey, b: HeightTileKey) -> bool {
    if a.space != b.space {
        return false;
    }
    let (Some(a), Some(b)) = (a.bounds(), b.bounds()) else {
        return false;
    };
    ((a[2] == b[0] || a[0] == b[2]) && a[1] < b[3] && b[1] < a[3])
        || ((a[3] == b[1] || a[1] == b[3]) && a[0] < b[2] && b[0] < a[2])
}

fn page_near(page: PageId, near: Option<(u32, [i64; 4])>) -> bool {
    near.is_some_and(|(space, bounds)| {
        page.space.0 == space
            && intersects(
                bounds,
                [
                    i64::from(page.x) * 16,
                    i64::from(page.z) * 16,
                    (i64::from(page.x) + 1) * 16,
                    (i64::from(page.z) + 1) * 16,
                ],
            )
    })
}

#[path = "horizon_wire.rs"]
mod wire;
pub use wire::{
    WireError, decode_background_result, encode_background_task, execute_background_request,
};
#[derive(Clone, Copy, Debug)]
pub enum TaskContext {
    Plan { id: u64 },
    Proxy { key: LodKey, revision: u64 },
}
impl TaskContext {
    pub fn fail(self, owner: &mut ViewHorizon) {
        match self {
            Self::Plan { id } => owner.fail_plan(id),
            Self::Proxy { key, revision } => owner.fail_proxy(key, revision),
        }
    }
}
/// The caller supplies bounded JobSpec/Dispatch metadata; both result kinds are
/// immutable and carry their own source/plan guards for owner-thread completion.
pub enum BackgroundTask {
    Plan(Box<PlanTask>),
    Proxy(ProxyTask),
}
impl BackgroundTask {
    pub fn context(&self) -> TaskContext {
        match self {
            Self::Plan(task) => TaskContext::Plan { id: task.id },
            Self::Proxy(task) => TaskContext::Proxy {
                key: lod_key(task.stamp.page),
                revision: task.revision,
            },
        }
    }
    pub fn job_key(&self) -> jobs::JobKey {
        match self {
            Self::Plan(task) => jobs::JobKey {
                page: PageId {
                    space: SpaceId(task.space),
                    x: task.camera[0] as i32,
                    y: 0,
                    z: task.camera[1] as i32,
                },
                kind: 0x484c,
            },
            Self::Proxy(task) => jobs::JobKey {
                page: task.stamp.page,
                kind: 0x4850,
            },
        }
    }
    pub fn retained_bytes(&self) -> usize {
        match self {
            Self::Plan(task) => {
                std::mem::size_of::<PlanTask>()
                    + task.refine.capacity() * std::mem::size_of::<HeightKey>()
                    + task.certificates.capacity() * std::mem::size_of::<Certificate>()
                    + task.previous.capacity() * std::mem::size_of::<HeightTileKey>()
            }
            Self::Proxy(task) => {
                std::mem::size_of::<net::PublicGeometry>()
                    + task.input.palette.capacity() * std::mem::size_of::<net::PublicSurface>()
                    + task.input.indices.capacity()
            }
        }
    }
    pub fn cost(&self) -> jobs::WorkCost {
        jobs::WorkCost {
            cells: if matches!(self, Self::Proxy(_)) {
                65536
            } else {
                0
            },
            bytes: (self.retained_bytes() + 262144) as u64,
            ..jobs::WorkCost::ZERO
        }
    }
}
impl ViewHorizon {
    pub fn complete_background(
        &mut self,
        context: TaskContext,
        result: BackgroundResult,
    ) -> Result<bool, Error> {
        match result {
            BackgroundResult::Plan(Ok(plan)) => self.complete_plan(plan),
            BackgroundResult::Proxy(Ok(proxy)) => {
                self.complete_proxy(proxy).map_err(|_| Error::Capacity)
            }
            _ => {
                context.fail(self);
                Ok(false)
            }
        }
    }
}
pub enum BackgroundResult {
    Plan(Result<Planned, Error>),
    Proxy(Result<ProxyBuilt, ProxyError>),
}
#[cfg(not(target_arch = "wasm32"))]
pub fn native_pool(
    workers: usize,
    capacity: usize,
) -> Result<jobs::NativePool<BackgroundTask, BackgroundResult>, jobs::PoolError> {
    jobs::NativePool::new(
        workers,
        capacity,
        |task: BackgroundTask, cancel: &jobs::CancellationToken| {
            if cancel.is_cancelled() {
                return Err("cancelled horizon job".into());
            }
            let result = match task {
                BackgroundTask::Plan(task) => BackgroundResult::Plan(task.execute()),
                BackgroundTask::Proxy(task) => BackgroundResult::Proxy(task.execute()),
            };
            if cancel.is_cancelled() {
                return Err("cancelled horizon job".into());
            }
            Ok(result)
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn adaptive_1024_plan_refines_known_protected_hints_and_balances_unknown_near_cutout() {
        let camera = [0, 0];
        let near = [-64, -64, 80, 80];
        let result = PlanTask {
            id: 1,
            generation: 0,
            config: Config::default(),
            space: 0,
            camera,
            refine: vec![HeightKey {
                space: 0,
                level: 2,
                x: 20,
                z: 20,
            }],
            near_radius: 4,
            projection: None,
            certificates: Vec::new(),
            previous: Vec::new(),
        }
        .execute()
        .unwrap();
        assert!(result.plan.tiles.len() <= 512);
        assert!(
            result.plan.outer_bounds_pages[0] <= -1024 && result.plan.outer_bounds_pages[2] >= 1024
        );
        assert!(
            result
                .plan
                .tiles
                .iter()
                .all(|tile| !intersects(near, tile.key.bounds().unwrap()))
        );
        assert!(result.plan.tiles.iter().any(|tile| tile.key.level == 1
            && contains([1280, 1280, 1344, 1344], tile.key.bounds().unwrap())));
        for (i, a) in result.plan.tiles.iter().enumerate() {
            for b in result.plan.tiles.iter().skip(i + 1) {
                if adjacent(a.key, b.key) {
                    assert!(a.key.level.abs_diff(b.key.level) <= 1);
                }
            }
        }
        assert!(result.work.refinements > 0);
        assert!(result.work.adjacency_tests > 0);
        assert!(result.work.near_tiles_removed > 0);
        println!(
            "M5 adaptive1024 tiles{} refinements{} nearCut{} pairTests{}",
            result.plan.tiles.len(),
            result.work.refinements,
            result.work.near_tiles_removed,
            result.work.adjacency_tests
        );
    }
}
