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
        for (i, a) in plan.tiles.iter().enumerate() {
            for b in &plan.tiles[i + 1..] {
                if a.key.space != b.key.space {
                    continue;
                }
                validate_neighbours(*a, *b)?;
                let a = a.key.bounds().ok_or(Error::InvalidPlan)?;
                let b = b.key.bounds().ok_or(Error::InvalidPlan)?;
                if a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3] {
                    return Err(Error::InvalidPlan);
                }
            }
        }
        if self.desired == desired {
            return Ok(false);
        }
        self.desired_epoch = self
            .desired_epoch
            .checked_add(1)
            .ok_or(Error::EpochExhausted)?;
        self.desired = desired;
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
                .values()
                .any(|front| front.handle == resident.handle)
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
        if ready {
            // Only a completed cover can replace the previous cover. This also
            // checks same-level edges, since editing a shared endpoint changes
            // both neighbours even without a LOD transition.
            for (i, a) in self.desired.keys().enumerate() {
                for b in self.desired.keys().skip(i + 1) {
                    let a = self.sources.get(a).expect("ready source");
                    let b = self.sources.get(b).expect("ready source");
                    if adjacent(a, b) {
                        work.seams_checked += 1;
                        if check_seam(a, b).is_err() {
                            ready = false;
                        }
                    }
                }
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
fn adjacent(a: &HeightTile, b: &HeightTile) -> bool {
    if a.key.space != b.key.space {
        return false;
    }
    let a = a.key.bounds().expect("validated");
    let b = b.key.bounds().expect("validated");
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
