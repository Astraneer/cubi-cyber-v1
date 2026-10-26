//! Bounded public projection leases and immutable worker cache. Public IDs are
//! global monotonic counters; private World/HID versions never become wire IDs.
use crate::{
    cold_projection::{BuiltColumn, Certificate, ColumnKey, ColumnSnapshot, ContentWitness},
    height_index::HeightEditIndex,
    lod,
};
use core_types::{Cell, PageId, Tick, Version};
use gen_authority::ServerGenerator;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use world::{World, WorldChange};
static NEXT_PUBLIC: AtomicU64 = AtomicU64::new(1);
/// Shared far512 + reserved raw256, for at most64 real connections. Larger
/// optional per-peer configurations still respect this total metadata bound.
pub const MAX_CONTENT_HEADS: usize = 64 * (512 + 256);
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_connections: usize,
    pub max_pages_per_connection: usize,
    pub max_inflight: usize,
    pub cache_columns: usize,
    pub cache_bytes: usize,
    /// Diagnostic-only acceptance delay; production remains zero.
    pub completion_delay_ticks: u8,
    pub capture: crate::cold_projection::Limits,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_connections: 64,
            max_pages_per_connection: 512,
            max_inflight: 4,
            cache_columns: 128,
            cache_bytes: 32 * 1024 * 1024,
            completion_delay_ticks: 0,
            capture: crate::cold_projection::Limits::default(),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Bounds,
    Capacity,
    Overflow,
    Spawn,
    Uninterested,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Head {
    pub source: Version,
    pub view: u64,
}
#[derive(Clone)]
pub struct WaterBody {
    pub head: Head,
    pub water_view: u64,
    pub column_source: u64,
    pub cells: Arc<[u8; 5832]>,
    pub tints: [u16; 256],
}
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Work {
    pub submitted: u64,
    pub completed: u64,
    pub stale: u64,
    pub failed: u64,
    pub deferred: u64,
    pub probes: u64,
    pub generated_columns: u64,
    pub public_baseline_columns: u64,
    pub public_baseline_scratch_reads: u64,
    pub public_baseline_scratch_writes: u64,
    pub tint_samples: u64,
    pub public_witness_bytes: u64,
    pub exterior: crate::public_projection::Work,
    pub generation_scratch_reads: u64,
    pub generation_scratch_writes: u64,
    pub snapshot_lookups: u64,
    pub captured_edit_cells: u64,
    pub captured_edit_bytes: u64,
    pub geometry_cells: u64,
    pub halo_samples: u64,
    pub public_copy_bytes: u64,
    pub evicted: u64,
    pub oldest_age: u64,
}
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Stats {
    pub public_ids: u64,
    pub witness_rows: usize,
    pub witness_digest_bytes: usize,
    /// Logical key+value storage; allocator node/header overhead is additional.
    pub witness_row_bytes: usize,
    pub connections: usize,
    pub raw_connections: usize,
    pub raw_leases: usize,
    pub leases: usize,
    pub columns: usize,
    pub heads: usize,
    pub queued: usize,
    pub running: usize,
    pub cache_entries: usize,
    pub cache_units: usize,
    pub cache_bytes: usize,
    pub changed_geometry: usize,
    pub changed_water: usize,
}
#[derive(Clone, Copy)]
struct PageHead {
    head: Head,
    water_view: u64,
}
struct Column {
    registration: u64,
    certificate: Option<Arc<Certificate>>,
    heads: [Option<PageHead>; 8],
    queued: bool,
    running: Option<jobs::JobId>,
    first: Tick,
    retry_after: Tick,
}
struct PageBody {
    geometry: Option<Arc<lod::PublicGeometryData>>,
    sealed_cells: Option<Arc<[Cell; 4096]>>,
    cells: Arc<[u8; 5832]>,
    witness: ContentWitness,
}
struct ColumnBody {
    certificate: Arc<Certificate>,
    pages: [PageBody; 8],
    tints: [u16; 256],
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct CacheKey(ColumnKey);
struct Cached {
    data: Arc<ColumnBody>,
    used: u64,
    bytes: usize,
    units: usize,
}
struct Running {
    column: ColumnKey,
    registration: u64,
    submitted: Tick,
}
pub struct Owner {
    limits: Limits,
    pool: jobs::NativePool<Box<ColumnSnapshot>, Box<BuiltColumn>>,
    peers: BTreeMap<u64, BTreeSet<PageId>>,
    raw_peers: BTreeMap<u64, BTreeSet<PageId>>,
    connections: BTreeSet<u64>,
    changed_geometry: BTreeSet<PageId>,
    changed_water: BTreeSet<PageId>,
    leases: BTreeMap<PageId, usize>,
    columns: BTreeMap<ColumnKey, Column>,
    queue: VecDeque<ColumnKey>,
    running: BTreeMap<jobs::JobId, Running>,
    delayed: VecDeque<(Tick, jobs::Completion<Box<BuiltColumn>>)>,
    cache: BTreeMap<CacheKey, Cached>,
    lru: BTreeSet<(u64, CacheKey)>,
    cache_bytes: usize,
    cache_units: usize,
    next_job: u64,
    next_registration: u64,
    next_use: u64,
    budget_tick: Option<Tick>,
    submissions: usize,
    completions: usize,
    receipts: usize,
    probes: usize,
    public_ids: u64,
    witnesses: BTreeMap<PageId, ContentWitness>,
}
impl Owner {
    pub fn new(generator: Arc<ServerGenerator>, limits: Limits) -> Result<Self, Error> {
        if limits.max_connections == 0
            || limits.max_connections > 64
            || limits.max_pages_per_connection == 0
            || limits.max_pages_per_connection > 4096
            || limits.max_inflight == 0
            || limits.max_inflight > 8
            || limits.cache_columns == 0
            || limits.cache_columns > 1024
            || limits.cache_bytes == 0
            || limits.cache_bytes > 128 * 1024 * 1024
            || limits.completion_delay_ticks > 20
        {
            return Err(Error::Bounds);
        }
        let pool = jobs::NativePool::new(
            2,
            8,
            move |snapshot: Box<ColumnSnapshot>, cancel: &jobs::CancellationToken| {
                snapshot
                    .execute(&generator, || cancel.is_cancelled())
                    .map_err(|e| e.to_string())
            },
        )
        .map_err(|_| Error::Spawn)?;
        Ok(Self {
            limits,
            pool,
            peers: BTreeMap::new(),
            raw_peers: BTreeMap::new(),
            connections: BTreeSet::new(),
            changed_geometry: BTreeSet::new(),
            changed_water: BTreeSet::new(),
            leases: BTreeMap::new(),
            columns: BTreeMap::new(),
            queue: VecDeque::new(),
            running: BTreeMap::new(),
            delayed: VecDeque::with_capacity(limits.max_inflight),
            cache: BTreeMap::new(),
            lru: BTreeSet::new(),
            cache_bytes: 0,
            cache_units: 0,
            next_job: 1,
            next_registration: 1,
            next_use: 1,
            budget_tick: None,
            submissions: 0,
            completions: 0,
            receipts: 0,
            probes: 0,
            public_ids: 0,
            witnesses: BTreeMap::new(),
        })
    }
    pub fn interest(&mut self, connection: u64, page: PageId, now: Tick) -> Result<bool, Error> {
        if canonical(page).is_none() {
            return Err(Error::Bounds);
        }
        self.interest_inner(connection, page, now, false)
    }
    /// A separately bounded near lease. Forgetting far geometry cannot release
    /// the same page while its canonical raw sanitizer still needs the body.
    pub fn interest_raw(
        &mut self,
        connection: u64,
        page: PageId,
        now: Tick,
    ) -> Result<bool, Error> {
        if canonical(page).is_none() {
            return Err(Error::Bounds);
        }
        self.interest_inner(connection, page, now, true)
    }
    fn interest_inner(
        &mut self,
        connection: u64,
        page: PageId,
        now: Tick,
        raw: bool,
    ) -> Result<bool, Error> {
        if [page.x, page.y, page.z]
            .into_iter()
            .any(|v| !(i32::MIN / 16..=i32::MAX / 16).contains(&v))
        {
            return Err(Error::Bounds);
        }
        if !self.connections.contains(&connection)
            && self.connections.len() >= self.limits.max_connections
        {
            return Err(Error::Capacity);
        }
        if !self.leases.contains_key(&page) && self.leases.len() >= MAX_CONTENT_HEADS {
            return Err(Error::Capacity);
        }
        let peers = if raw {
            &mut self.raw_peers
        } else {
            &mut self.peers
        };
        let peer = peers.entry(connection).or_default();
        if peer.contains(&page) {
            return Ok(false);
        }
        let maximum = if raw {
            crate::MAX_AOI_PAGES
        } else {
            self.limits.max_pages_per_connection
        };
        if peer.len() >= maximum {
            return Err(Error::Capacity);
        }
        if let Some(key) = canonical(page)
            && !self.columns.contains_key(&key)
        {
            let registration = self.next_registration;
            self.next_registration = registration.checked_add(1).ok_or(Error::Overflow)?;
            self.columns.insert(
                key,
                Column {
                    registration,
                    certificate: None,
                    heads: [None; 8],
                    queued: false,
                    running: None,
                    first: now,
                    retry_after: now,
                },
            );
        }
        peer.insert(page);
        self.connections.insert(connection);
        *self.leases.entry(page).or_default() += 1;
        if let Some(key) = canonical(page) {
            self.request(key, now)?;
        }
        Ok(true)
    }
    pub fn request(&mut self, key: ColumnKey, now: Tick) -> Result<bool, Error> {
        let Some(column) = self.columns.get_mut(&key) else {
            return Err(Error::Uninterested);
        };
        if column.queued || column.running.is_some() {
            return Ok(false);
        }
        column.queued = true;
        column.first = now;
        column.retry_after = now;
        self.queue.push_back(key);
        Ok(true)
    }
    pub fn forget(&mut self, connection: u64, page: PageId) {
        self.forget_inner(connection, page, false);
    }
    pub fn forget_raw(&mut self, connection: u64, page: PageId) {
        self.forget_inner(connection, page, true);
    }
    fn forget_inner(&mut self, connection: u64, page: PageId, raw: bool) {
        let peers = if raw {
            &mut self.raw_peers
        } else {
            &mut self.peers
        };
        let Some(peer) = peers.get_mut(&connection) else {
            return;
        };
        if !peer.remove(&page) {
            return;
        }
        let count = self.leases.get_mut(&page).expect("leased page");
        *count -= 1;
        if *count > 0 {
            return;
        }
        self.leases.remove(&page);
        self.witnesses.remove(&page);
        self.changed_geometry.remove(&page);
        self.changed_water.remove(&page);
        if let Some(key) = canonical(page) {
            if let Some(column) = self.columns.get_mut(&key) {
                column.heads[page.y as usize] = None;
            }
            let wanted = (0..8).any(|y| {
                self.leases.contains_key(&PageId {
                    space: core_types::SpaceId(0),
                    x: key.x,
                    y,
                    z: key.z,
                })
            });
            if !wanted {
                self.columns.remove(&key);
                self.queue.retain(|p| *p != key);
            }
        }
    }
    pub fn forget_peer(&mut self, connection: u64) {
        if let Some(peer) = self.peers.get(&connection) {
            let pages: Vec<_> = peer.iter().copied().collect();
            for page in pages {
                self.forget(connection, page);
            }
        }
        self.peers.remove(&connection);
        if !self.raw_peers.contains_key(&connection) {
            self.connections.remove(&connection);
        }
    }
    pub fn forget_raw_peer(&mut self, connection: u64) {
        if let Some(peer) = self.raw_peers.get(&connection) {
            let pages: Vec<_> = peer.iter().copied().collect();
            for page in pages {
                self.forget_raw(connection, page);
            }
        }
        self.raw_peers.remove(&connection);
        if !self.peers.contains_key(&connection) {
            self.connections.remove(&connection);
        }
    }
    /// Coalesced notifications for changes to an already published/leased head.
    /// Initial requests already wait for their head and need no extra wake. The
    /// set is bounded by active leases; forgetting the last lease removes it.
    pub fn take_changed_geometry(&mut self) -> Option<PageId> {
        self.changed_geometry.pop_first()
    }
    /// Water may change after a neighbouring fluid/exposure input while the
    /// central geometry head stays identical. Notify that stream independently.
    pub fn take_changed_water(&mut self) -> Option<PageId> {
        self.changed_water.pop_first()
    }
    /// Changed-page probes are bounded to25 neighbouring column keys. LIGHT is
    /// ignored; no cold-world traversal or all-head invalidation is performed.
    pub fn observe(&mut self, changes: &[WorldChange], now: Tick) {
        let channels = crate::far_water::capture::CHANNELS;
        for page in changes
            .iter()
            .flat_map(|c| &c.pages)
            .filter(|p| p.created || p.removed || p.channels.intersects(channels))
        {
            if page.page.space.0 != 0 {
                continue;
            }
            for dz in -2..=2 {
                for dx in -2..=2 {
                    let (Some(x), Some(z)) =
                        (page.page.x.checked_add(dx), page.page.z.checked_add(dz))
                    else {
                        continue;
                    };
                    let key = ColumnKey { x, z };
                    if let Some(column) = self.columns.get_mut(&key)
                        && !column.queued
                        && column.running.is_none()
                    {
                        column.queued = true;
                        column.first = now;
                        column.retry_after = now;
                        self.queue.push_back(key);
                    }
                }
            }
        }
    }
    pub fn pump(
        &mut self,
        world: &World,
        index: &HeightEditIndex,
        now: Tick,
        max_submit: usize,
        max_complete: usize,
    ) -> Result<Work, Error> {
        if self.budget_tick != Some(now) {
            self.budget_tick = Some(now);
            self.submissions = 0;
            self.completions = 0;
            self.receipts = 0;
            self.probes = 0;
        }
        let mut work = Work::default();
        while self.completions < max_complete.min(2) {
            let (done, received) =
                if let Some(index) = self.delayed.iter().position(|(due, _)| *due <= now) {
                    (self.delayed.remove(index).expect("due completion").1, false)
                } else {
                    if self.receipts >= max_complete.min(2) {
                        break;
                    }
                    let Some(done) = self.pool.try_recv() else {
                        break;
                    };
                    self.receipts += 1;
                    (done, true)
                };
            if received && let jobs::JobOutcome::Success(built) = &done.outcome {
                // Stale work still consumed real generator/packing resources.
                work.generated_columns += built.work.generated_columns;
                work.public_baseline_columns += built.work.public_baseline_columns;
                work.public_baseline_scratch_reads += built.work.public_baseline_scratch_reads;
                work.public_baseline_scratch_writes += built.work.public_baseline_scratch_writes;
                work.tint_samples += built.work.tint_samples;
                work.public_witness_bytes += built.work.public_witness_bytes;
                work.exterior.authority_reads += built.work.exterior.authority_reads;
                work.exterior.baseline_reads += built.work.exterior.baseline_reads;
                work.exterior.flood_cells += built.work.exterior.flood_cells;
                work.exterior.closed_transparent_cells +=
                    built.work.exterior.closed_transparent_cells;
                work.exterior.mask_bytes += built.work.exterior.mask_bytes;
                work.exterior.peak_queue_bytes = work
                    .exterior
                    .peak_queue_bytes
                    .max(built.work.exterior.peak_queue_bytes);
                work.generation_scratch_reads += built.work.generation_scratch_reads;
                work.generation_scratch_writes += built.work.generation_scratch_writes;
                work.geometry_cells += built.work.projected_geometry_cells;
                work.halo_samples += built.work.halo_samples;
            }
            if received && let Some(running) = self.running.get(&done.id) {
                let due = Tick(
                    running
                        .submitted
                        .0
                        .saturating_add(u64::from(self.limits.completion_delay_ticks)),
                );
                if now < due {
                    debug_assert!(self.delayed.len() < self.limits.max_inflight);
                    self.delayed.push_back((due, done));
                    continue;
                }
            }
            self.completions += 1;
            let Some(running) = self.running.remove(&done.id) else {
                continue;
            };
            let Some(column) = self
                .columns
                .get_mut(&running.column)
                .filter(|c| c.registration == running.registration && c.running == Some(done.id))
            else {
                work.stale += 1;
                continue;
            };
            column.running = None;
            match done.outcome {
                jobs::JobOutcome::Success(built) if built.certificate.matches(world, index) => {
                    work.completed += 1;
                    self.accept(running.column, built, &mut work)?;
                }
                jobs::JobOutcome::Success(_) => {
                    work.stale += 1;
                    self.requeue(running.column, now, now);
                }
                _ => {
                    work.failed += 1;
                    self.requeue(running.column, Tick(now.0.saturating_add(20)), now);
                }
            }
        }
        while self.submissions < max_submit.min(2)
            && self.running.len() < self.limits.max_inflight
            && self.probes < 4
        {
            let Some(key) = self.queue.pop_front() else {
                break;
            };
            self.probes += 1;
            work.probes += 1;
            let Some(column) = self.columns.get_mut(&key) else {
                continue;
            };
            column.queued = false;
            if column.retry_after > now {
                column.queued = true;
                self.queue.push_back(key);
                continue;
            }
            if column.running.is_some() {
                continue;
            }
            work.oldest_age = work.oldest_age.max(now.0.saturating_sub(column.first.0));
            let cached = self.cached_column(key);
            if let Some(cached) = cached.filter(|b| b.certificate.matches(world, index)) {
                self.install(key, &cached, None)?;
                self.touch(CacheKey(key))?;
                continue;
            }
            let snapshot = match ColumnSnapshot::capture(world, index, key, self.limits.capture) {
                Ok(s) => s,
                Err(_) => {
                    work.deferred += 1;
                    self.requeue(key, Tick(now.0.saturating_add(20)), now);
                    continue;
                }
            };
            let captured = snapshot.capture_work();
            work.snapshot_lookups += captured.snapshot_lookups;
            work.captured_edit_cells += captured.captured_edit_cells;
            work.captured_edit_bytes += captured.captured_edit_bytes;
            let id = jobs::JobId(self.next_job);
            let next = self.next_job.checked_add(1).ok_or(Error::Overflow)?;
            let spec = jobs::JobSpec::derived(
                jobs::JobKey {
                    page: PageId {
                        space: core_types::SpaceId(0),
                        x: key.x,
                        y: 0,
                        z: key.z,
                    },
                    kind: 96,
                },
                jobs::Priority::Normal,
                jobs::WorkCost {
                    cells: 8 * (4096 + 5832),
                    quads: 0,
                    bytes: 20 * 1024 * 1024,
                    rays: 0,
                },
                self.limits.capture.max_output_bytes,
                now,
            );
            if self
                .pool
                .try_submit(jobs::Dispatch {
                    id,
                    spec,
                    cancellation: jobs::CancellationToken::new(),
                    payload: Box::new(snapshot),
                })
                .is_err()
            {
                self.requeue(key, now, now);
                break;
            }
            self.next_job = next;
            let column = self.columns.get_mut(&key).expect("requested column");
            column.running = Some(id);
            self.running.insert(
                id,
                Running {
                    column: key,
                    registration: column.registration,
                    submitted: now,
                },
            );
            self.submissions += 1;
            work.submitted += 1;
        }
        Ok(work)
    }
    fn requeue(&mut self, key: ColumnKey, due: Tick, now: Tick) {
        if let Some(column) = self.columns.get_mut(&key)
            && !column.queued
        {
            column.queued = true;
            column.retry_after = due;
            if column.first > now {
                column.first = now
            }
            self.queue.push_back(key);
        }
    }
    fn cached_column(&self, key: ColumnKey) -> Option<Arc<ColumnBody>> {
        self.cache
            .get(&CacheKey(key))
            .map(|entry| Arc::clone(&entry.data))
    }
    fn accept(
        &mut self,
        key: ColumnKey,
        built: Box<BuiltColumn>,
        work: &mut Work,
    ) -> Result<(), Error> {
        let old = self.cached_column(key);
        let BuiltColumn {
            certificate,
            pages,
            tints,
            ..
        } = *built;
        let certificate = Arc::new(certificate);
        let mut bodies = Vec::with_capacity(8);
        let mut bytes = std::mem::size_of::<ColumnBody>() + std::mem::size_of::<Certificate>();
        for page in pages {
            let geometry = page.geometry.map(|g| {
                work.public_copy_bytes += 1024;
                bytes += std::mem::size_of::<lod::PublicGeometryData>()
                    + g.palette.capacity() * std::mem::size_of::<lod::PublicSurface>()
                    + g.indices.capacity();
                Arc::new(g)
            });
            work.public_copy_bytes += 5832;
            bytes += 5832;
            bodies.push(PageBody {
                witness: page.witness,
                geometry,
                sealed_cells: page.sealed_cells.map(|cells| {
                    let cell_bytes = 4096 * std::mem::size_of::<Cell>();
                    bytes += cell_bytes;
                    work.public_copy_bytes += cell_bytes as u64;
                    Arc::from(cells)
                }),
                cells: Arc::from(page.water),
            });
        }
        let body = Arc::new(ColumnBody {
            certificate,
            pages: bodies.try_into().ok().expect("eight projected pages"),
            tints,
        });
        if bytes > self.limits.cache_bytes {
            work.deferred += 1;
            return Ok(());
        }
        self.install(key, &body, old.as_deref())?;
        work.evicted += self.cache_insert(CacheKey(key), body, bytes, 8)?;
        Ok(())
    }
    fn install(
        &mut self,
        key: ColumnKey,
        body: &ColumnBody,
        old_body: Option<&ColumnBody>,
    ) -> Result<(), Error> {
        let Some(column) = self.columns.get(&key) else {
            return Ok(());
        };
        let mut planned = [None; 8];
        let mut needed = 0;
        for (i, plan) in planned.iter_mut().enumerate() {
            let page = PageId {
                space: core_types::SpaceId(0),
                x: key.x,
                y: i as i32,
                z: key.z,
            };
            if !self.leases.contains_key(&page) {
                continue;
            }
            let Some(geometry) = body.pages[i].geometry.as_ref() else {
                continue;
            };
            let old = column.heads[i];
            let same_geometry = if let Some(b) = old_body {
                b.pages[i]
                    .geometry
                    .as_ref()
                    .is_some_and(|g| g.as_ref() == geometry.as_ref())
            } else {
                // Exterior exposure depends on every certified profile page.
                // An unchanged centre alone cannot prove the same public body
                // after a neighbouring portal opens. After LRU eviction only
                // a cryptographic witness of the public body proves
                // equivalence without exposing changed private provenance.
                self.witnesses
                    .get(&page)
                    .is_some_and(|w| w.geometry == body.pages[i].witness.geometry)
            };
            let same_geometry = old.is_some() && same_geometry;
            let same_water = same_geometry
                && (column
                    .certificate
                    .as_ref()
                    .is_some_and(|old| old.as_ref() == body.certificate.as_ref())
                    || old_body.is_some_and(|b| {
                        b.pages[i].cells == body.pages[i].cells && b.tints == body.tints
                    })
                    || old_body.is_none()
                        && self
                            .witnesses
                            .get(&page)
                            .is_some_and(|w| w.water == body.pages[i].witness.water));
            needed += u64::from(!same_geometry) + u64::from(!same_water);
            *plan = Some((same_geometry, same_water));
        }
        let mut next = reserve_ids(needed)?;
        self.public_ids = self.public_ids.checked_add(needed).ok_or(Error::Overflow)?;
        let column = self.columns.get_mut(&key).expect("checked column");
        for (i, plan) in planned.into_iter().enumerate() {
            let page = PageId {
                space: core_types::SpaceId(0),
                x: key.x,
                y: i as i32,
                z: key.z,
            };
            let old = column.heads[i];
            let Some((same_geometry, same_water)) = plan else {
                if old.is_some() && self.leases.contains_key(&page) {
                    self.changed_geometry.insert(page);
                    self.changed_water.insert(page);
                }
                column.heads[i] = None;
                self.witnesses.remove(&page);
                continue;
            };
            let head = if same_geometry {
                old.expect("retained public head").head
            } else {
                let id = next;
                next += 1;
                Head {
                    source: Version(id),
                    view: id,
                }
            };
            let water_view = if same_water {
                old.expect("retained water head").water_view
            } else {
                let id = next;
                next += 1;
                id
            };
            if old.is_some_and(|old| old.head != head) {
                self.changed_geometry.insert(page);
            }
            if old.is_some_and(|old| old.water_view != water_view) {
                self.changed_water.insert(page);
            }
            column.heads[i] = Some(PageHead { head, water_view });
            self.witnesses.insert(page, body.pages[i].witness);
        }
        column.certificate = Some(Arc::clone(&body.certificate));
        Ok(())
    }
    pub fn head(&self, page: PageId, world: &World, index: &HeightEditIndex) -> Option<Head> {
        let key = canonical(page)?;
        if !self.leases.contains_key(&page) {
            return None;
        }
        let column = self.columns.get(&key)?;
        if !column.certificate.as_ref()?.matches(world, index) {
            return None;
        }
        Some(column.heads[page.y as usize]?.head)
    }
    pub fn geometry(
        &mut self,
        page: PageId,
        world: &World,
        index: &HeightEditIndex,
    ) -> Option<(Head, Arc<lod::PublicGeometryData>)> {
        let head = self.head(page, world, index)?;
        let key = CacheKey(canonical(page)?);
        let geometry = Arc::clone(
            self.cache.get(&key)?.data.pages[page.y as usize]
                .geometry
                .as_ref()?,
        );
        self.touch(key).ok()?;
        Some((head, geometry))
    }
    pub fn water(
        &mut self,
        page: PageId,
        world: &World,
        index: &HeightEditIndex,
    ) -> Option<WaterBody> {
        let key = canonical(page)?;
        let head = self.head(page, world, index)?;
        let metadata = self.columns.get(&key)?.heads[page.y as usize]?;
        let body = self.cached_column(key)?;
        if !body.certificate.matches(world, index) {
            return None;
        }
        let result = WaterBody {
            head,
            water_view: metadata.water_view,
            // The body is current under its selective input certificate. The
            // private Height envelope may have advanced for a resident-only
            // emission edit, even while all public bytes/IDs remain identical.
            // Keep publish_projected's current-private-envelope check strict.
            column_source: index
                .source(crate::height_index::TileKey {
                    space: page.space,
                    level: 0,
                    x: page.x,
                    z: page.z,
                })
                .ok()?,
            cells: Arc::clone(&body.pages[page.y as usize].cells),
            tints: body.tints,
        };
        self.touch(CacheKey(key)).ok()?;
        Some(result)
    }
    /// Worker-produced sealed/ore-redacted body for a near observer. The near
    /// adapter may restore actual values only after its concrete visibility ray.
    pub fn sealed_cells(
        &mut self,
        page: PageId,
        world: &World,
        index: &HeightEditIndex,
    ) -> Option<Arc<[Cell; 4096]>> {
        let key = canonical(page)?;
        self.head(page, world, index)?;
        let body = self.cached_column(key)?;
        if !body.certificate.matches(world, index) {
            return None;
        }
        let cells = Arc::clone(body.pages[page.y as usize].sealed_cells.as_ref()?);
        self.touch(CacheKey(key)).ok()?;
        Some(cells)
    }
    /// No body clone and no foreground generation. A missing/evicted/stale body
    /// re-enters the same bounded worker queue while this raw lease remains live.
    pub fn raw_cells(
        &mut self,
        connection: u64,
        page: PageId,
        world: &World,
        index: &HeightEditIndex,
        now: Tick,
    ) -> Option<Arc<[Cell; 4096]>> {
        if !self.raw_peers.get(&connection)?.contains(&page) {
            return None;
        }
        if let Some(cells) = self.sealed_cells(page, world, index) {
            return Some(cells);
        }
        let key = canonical(page)?;
        self.request(key, now).ok()?;
        None
    }
    fn touch(&mut self, key: CacheKey) -> Result<(), Error> {
        let Some(cached) = self.cache.get_mut(&key) else {
            return Ok(());
        };
        self.lru.remove(&(cached.used, key));
        let used = self.next_use;
        self.next_use = used.checked_add(1).ok_or(Error::Overflow)?;
        cached.used = used;
        self.lru.insert((used, key));
        Ok(())
    }
    fn cache_insert(
        &mut self,
        key: CacheKey,
        data: Arc<ColumnBody>,
        bytes: usize,
        units: usize,
    ) -> Result<u64, Error> {
        if let Some(old) = self.cache.remove(&key) {
            self.lru.remove(&(old.used, key));
            self.cache_bytes -= old.bytes;
            self.cache_units -= old.units;
        }
        let used = self.next_use;
        self.next_use = used.checked_add(1).ok_or(Error::Overflow)?;
        self.cache.insert(
            key,
            Cached {
                data,
                used,
                bytes,
                units,
            },
        );
        self.lru.insert((used, key));
        self.cache_bytes += bytes;
        self.cache_units += units;
        let mut count = 0;
        while self.cache_units > self.limits.cache_columns * 8
            || self.cache_bytes > self.limits.cache_bytes
        {
            let Some((used, key)) = self.lru.pop_first() else {
                break;
            };
            let Some(old) = self.cache.remove(&key) else {
                continue;
            };
            debug_assert_eq!(used, old.used);
            self.cache_bytes -= old.bytes;
            self.cache_units -= old.units;
            count += 1;
        }
        Ok(count)
    }
    pub fn stats(&self) -> Stats {
        Stats {
            public_ids: self.public_ids,
            witness_rows: self.witnesses.len(),
            witness_digest_bytes: self.witnesses.len() * std::mem::size_of::<ContentWitness>(),
            witness_row_bytes: self.witnesses.len()
                * std::mem::size_of::<(PageId, ContentWitness)>(),
            connections: self.connections.len(),
            raw_connections: self.raw_peers.len(),
            raw_leases: self.raw_peers.values().map(|p| p.len()).sum(),
            leases: self.leases.len(),
            columns: self.columns.len(),
            heads: self
                .columns
                .values()
                .map(|c| c.heads.iter().flatten().count())
                .sum::<usize>(),
            queued: self.queue.len(),
            running: self.running.len(),
            cache_entries: self.cache.len(),
            cache_units: self.cache_units,
            cache_bytes: self.cache_bytes,
            changed_geometry: self.changed_geometry.len(),
            changed_water: self.changed_water.len(),
        }
    }
}
fn reserve_ids(count: u64) -> Result<u64, Error> {
    NEXT_PUBLIC
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
            v.checked_add(count)
        })
        .map_err(|_| Error::Overflow)
}
fn canonical(page: PageId) -> Option<ColumnKey> {
    (page.space.0 == 0 && (0..8).contains(&page.y)).then_some(ColumnKey {
        x: page.x,
        z: page.z,
    })
}
