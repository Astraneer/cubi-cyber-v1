use super::capture::{CHANNELS, Certificate, Classified, Error, Snapshot};
use core_types::{PageId, Tick};
use net::water::{Request, WaterHalo};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use world::{World, WorldChange};
type Key = (u64, PageId);
type Pool = jobs::NativePool<Box<Snapshot>, Classified>;
const MAX_RUNNING: usize = 8;
#[derive(Clone)]
struct Front {
    request: Request,
    certificate: Option<Certificate>,
    projected_view: Option<u64>,
    view: u64,
}
struct Stream {
    epoch: u64,
    request: Request,
    front: Option<Front>,
    confirmed: Option<u64>,
    force: bool,
    dirty: bool,
    queued: bool,
    running: bool,
    first_dirty: Tick,
    retry: Option<Tick>,
}
#[derive(Default)]
struct Peer {
    streams: BTreeMap<PageId, Stream>,
    queue: VecDeque<PageId>,
}
struct Running {
    key: Key,
    epoch: u64,
    request: Request,
    certificate: Certificate,
    cancellation: jobs::CancellationToken,
}
pub(crate) struct Ready {
    pub key: Key,
    epoch: u64,
    pub request: Request,
    pub classified: Classified,
    certificate: Certificate,
}
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Stats {
    pub streams: usize,
    pub queued: usize,
    pub running: usize,
    pub watcher_links: usize,
    pub snapshot_lookups: u64,
    pub classified_cells: u64,
    pub tint_samples: u64,
    pub submitted: u64,
    pub published: u64,
    pub discarded: u64,
    pub failed: u64,
    pub canonical_generation_attempts: u64,
    pub projected_images: u64,
    pub projected_image_bytes: u64,
    pub oldest_dirty_age: u64,
}
pub(crate) struct Owner {
    pool: Pool,
    peers: BTreeMap<u64, Peer>,
    watchers: BTreeMap<PageId, BTreeSet<Key>>,
    running: BTreeMap<jobs::JobId, Running>,
    retries: BTreeSet<(Tick, Key, u64)>,
    next_job: u64,
    next_stream: u64,
    next_view: u64,
    next_peer: u64,
    max_peers: usize,
    max_pages: usize,
    work: Stats,
    budget_tick: Option<Tick>,
    attempts: u8,
    submissions: u8,
    publications: u8,
    retries_woken: bool,
    generation_attempts: u8,
}
fn same_binding(a: Request, b: Request) -> bool {
    a.page == b.page
        && a.geometry_source == b.geometry_source
        && a.geometry_view == b.geometry_view
        && a.column_source == b.column_source
}
impl Owner {
    pub(crate) fn new(
        terrain: r#gen::Generator,
        max_peers: usize,
        max_pages: usize,
    ) -> Result<Self, Error> {
        if max_peers == 0 || max_peers > crate::MAX_CONNECTIONS || max_pages == 0 {
            return Err(Error::Capacity);
        }
        let pool = Pool::new(2, MAX_RUNNING, move |snapshot, cancel| {
            snapshot
                .classify(&terrain, || cancel.is_cancelled())
                .map_err(|e| e.to_string())
        })
        .map_err(|_| Error::Capacity)?;
        Ok(Self {
            pool,
            peers: BTreeMap::new(),
            watchers: BTreeMap::new(),
            running: BTreeMap::new(),
            retries: BTreeSet::new(),
            next_job: 1,
            next_stream: 1,
            next_view: 1,
            next_peer: 0,
            max_peers,
            max_pages,
            work: Stats::default(),
            budget_tick: None,
            attempts: 0,
            submissions: 0,
            publications: 0,
            retries_woken: false,
            generation_attempts: 0,
        })
    }
    pub(crate) fn begin_tick(&mut self, now: Tick) {
        if self.budget_tick != Some(now) {
            self.budget_tick = Some(now);
            self.attempts = 0;
            self.submissions = 0;
            self.publications = 0;
            self.retries_woken = false;
            self.generation_attempts = 0;
        }
    }
    pub(crate) fn admit_generation_attempt(&mut self) -> bool {
        if self.generation_attempts >= 54 {
            return false;
        }
        self.generation_attempts += 1;
        self.work.canonical_generation_attempts += 1;
        true
    }
    pub(crate) fn request(
        &mut self,
        connection: u64,
        request: Request,
        now: Tick,
    ) -> Result<(), Error> {
        request.validate().map_err(|_| Error::CoordinateOverflow)?;
        let neighbors = neighbors(request.page)?;
        if !self.peers.contains_key(&connection) && self.peers.len() >= self.max_peers {
            return Err(Error::Capacity);
        }
        let peer = self.peers.entry(connection).or_default();
        if !peer.streams.contains_key(&request.page) {
            if peer.streams.len() >= self.max_pages {
                return Err(Error::Capacity);
            }
            let epoch = self.next_stream;
            self.next_stream = epoch.checked_add(1).ok_or(Error::Capacity)?;
            peer.streams.insert(
                request.page,
                Stream {
                    epoch,
                    request,
                    front: None,
                    confirmed: None,
                    force: true,
                    dirty: true,
                    queued: false,
                    running: false,
                    first_dirty: now,
                    retry: None,
                },
            );
            for page in neighbors {
                self.watchers
                    .entry(page)
                    .or_default()
                    .insert((connection, request.page));
            }
        }
        let stream = peer
            .streams
            .get_mut(&request.page)
            .expect("inserted stream");
        if request.geometry_view < stream.request.geometry_view
            || request.column_source < stream.request.column_source
        {
            return Ok(());
        }
        let binding_changed = !same_binding(request, stream.request);
        stream.request = request;
        if let Some(front) = &stream.front {
            if same_binding(front.request, request) && request.acknowledged == Some(front.view) {
                stream.confirmed = Some(front.view);
            } else {
                stream.force = true;
            }
        }
        if binding_changed || stream.force {
            if !stream.dirty {
                stream.first_dirty = now
            }
            stream.dirty = true;
        }
        Self::enqueue(peer, request.page);
        Ok(())
    }
    fn enqueue(peer: &mut Peer, page: PageId) {
        let Some(stream) = peer.streams.get_mut(&page) else {
            return;
        };
        let credit = stream.force
            || stream
                .front
                .as_ref()
                .is_none_or(|f| stream.confirmed == Some(f.view));
        if stream.dirty && credit && !stream.queued && !stream.running {
            stream.queued = true;
            peer.queue.push_back(page);
        }
    }
    pub(crate) fn forget(&mut self, connection: u64, page: PageId) {
        let Some(peer) = self.peers.get_mut(&connection) else {
            return;
        };
        let Some(stream) = peer.streams.remove(&page) else {
            return;
        };
        if let Some(due) = stream.retry {
            self.retries
                .remove(&(due, (connection, page), stream.epoch));
        }
        peer.queue.retain(|p| *p != page);
        if let Ok(neighbors) = neighbors(page) {
            for input in neighbors {
                if let Some(watchers) = self.watchers.get_mut(&input) {
                    watchers.remove(&(connection, page));
                    if watchers.is_empty() {
                        self.watchers.remove(&input);
                    }
                }
            }
        }
        for job in self
            .running
            .values()
            .filter(|job| job.key == (connection, page))
        {
            job.cancellation.cancel();
        }
    }
    pub(crate) fn forget_peer(&mut self, connection: u64) {
        let Some(peer) = self.peers.get(&connection) else {
            return;
        };
        let pages: Vec<_> = peer.streams.keys().copied().collect();
        for page in pages {
            self.forget(connection, page);
        }
        self.peers.remove(&connection);
    }
    pub(crate) fn retain_peers(&mut self, keep: impl Fn(u64) -> bool) {
        let mut gone = [0; crate::MAX_CONNECTIONS];
        let mut count = 0;
        for &connection in self.peers.keys().filter(|&&connection| !keep(connection)) {
            gone[count] = connection;
            count += 1;
        }
        for &connection in &gone[..count] {
            self.forget_peer(connection)
        }
    }
    pub(crate) fn forget_column(&mut self, connection: u64, key: net::height::HeightKey) {
        let Some(peer) = self.peers.get(&connection) else {
            return;
        };
        let pages: Vec<_> = peer
            .streams
            .keys()
            .copied()
            .filter(|page| {
                key.level == 0 && page.space.0 == key.space && page.x == key.x && page.z == key.z
            })
            .collect();
        for page in pages {
            self.forget(connection, page)
        }
    }
    pub(crate) fn observe(&mut self, changes: &[WorldChange], now: Tick) {
        for page in changes
            .iter()
            .flat_map(|c| &c.pages)
            .filter(|p| p.created || p.removed || p.channels.intersects(CHANNELS))
        {
            let Some(watchers) = self.watchers.get(&page.page) else {
                continue;
            };
            // Indexed only by the changed source page, bounded by subscribed streams.
            let keys: Vec<_> = watchers.iter().copied().collect();
            for (connection, center) in keys {
                let Some(peer) = self.peers.get_mut(&connection) else {
                    continue;
                };
                let Some(stream) = peer.streams.get_mut(&center) else {
                    continue;
                };
                if !stream.dirty && stream.retry.is_none() {
                    stream.first_dirty = now
                }
                stream.dirty = true;
                Self::enqueue(peer, center);
            }
        }
    }
    /// A completed immutable projection can change a halo without another
    /// World event. Preserve image credit and coalesce this wake with edits.
    pub(crate) fn wake_projection(&mut self, page: PageId, now: Tick) {
        for peer in self.peers.values_mut() {
            if let Some(stream) = peer.streams.get_mut(&page) {
                if !stream.dirty {
                    stream.first_dirty = now;
                }
                stream.dirty = true;
                Self::enqueue(peer, page);
            }
        }
    }
    pub(crate) fn next_request(&mut self) -> Option<(Key, Request)> {
        use std::ops::Bound::{Excluded, Unbounded};
        if self.attempts >= 4 || self.submissions >= 2 {
            return None;
        }
        for _ in 0..self.peers.len() {
            let connection = self
                .peers
                .range((Excluded(self.next_peer), Unbounded))
                .next()
                .or_else(|| self.peers.first_key_value())
                .map(|(&id, _)| id)?;
            self.next_peer = connection;
            let peer = self.peers.get_mut(&connection).expect("listed peer");
            let Some(page) = peer.queue.pop_front() else {
                continue;
            };
            let stream = peer.streams.get_mut(&page).expect("queued stream");
            stream.queued = false;
            self.attempts += 1;
            return Some(((connection, page), stream.request));
        }
        None
    }
    pub(crate) fn wake_retries(&mut self, now: Tick) {
        if self.retries_woken {
            return;
        }
        self.retries_woken = true;
        for _ in 0..2 {
            let Some(&(due, key, epoch)) = self.retries.first() else {
                break;
            };
            if due > now {
                break;
            }
            self.retries.pop_first();
            let Some(peer) = self.peers.get_mut(&key.0) else {
                continue;
            };
            let Some(stream) = peer.streams.get_mut(&key.1).filter(|s| s.epoch == epoch) else {
                continue;
            };
            let next = Tick(now.0.saturating_add(20));
            stream.retry = Some(next);
            self.retries.insert((next, key, epoch));
            stream.dirty = true;
            Self::enqueue(peer, key.1);
        }
    }
    pub(crate) fn missing_inputs(&self, key: Key) -> impl Iterator<Item = PageId> + '_ {
        self.peers
            .get(&key.0)
            .and_then(|p| p.streams.get(&key.1))
            .and_then(|s| s.front.as_ref())
            .and_then(|f| f.certificate.as_ref())
            .into_iter()
            .flat_map(|certificate| {
                certificate
                    .inputs()
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| !certificate.available(*i))
                    .map(|(_, input)| input.page)
            })
    }
    pub(crate) fn defer(&mut self, key: Key) {
        if let Some(peer) = self.peers.get_mut(&key.0) {
            Self::enqueue(peer, key.1)
        }
    }
    pub(crate) fn skip_unchanged(&mut self, key: Key, world: &World) -> bool {
        let Some(stream) = self
            .peers
            .get_mut(&key.0)
            .and_then(|p| p.streams.get_mut(&key.1))
        else {
            return true;
        };
        if !stream.force
            && stream.front.as_ref().is_some_and(|front| {
                same_binding(front.request, stream.request)
                    && front
                        .certificate
                        .as_ref()
                        .is_some_and(|certificate| certificate.matches(world))
            })
        {
            stream.dirty = false;
            return true;
        }
        false
    }
    pub(crate) fn submit(
        &mut self,
        key: Key,
        request: Request,
        snapshot: Snapshot,
        now: Tick,
    ) -> bool {
        if self.running.len() >= MAX_RUNNING || self.submissions >= 2 {
            self.defer(key);
            return false;
        }
        let Some(next) = self.next_job.checked_add(1) else {
            return false;
        };
        let id = jobs::JobId(self.next_job);
        let certificate = snapshot.certificate.clone();
        let cancellation = jobs::CancellationToken::new();
        let spec = jobs::JobSpec::derived(
            jobs::JobKey {
                page: key.1,
                kind: 94,
            },
            jobs::Priority::Normal,
            jobs::WorkCost {
                cells: 6088,
                quads: 0,
                bytes: 65536,
                rays: 0,
            },
            65536,
            now,
        );
        if self
            .pool
            .try_submit(jobs::Dispatch {
                id,
                spec,
                cancellation: cancellation.clone(),
                payload: Box::new(snapshot),
            })
            .is_err()
        {
            self.defer(key);
            return false;
        }
        self.next_job = next;
        let stream = self
            .peers
            .get_mut(&key.0)
            .expect("submitted peer")
            .streams
            .get_mut(&key.1)
            .expect("submitted stream");
        stream.running = true;
        self.running.insert(
            id,
            Running {
                key,
                epoch: stream.epoch,
                request,
                certificate,
                cancellation,
            },
        );
        self.work.submitted += 1;
        self.submissions += 1;
        self.work.snapshot_lookups += 27;
        true
    }
    pub(crate) fn poll(&mut self, world: &World) -> Option<Ready> {
        if self.publications >= 2 {
            return None;
        }
        let completion = self.pool.try_recv()?;
        let running = self.running.remove(&completion.id)?;
        let peer = self.peers.get_mut(&running.key.0)?;
        let stream = peer.streams.get_mut(&running.key.1)?;
        if stream.epoch != running.epoch {
            self.work.discarded += 1;
            return None;
        }
        stream.running = false;
        let jobs::JobOutcome::Success(classified) = completion.outcome else {
            self.work.failed += 1;
            Self::enqueue(peer, running.key.1);
            return None;
        };
        self.work.classified_cells += classified.work.classified_cells;
        self.work.tint_samples += classified.work.tint_samples;
        if !same_binding(stream.request, running.request)
            || !running.certificate.matches(world)
            || classified.page != running.key.1
        {
            self.work.discarded += 1;
            Self::enqueue(peer, running.key.1);
            return None;
        }
        Some(Ready {
            key: running.key,
            epoch: running.epoch,
            request: running.request,
            certificate: running.certificate,
            classified,
        })
    }
    pub(crate) fn publish(&mut self, ready: Ready, now: Tick) -> Result<Box<WaterHalo>, Error> {
        let peer = self
            .peers
            .get_mut(&ready.key.0)
            .ok_or(Error::MissingCenter)?;
        let stream = peer
            .streams
            .get_mut(&ready.key.1)
            .ok_or(Error::MissingCenter)?;
        if stream.epoch != ready.epoch || !same_binding(stream.request, ready.request) {
            return Err(Error::MissingCenter);
        }
        let view = if let Some(front) = &stream.front
            && same_binding(front.request, ready.request)
            && front.certificate.as_ref() == Some(&ready.certificate)
        {
            front.view
        } else {
            let next = self.next_view.checked_add(1).ok_or(Error::Capacity)?;
            let view = self.next_view;
            self.next_view = next;
            view
        };
        stream.front = Some(Front {
            request: ready.request,
            certificate: Some(ready.certificate),
            projected_view: None,
            view,
        });
        if let Some(due) = stream.retry.take() {
            self.retries.remove(&(due, ready.key, stream.epoch));
        }
        let certificate = stream
            .front
            .as_ref()
            .expect("assigned front")
            .certificate
            .as_ref()
            .expect("resident front");
        let incomplete = certificate.inputs().iter().enumerate().any(|(i, input)| {
            !certificate.available(i) && input.page.space.0 == 0 && (0..8).contains(&input.page.y)
        });
        if incomplete {
            let due = Tick(now.0.saturating_add(20));
            stream.retry = Some(due);
            self.retries.insert((due, ready.key, stream.epoch));
        }
        stream.dirty = false;
        stream.force = false;
        self.work.published += 1;
        self.publications += 1;
        Ok(Box::new(WaterHalo {
            page: ready.key.1,
            geometry_source: ready.request.geometry_source,
            geometry_view: ready.request.geometry_view,
            column_source: ready.request.column_source,
            water_view: view,
            cells: ready.classified.cells,
            tints: ready.classified.tints,
        }))
    }
    /// Reuse the immutable cold worker body. Receipt credit is identical to the
    /// resident path; copying this bounded image does not decode authoritative cells.
    pub(crate) fn publish_projected(
        &mut self,
        key: Key,
        request: Request,
        body: crate::cold_projection_owner::WaterBody,
        private_column_source: u64,
    ) -> Result<Option<Box<WaterHalo>>, Error> {
        if self.publications >= 2 {
            self.defer(key);
            return Ok(None);
        }
        if body.head.source != request.geometry_source
            || body.head.view != request.geometry_view
            || body.column_source != private_column_source
        {
            return Err(Error::MissingCenter);
        }
        let peer = self.peers.get_mut(&key.0).ok_or(Error::MissingCenter)?;
        let stream = peer.streams.get_mut(&key.1).ok_or(Error::MissingCenter)?;
        if !same_binding(stream.request, request) {
            return Err(Error::MissingCenter);
        }
        let unchanged = stream.front.as_ref().filter(|front| {
            same_binding(front.request, request) && front.projected_view == Some(body.water_view)
        });
        if !stream.force && unchanged.is_some() {
            stream.dirty = false;
            return Ok(None);
        }
        let view = if let Some(front) = unchanged {
            front.view
        } else {
            let view = self.next_view;
            self.next_view = view.checked_add(1).ok_or(Error::Capacity)?;
            view
        };
        stream.front = Some(Front {
            request,
            certificate: None,
            projected_view: Some(body.water_view),
            view,
        });
        if let Some(due) = stream.retry.take() {
            self.retries.remove(&(due, key, stream.epoch));
        }
        stream.force = false;
        stream.dirty = false;
        self.work.published += 1;
        self.publications += 1;
        self.work.projected_images += 1;
        self.work.projected_image_bytes += net::water::MAX_BYTES as u64;
        Ok(Some(Box::new(WaterHalo {
            page: key.1,
            geometry_source: request.geometry_source,
            geometry_view: request.geometry_view,
            column_source: request.column_source,
            water_view: view,
            cells: Box::new(*body.cells),
            tints: body.tints,
        })))
    }
    pub(crate) fn stats(&self, now: Tick) -> Stats {
        let mut stats = self.work;
        stats.running = self.running.len();
        for peer in self.peers.values() {
            stats.streams += peer.streams.len();
            stats.queued += peer.queue.len();
            for stream in peer
                .streams
                .values()
                .filter(|s| s.dirty || s.retry.is_some())
            {
                stats.oldest_dirty_age = stats
                    .oldest_dirty_age
                    .max(now.0.saturating_sub(stream.first_dirty.0));
            }
        }
        stats.watcher_links = self.watchers.values().map(BTreeSet::len).sum();
        stats
    }
}
fn neighbors(page: PageId) -> Result<[PageId; 27], Error> {
    for coordinate in [page.x, page.y, page.z] {
        coordinate.checked_sub(1).ok_or(Error::CoordinateOverflow)?;
        coordinate.checked_add(1).ok_or(Error::CoordinateOverflow)?;
    }
    Ok(std::array::from_fn(|i| PageId {
        space: page.space,
        x: page.x + i as i32 % 3 - 1,
        y: page.y + i as i32 / 9 - 1,
        z: page.z + i as i32 / 3 % 3 - 1,
    }))
}
