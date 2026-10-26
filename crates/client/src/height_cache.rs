//! Public, versioned horizon owner. Staged writes never touch the active cover.
//! The caller supplies already-public height tiles and readiness of their exact
//! protected geometry; this module has no generator or raw-world access.
use render::{
    height_gpu::{HeightGpu, HeightGpuError, HeightHandle},
    height_tile::{HeightError, HeightPlan, HeightTile, HeightTileKey},
    lod::Edge,
    terrain_gpu::UploadBudget,
};
use std::{collections::BTreeMap, sync::Arc};
#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub max_tiles: usize,
    pub max_sources: usize,
    pub atlas_layers: u32,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            max_tiles: 512,
            max_sources: 2048,
            atlas_layers: 186,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidConfig,
    InvalidPlan,
    InvalidTile,
    Capacity,
    ConflictingSource,
    IncompatibleSeam,
    EpochExhausted,
    Gpu(HeightGpuError),
}
impl From<HeightGpuError> for Error {
    fn from(value: HeightGpuError) -> Self {
        Self::Gpu(value)
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Work {
    pub source_tiles: usize,
    pub staged: usize,
    pub released: usize,
    pub published: bool,
    pub held_previous: bool,
    pub seams_checked: usize,
    pub upload_bytes: u64,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlanWork {
    pub ancestor_lookups: u64,
    pub neighbour_lookups: u64,
    pub adjacent_pairs: usize,
}
#[derive(Clone)]
struct Resident {
    tile: Arc<HeightTile>,
    seams: u32,
    handle: HeightHandle,
    enabled: bool,
    drawable: bool,
}
pub struct HeightCache {
    config: Config,
    desired: BTreeMap<HeightTileKey, u32>,
    neighbours: Vec<(HeightTileKey, HeightTileKey)>,
    plan_work: PlanWork,
    seams_checked_epoch: Option<u64>,
    sources: BTreeMap<HeightTileKey, Arc<HeightTile>>,
    front: BTreeMap<HeightTileKey, Resident>,
    staged: BTreeMap<HeightTileKey, Resident>,
    retired: Vec<HeightHandle>,
    changes: Vec<(HeightHandle, bool)>,
    remove: Vec<HeightTileKey>,
    origin: [i64; 3],
    desired_epoch: u64,
    published_epoch: u64,
    dirty: bool,
    suppressed: Option<(u32, [i64; 4])>,
}
impl HeightCache {
    pub fn new(config: Config) -> Result<Self, Error> {
        if config.max_tiles == 0
            || config.max_tiles > 4096
            || config.max_sources < config.max_tiles
            || config.max_sources > 8192
            || config.atlas_layers == 0
        {
            return Err(Error::InvalidConfig);
        }
        Ok(Self {
            config,
            desired: BTreeMap::new(),
            neighbours: Vec::with_capacity(config.max_tiles * 4),
            plan_work: PlanWork::default(),
            seams_checked_epoch: None,
            sources: BTreeMap::new(),
            front: BTreeMap::new(),
            staged: BTreeMap::new(),
            retired: Vec::with_capacity(config.max_tiles * 2),
            changes: Vec::with_capacity(config.max_tiles * 2),
            remove: Vec::with_capacity(config.max_tiles),
            origin: [0; 3],
            desired_epoch: 0,
            published_epoch: 0,
            dirty: false,
            suppressed: None,
        })
    }
    pub fn pending_retirements(&self) -> usize {
        self.retired.len()
    }
    pub fn front_len(&self) -> usize {
        self.front.len()
    }
    pub fn staged_len(&self) -> usize {
        self.staged.len()
    }
    pub fn source_len(&self) -> usize {
        self.sources.len()
    }
    pub fn epoch(&self) -> u64 {
        self.published_epoch
    }
    pub fn plan_work(&self) -> PlanWork {
        self.plan_work
    }
    pub fn front_source(&self, key: HeightTileKey) -> Option<u64> {
        self.front.get(&key).map(|r| r.tile.source)
    }
    pub fn wanted(&self) -> impl ExactSizeIterator<Item = HeightTileKey> + '_ {
        self.desired.keys().copied()
    }
    pub fn missing(&self) -> impl Iterator<Item = HeightTileKey> + '_ {
        self.desired
            .keys()
            .filter(|k| !self.sources.contains_key(k))
            .copied()
    }
    /// Plans are generated on page/quality transitions, not every rendered frame.
    /// Near tiles may be omitted only by the caller that owns their exact cover.
    pub fn set_plan(&mut self, plan: &HeightPlan) -> Result<bool, Error> {
        if plan.tiles.len() > self.config.max_tiles {
            return Err(Error::Capacity);
        }
        if plan.tiles.len() == self.desired.len()
            && plan
                .tiles
                .windows(2)
                .all(|tiles| tiles[0].key < tiles[1].key)
            && plan
                .tiles
                .iter()
                .all(|tile| self.desired.get(&tile.key) == Some(&tile.seams))
        {
            return Ok(false);
        }
        let mut desired = BTreeMap::new();
        for tile in &plan.tiles {
            if tile.key.bounds().is_none()
                || tile.seams & !15 != 0
                || desired.insert(tile.key, tile.seams).is_some()
            {
                return Err(Error::InvalidPlan);
            }
        }
        let (neighbours, plan_work) = index_plan(&desired)?;
        if self.desired == desired {
            return Ok(false);
        }
        self.desired_epoch = self
            .desired_epoch
            .checked_add(1)
            .ok_or(Error::EpochExhausted)?;
        self.desired = desired;
        self.neighbours = neighbours;
        self.plan_work = plan_work;
        self.dirty = true;
        self.sources.retain(|key, _| {
            self.desired.contains_key(key)
                || self.front.contains_key(key)
                || self.staged.contains_key(key)
        });
        Ok(true)
    }
    pub fn ingest(&mut self, tile: Arc<HeightTile>) -> Result<bool, Error> {
        self.ingest_inner(tile, false)
    }
    /// Caller has validated an authenticated, nonempty exact-column manifest.
    /// Only an unchanged leaf's unprotected→fully protected representation can
    /// change at the same world source. It remains hidden until protected_ready.
    pub fn ingest_exact_promotion(&mut self, tile: Arc<HeightTile>) -> Result<bool, Error> {
        self.ingest_inner(tile, true)
    }
    fn ingest_inner(&mut self, tile: Arc<HeightTile>, exact: bool) -> Result<bool, Error> {
        tile.validate(self.config.atlas_layers)
            .map_err(|_| Error::InvalidTile)?;
        if !self.desired.contains_key(&tile.key) {
            return Ok(false);
        }
        if let Some(old) = self.sources.get(&tile.key) {
            if tile.source < old.source {
                return Ok(false);
            }
            if tile.source == old.source {
                if *old.as_ref() == *tile {
                    return Ok(false);
                }
                if !exact || !exact_promotion(old, &tile) {
                    return Err(Error::ConflictingSource);
                }
            }
        } else if self.sources.len() >= self.config.max_sources {
            return Err(Error::Capacity);
        }
        self.desired_epoch = self
            .desired_epoch
            .checked_add(1)
            .ok_or(Error::EpochExhausted)?;
        self.sources.insert(tile.key, tile);
        self.dirty = true;
        Ok(true)
    }
    fn current(&self, key: &HeightTileKey, resident: &Resident) -> bool {
        self.desired.get(key) == Some(&resident.seams)
            && self
                .sources
                .get(key)
                .is_some_and(|tile| Arc::ptr_eq(tile, &resident.tile))
    }
    /// Work is bounded by actual changed tiles and the explicit upload count.
    /// If data or budget is missing, all prior active handles stay enabled.
    pub fn pump(
        &mut self,
        gpu: &mut HeightGpu,
        queue: &wgpu::Queue,
        budget: &mut UploadBudget,
        max_uploads: usize,
        protected_ready: impl Fn(&HeightTile) -> bool,
    ) -> Result<Work, Error> {
        self.pump_inner(gpu, queue, budget, max_uploads, protected_ready, true)
    }
    /// Preload received tiles while authority leases or exact replacement
    /// manifests are still arriving. Every new GPU handle remains inactive;
    /// this path can never replace or enable the active cover.
    pub fn pump_staging(
        &mut self,
        gpu: &mut HeightGpu,
        queue: &wgpu::Queue,
        budget: &mut UploadBudget,
        max_uploads: usize,
    ) -> Result<Work, Error> {
        self.pump_inner(gpu, queue, budget, max_uploads, |_| false, false)
    }
    fn pump_inner(
        &mut self,
        gpu: &mut HeightGpu,
        queue: &wgpu::Queue,
        budget: &mut UploadBudget,
        max_uploads: usize,
        protected_ready: impl Fn(&HeightTile) -> bool,
        allow_publish: bool,
    ) -> Result<Work, Error> {
        let before = budget.remaining_bytes;
        let mut work = Work::default();
        while let Some(handle) = self.retired.last().copied() {
            if budget.remaining_bytes < 4 {
                break;
            }
            gpu.release(queue, handle, budget)?;
            self.retired.pop();
            work.released += 1;
        }
        self.remove.clear();
        for (key, resident) in &self.staged {
            if !self.current(key, resident) {
                self.remove.push(*key);
            }
        }
        for key in &self.remove {
            let resident = self.staged.get(key).expect("collected staged");
            // Exact promotion borrows its unchanged front handle. Discarding
            // an obsolete staged representation must never release that front.
            if self
                .front
                .get(key)
                .is_some_and(|front| front.handle == resident.handle)
            {
                self.staged.remove(key);
                continue;
            }
            if budget.remaining_bytes < 4 {
                break;
            }
            gpu.release(queue, resident.handle, budget)?;
            self.staged.remove(key);
            work.released += 1;
        }
        if !self.dirty {
            work.upload_bytes = before - budget.remaining_bytes;
            return Ok(work);
        }
        for (key, seams) in &self.desired {
            if work.staged == max_uploads {
                break;
            }
            if self
                .front
                .get(key)
                .is_some_and(|resident| self.current(key, resident))
                || self.staged.contains_key(key)
            {
                continue;
            }
            let Some(tile) = self.sources.get(key) else {
                continue;
            };
            if let Some(front) = self.front.get(key)
                && tile.protected == [255; 32]
                && same_surface(&front.tile, tile)
            {
                // No height triangles survive a whole-column replacement. Keep
                // the existing GPU payload/handle untouched; atomic publication
                // disables it only after every exact proxy is ready. This also
                // preserves the GPU's strict source/hash conflict contract.
                self.staged.insert(
                    *key,
                    Resident {
                        tile: tile.clone(),
                        seams: *seams,
                        handle: front.handle,
                        enabled: false,
                        drawable: false,
                    },
                );
                work.staged += 1;
                continue;
            }
            // Exact bounded per-tile cost. Insufficient budget makes no write.
            if budget.remaining_bytes < render::height_gpu::TILE_BYTES + 64 {
                break;
            }
            let handle = gpu.upload_staged(queue, tile, *seams, self.origin, budget)?;
            self.staged.insert(
                *key,
                Resident {
                    tile: tile.clone(),
                    seams: *seams,
                    handle,
                    enabled: false,
                    drawable: tile.drawable_cells(*seams) > 0,
                },
            );
            work.staged += 1;
        }
        if !allow_publish {
            work.held_previous = true;
            work.upload_bytes = before - budget.remaining_bytes;
            return Ok(work);
        }
        let mut ready = true;
        for key in self.desired.keys() {
            let resident = self
                .staged
                .get(key)
                .filter(|r| self.current(key, r))
                .or_else(|| self.front.get(key).filter(|r| self.current(key, r)));
            if resident.is_none_or(|r| {
                r.tile.protected.iter().any(|bits| *bits != 0) && !protected_ready(&r.tile)
            }) {
                ready = false;
                break;
            }
        }
        if ready && self.seams_checked_epoch != Some(self.desired_epoch) {
            // Only a completed cover can replace the previous cover. This also
            // checks same-level edges, since editing a shared endpoint changes
            // both neighbours even without a LOD transition.
            for (a, b) in &self.neighbours {
                let a = self.sources.get(a).expect("ready source");
                let b = self.sources.get(b).expect("ready source");
                work.seams_checked += 1;
                if check_seam(a, b).is_err() {
                    ready = false;
                }
            }
            if ready {
                self.seams_checked_epoch = Some(self.desired_epoch);
            }
        }
        if ready {
            self.changes.clear();
            self.remove.clear();
            for (key, resident) in &self.front {
                if !self.current(key, resident) {
                    self.changes.push((resident.handle, false));
                    self.remove.push(*key);
                }
            }
            for (key, resident) in &self.staged {
                if self.current(key, resident) {
                    self.changes
                        .push((resident.handle, resident.drawable && !self.suppresses(*key)));
                }
            }
            self.changes.sort_unstable_by_key(|(handle, _)| handle.slot);
            self.changes.dedup();
            let bytes = self.changes.len() as u64 * 4;
            if budget.remaining_bytes >= bytes {
                gpu.set_enabled_batch(queue, &self.changes, budget)?;
                for key in &self.remove {
                    let old = self.front.remove(key).expect("collected front");
                    if !self
                        .staged
                        .get(key)
                        .is_some_and(|new| new.handle == old.handle)
                    {
                        self.retired.push(old.handle);
                    }
                }
                self.remove.clear();
                for (key, resident) in &self.staged {
                    if self.current(key, resident) {
                        self.remove.push(*key);
                    }
                }
                for key in &self.remove {
                    let mut resident = self.staged.remove(key).expect("collected staging");
                    resident.enabled = resident.drawable && !self.suppresses(*key);
                    self.front.insert(*key, resident);
                }
                self.published_epoch = self.desired_epoch;
                self.dirty = false;
                work.published = true;
                self.sources.retain(|key, _| {
                    self.desired.contains_key(key) || self.staged.contains_key(key)
                });
            }
        }
        work.source_tiles = self.sources.len();
        work.held_previous = self.dirty;
        work.upload_bytes = before - budget.remaining_bytes;
        Ok(work)
    }
    pub fn rebase(
        &mut self,
        gpu: &mut HeightGpu,
        queue: &wgpu::Queue,
        origin: [i64; 3],
        budget: &mut UploadBudget,
    ) -> Result<u64, Error> {
        let bytes = gpu.rebase(queue, origin, budget)?;
        self.origin = origin;
        Ok(bytes)
    }
    fn suppresses(&self, key: HeightTileKey) -> bool {
        self.suppressed
            .is_some_and(|(space, bounds)| intersects(key, space, bounds))
    }
    /// Move the near Unknown cutout before drawing, even while the asynchronous
    /// next plan is unfinished. The full flag budget is checked before writing;
    /// later cover publication retains this guard and cannot fill a natural hole.
    pub fn suppress_region(
        &mut self,
        gpu: &mut HeightGpu,
        queue: &wgpu::Queue,
        space: u32,
        bounds: [i64; 4],
        budget: &mut UploadBudget,
    ) -> Result<u64, Error> {
        if bounds[0] >= bounds[2] || bounds[1] >= bounds[3] {
            return Err(Error::InvalidPlan);
        }
        if self.suppressed == Some((space, bounds)) {
            return Ok(0);
        }
        self.changes.clear();
        for (key, resident) in &self.front {
            let enabled = resident.drawable && !intersects(*key, space, bounds);
            if resident.enabled != enabled {
                self.changes.push((resident.handle, enabled));
            }
        }
        self.changes.sort_unstable_by_key(|(handle, _)| handle.slot);
        let bytes = gpu.set_enabled_batch(queue, &self.changes, budget)?;
        self.suppressed = Some((space, bounds));
        for (key, resident) in &mut self.front {
            resident.enabled = resident.drawable && !intersects(*key, space, bounds);
        }
        Ok(bytes)
    }
}
fn exact_promotion(old: &HeightTile, new: &HeightTile) -> bool {
    old.key.level == 0
        && same_surface(old, new)
        && old.protected == [0; 32]
        && new.protected == [255; 32]
}
fn same_surface(old: &HeightTile, new: &HeightTile) -> bool {
    old.key == new.key
        && old.source == new.source
        && old.heights == new.heights
        && old.known == new.known
        && old.atlas == new.atlas
        && old.tint == new.tint
}
fn intersects(key: HeightTileKey, space: u32, bounds: [i64; 4]) -> bool {
    if key.space != space {
        return false;
    }
    let Some(tile) = key.bounds() else {
        return true;
    };
    tile[0] < bounds[2] && bounds[0] < tile[2] && tile[1] < bounds[3] && bounds[1] < tile[3]
}
type NeighbourPairs = Vec<(HeightTileKey, HeightTileKey)>;
fn index_plan(desired: &BTreeMap<HeightTileKey, u32>) -> Result<(NeighbourPairs, PlanWork), Error> {
    use crate::height_neighbors::ancestor_neighbour;
    use render::height_tile::{MAX_LEVEL, PlannedHeightTile};
    let mut work = PlanWork::default();
    // Dyadic rectangles overlap exactly when one selected key is an ancestor
    // of another. Same-level duplicates were rejected while building the map.
    for key in desired.keys() {
        for level in key.level + 1..=MAX_LEVEL {
            let divisor = 1_i32 << (level - key.level);
            let parent = HeightTileKey {
                level,
                x: key.x.div_euclid(divisor),
                z: key.z.div_euclid(divisor),
                ..*key
            };
            work.ancestor_lookups += 1;
            if desired.contains_key(&parent) {
                return Err(Error::InvalidPlan);
            }
        }
    }
    let limit = desired.len().checked_mul(4).ok_or(Error::Capacity)?;
    let mut pairs = Vec::with_capacity(limit);
    for (&key, &seams) in desired {
        for level in key.level..=MAX_LEVEL {
            for edge in 0..4 {
                work.neighbour_lookups += 1;
                let Some(other) = ancestor_neighbour(key, edge, level) else {
                    continue;
                };
                if level == key.level && key >= other {
                    continue;
                }
                let Some(&other_seams) = desired.get(&other) else {
                    continue;
                };
                if !adjacent_keys(key, other) {
                    continue;
                }
                validate_neighbours(
                    PlannedHeightTile { key, seams },
                    PlannedHeightTile {
                        key: other,
                        seams: other_seams,
                    },
                )?;
                // A disjoint rectangular forest has fewer than four edge pairs
                // per leaf. Keep an explicit bound even for malformed callers.
                if pairs.len() == limit {
                    return Err(Error::Capacity);
                }
                pairs.push((key, other));
            }
        }
    }
    work.adjacent_pairs = pairs.len();
    Ok((pairs, work))
}
fn adjacent_keys(a: HeightTileKey, b: HeightTileKey) -> bool {
    if a.space != b.space {
        return false;
    }
    let a = a.bounds().expect("validated");
    let b = b.bounds().expect("validated");
    ((a[2] == b[0] || a[0] == b[2]) && a[1] < b[3] && b[1] < a[3])
        || ((a[3] == b[1] || a[1] == b[3]) && a[0] < b[2] && b[0] < a[2])
}
fn validate_neighbours(
    a: render::height_tile::PlannedHeightTile,
    b: render::height_tile::PlannedHeightTile,
) -> Result<(), Error> {
    let (fine, coarse) = if a.key.level <= b.key.level {
        (a, b)
    } else {
        (b, a)
    };
    if fine.key.level == coarse.key.level {
        return Ok(());
    }
    let f = fine.key.bounds().ok_or(Error::InvalidPlan)?;
    let c = coarse.key.bounds().ok_or(Error::InvalidPlan)?;
    let bit = if f[2] == c[0] && f[1] < c[3] && c[1] < f[3] {
        2
    } else if f[0] == c[2] && f[1] < c[3] && c[1] < f[3] {
        1
    } else if f[3] == c[1] && f[0] < c[2] && c[0] < f[2] {
        8
    } else if f[1] == c[3] && f[0] < c[2] && c[0] < f[2] {
        4
    } else {
        return Ok(());
    };
    if fine.key.level.checked_add(1) != Some(coarse.key.level) || fine.seams & bit == 0 {
        return Err(Error::InvalidPlan);
    }
    Ok(())
}
fn check_seam(a: &HeightTile, b: &HeightTile) -> Result<(), HeightError> {
    let (a, b) = if a.key.level > b.key.level {
        (b, a)
    } else {
        (a, b)
    };
    let ab = a.key.bounds().ok_or(HeightError::InvalidKey)?;
    let bb = b.key.bounds().ok_or(HeightError::InvalidKey)?;
    let edge = if ab[2] == bb[0] {
        Edge::East
    } else if ab[0] == bb[2] {
        Edge::West
    } else if ab[3] == bb[1] {
        Edge::South
    } else {
        Edge::North
    };
    if a.key.level != b.key.level {
        return a.compatible_coarse(b, edge);
    }
    for along in 0..17 {
        let (ai, bi) = match edge {
            Edge::East => (16 + 17 * along, 17 * along),
            Edge::West => (17 * along, 16 + 17 * along),
            Edge::South => (along + 17 * 16, along),
            Edge::North => (along, along + 17 * 16),
        };
        let ak = a.sample_known(ai % 17, ai / 17);
        let bk = b.sample_known(bi % 17, bi / 17);
        if ak != bk || ak && a.heights[ai] != b.heights[bi] {
            return Err(HeightError::SourceMismatch);
        }
    }
    Ok(())
}

#[cfg(test)]
mod indexed_tests {
    use super::*;
    use render::height_tile::PlannedHeightTile;
    use std::collections::BTreeSet;
    fn key(level: u8, x: i32, z: i32) -> HeightTileKey {
        HeightTileKey {
            space: 0,
            level,
            x,
            z,
        }
    }
    #[test]
    fn four_thousand_leaf_plan_has_linear_bounded_validation_and_exact_grid_edges() {
        let mut cache = HeightCache::new(Config {
            max_tiles: 4096,
            max_sources: 8192,
            ..Default::default()
        })
        .unwrap();
        let plan = HeightPlan {
            tiles: (-32..32)
                .flat_map(|x| {
                    (-32..32).map(move |z| PlannedHeightTile {
                        key: key(0, x, z),
                        seams: 0,
                    })
                })
                .collect(),
            visited_nodes: 4096,
            outer_bounds_pages: [-32, -32, 32, 32],
        };
        assert!(cache.set_plan(&plan).unwrap());
        let work = cache.plan_work();
        assert_eq!(work.adjacent_pairs, 2 * 64 * 63);
        assert_eq!(work.ancestor_lookups, 4096 * 10);
        assert!(work.neighbour_lookups <= 4096 * 4 * 11);
        let scope = telemetry::AllocationScope::begin().unwrap();
        for _ in 0..100 {
            assert!(!cache.set_plan(&plan).unwrap());
        }
        let counts = scope.finish();
        assert_eq!(counts.allocating_calls(), 0);
        assert_eq!(counts.dealloc_calls, 0);
        let mut invalid = HeightPlan {
            tiles: vec![
                PlannedHeightTile {
                    key: key(1, 0, 0),
                    seams: 0,
                },
                PlannedHeightTile {
                    key: key(0, 0, 0),
                    seams: 0,
                },
            ],
            visited_nodes: 2,
            outer_bounds_pages: [0; 4],
        };
        assert_eq!(cache.set_plan(&invalid), Err(Error::InvalidPlan));
        invalid.tiles[1].key = key(0, -1, 0);
        invalid.tiles[0].key = key(2, 0, 0);
        invalid.tiles[1].seams = 2;
        assert_eq!(cache.set_plan(&invalid), Err(Error::InvalidPlan));
        assert_eq!(cache.wanted().count(), 4096);
        assert_eq!(cache.plan_work(), work);
    }
    #[test]
    fn indexed_pairs_equal_independent_all_pairs_on_negative_mixed_forests_and_spaces() {
        for seed in 0..16_u64 {
            let mut state = seed + 1;
            let mut next = || {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                state
            };
            let mut keys = Vec::new();
            for x in -4..=4 {
                for z in -4..=4 {
                    if next() % 5 == 0 {
                        continue;
                    }
                    if next() % 2 == 0 {
                        keys.push(key(2, x, z));
                    } else {
                        for dx in 0..2 {
                            for dz in 0..2 {
                                if next() % 7 != 0 {
                                    keys.push(key(1, 2 * x + dx, 2 * z + dz));
                                }
                            }
                        }
                    }
                }
            }
            keys.push(HeightTileKey {
                space: 1,
                ..key(1, 0, 0)
            });
            let mut desired: BTreeMap<_, _> = keys.iter().map(|&k| (k, 0)).collect();
            let mut expected = BTreeSet::new();
            for (i, &a) in keys.iter().enumerate() {
                for &b in &keys[i + 1..] {
                    if a.space != b.space {
                        continue;
                    }
                    let ab = a.bounds().unwrap();
                    let bb = b.bounds().unwrap();
                    let x_overlap = ab[0] < bb[2] && bb[0] < ab[2];
                    let z_overlap = ab[1] < bb[3] && bb[1] < ab[3];
                    assert!(!(x_overlap && z_overlap));
                    if !(((ab[2] == bb[0] || bb[2] == ab[0]) && z_overlap)
                        || ((ab[3] == bb[1] || bb[3] == ab[1]) && x_overlap))
                    {
                        continue;
                    }
                    expected.insert(if a < b { (a, b) } else { (b, a) });
                    if a.level != b.level {
                        let (fine, fb, coarse) = if a.level < b.level {
                            (a, ab, bb)
                        } else {
                            (b, bb, ab)
                        };
                        let mask = if fb[2] == coarse[0] {
                            2
                        } else if fb[0] == coarse[2] {
                            1
                        } else if fb[3] == coarse[1] {
                            8
                        } else {
                            4
                        };
                        *desired.get_mut(&fine).unwrap() |= mask;
                    }
                }
            }
            let (pairs, work) = index_plan(&desired).unwrap();
            let count = pairs.len();
            let actual = pairs
                .into_iter()
                .map(|(a, b)| if a < b { (a, b) } else { (b, a) })
                .collect::<BTreeSet<_>>();
            assert_eq!(actual, expected, "forest seed{seed}");
            assert_eq!(count, actual.len(), "duplicate edge seed{seed}");
            assert_eq!(work.adjacent_pairs, count);
        }
    }
}
