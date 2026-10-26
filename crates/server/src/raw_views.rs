//! Near views consume certified worker projection bodies. Per-observer queries
//! are resumable and bounded; no private partial image leaves this owner.
use super::*;
use std::sync::Arc;

/// Reliable retries name one retained obligation. Duplicate requests must not
/// consume the bounded queue ahead of pages which have never been published.
pub(super) fn queue_request(
    queue: &mut VecDeque<(PageId, bool)>,
    page: PageId,
    force: bool,
) -> bool {
    if let Some((_, queued_force)) = queue.iter_mut().find(|(p, _)| *p == page) {
        *queued_force |= force;
        return true;
    }
    if queue.len() >= MAX_AOI_PAGES {
        return false;
    }
    queue.push_back((page, force));
    true
}

/// Restore the slot of an obligation already removed with pop_front. Existing
/// internal visibility sweeps can exceed the fresh-admission cap; restoration
/// must not discard an admitted request or its full-repair requirement.
pub(super) fn restore_popped_request(
    queue: &mut VecDeque<(PageId, bool)>,
    page: PageId,
    force: bool,
) {
    if !queue_request(queue, page, force) {
        queue.push_back((page, force));
    }
}

#[cfg(test)]
mod request_queue_tests {
    use super::*;

    fn active(page: PageId) -> ObserverViews {
        ObserverViews {
            pending: Some(PendingRaw {
                view: visibility::PendingView::new(
                    page,
                    1,
                    Box::new([Cell::AIR; core_types::PAGE_VOLUME]),
                    None,
                )
                .unwrap(),
                force: false,
                policy: None,
                eye: Some([0.; 3]),
                dependencies: BTreeMap::new(),
                restart: false,
            }),
            ..ObserverViews::default()
        }
    }

    #[test]
    fn full_queue_coalesces_retries_without_starving_later_pages_or_canceling_repairs() {
        let page = |x| PageId {
            space: SpaceId(0),
            x,
            y: -1,
            z: -2,
        };
        let mut queue = VecDeque::new();
        for x in 0..MAX_AOI_PAGES as i32 {
            assert!(queue_request(&mut queue, page(x), false));
        }
        let original = queue.clone();
        for _ in 0..10_000 {
            assert!(queue_request(&mut queue, page(0), false));
        }
        assert_eq!(queue, original);
        assert!(!queue_request(
            &mut queue,
            page(MAX_AOI_PAGES as i32),
            false
        ));
        assert_eq!(queue, original);
        // A full repair remains admitted at capacity, and a late confirmed-base
        // request cannot cancel it or move the remaining FIFO obligations.
        assert!(queue_request(&mut queue, page(0), true));
        assert!(queue_request(&mut queue, page(0), false));
        assert_eq!(queue.pop_front(), Some((page(0), true)));
        assert_eq!(queue, original.into_iter().skip(1).collect::<VecDeque<_>>());
    }

    #[test]
    fn active_request_absorbs_repairs_and_stale_duplicates_without_reordering_fifo() {
        let page = PageId {
            space: SpaceId(0),
            x: 0,
            y: 0,
            z: 0,
        };
        let mut views = active(page);
        let mut queue: VecDeque<_> = (1..=MAX_AOI_PAGES as i32)
            .map(|x| (PageId { x, ..page }, false))
            .collect();
        let original = queue.clone();
        assert!(views.queue_request(&mut queue, page, true));
        assert_eq!(queue, original);
        assert_eq!(views.pending_request(), Some((page, true)));
        for _ in 0..1000 {
            assert!(views.queue_request(&mut queue, page, false));
        }
        assert_eq!(queue, original);
        // Old internal sweeps may already have queued a duplicate of the active
        // page. Its repair bit must survive while every other row keeps order.
        views.pending.as_mut().unwrap().force = false;
        queue.insert(7, (page, true));
        queue.insert(21, (page, false));
        assert!(views.queue_request(&mut queue, page, false));
        assert_eq!(views.pending_request(), Some((page, true)));
        assert_eq!(queue, original);
    }

    #[test]
    fn restoring_popped_obligation_preserves_force_even_above_fresh_admission_cap() {
        let page = PageId {
            space: SpaceId(0),
            x: 0,
            y: 0,
            z: 0,
        };
        let mut queue: VecDeque<_> = (0..MAX_AOI_PAGES as i32 + 3)
            .map(|x| (PageId { x, ..page }, x == 0))
            .collect();
        let (popped, force) = queue.pop_front().unwrap();
        let retained = queue.clone();
        restore_popped_request(&mut queue, popped, force);
        assert_eq!(queue.len(), MAX_AOI_PAGES + 3);
        assert_eq!(queue.pop_back(), Some((page, true)));
        assert_eq!(queue, retained);
        queue.insert(5, (page, false));
        let order: Vec<_> = queue.iter().map(|(p, _)| *p).collect();
        restore_popped_request(&mut queue, page, true);
        assert_eq!(queue.iter().map(|(p, _)| *p).collect::<Vec<_>>(), order);
        assert_eq!(queue[5], (page, true));
    }

    #[test]
    fn invalid_partial_view_keeps_forced_obligation_at_capacity_until_fresh_rebuild() {
        struct Directory(std::path::PathBuf);
        impl Drop for Directory {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let directory = Directory(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../.local/tmp")
                .join(format!(
                    "raw-pending-defer-{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                )),
        );
        std::fs::create_dir_all(&directory.0).unwrap();
        let page = PageId {
            space: SpaceId(0),
            x: 0,
            y: 8,
            z: 0,
        };
        let mut world = World::new(rules::properties, 1 << 20);
        let mut cells = vec![Cell::AIR; core_types::PAGE_VOLUME];
        cells[..2].fill(Cell::from_parts(rules::ids::IRON_ORE, 0));
        let mut transaction = world::Transaction::new(Tick(0), page.space);
        transaction.create_page(page, cells);
        world.commit(transaction).unwrap();
        let mut config = ServerConfig::new(&directory.0);
        config.generation_enabled = false;
        config.spawn_override = Some([8.5, 129., 8.5]);
        let mut server = Server::open_with_world(config, world).unwrap();
        let hello = net::decode_client(&std::fs::read(directory.0.join("owner.identity")).unwrap())
            .unwrap();
        server.handle(1, hello, 0);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut now = 0;
        while !server.peers.get(&1).is_some_and(|peer| peer.ready) {
            assert!(
                std::time::Instant::now() < deadline,
                "durable real owner Hello"
            );
            now += 10;
            server.tick(now);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let mut peer = server.peers.remove(&1).unwrap();
        peer.raw_view.begin_tick(Tick(100));
        peer.raw_view.budget.rays = 1;
        assert!(server.build_raw_view(1, &mut peer, page, false).is_none());
        assert_eq!(peer.raw_view.pending.as_ref().unwrap().view.next_index(), 1);
        for x in 1..=MAX_AOI_PAGES as i32 {
            peer.page_requests.push_back((PageId { x, ..page }, false));
        }
        let original = peer.page_requests.clone();
        assert!(
            peer.raw_view
                .queue_request(&mut peer.page_requests, page, true)
        );
        assert_eq!(peer.page_requests, original);

        // Dependency overflow and explicit observer invalidation use this same
        // restart bit. Even unchanged source/eye must discard the old prefix.
        peer.raw_view.invalidate_observer();
        peer.raw_view.begin_tick(Tick(101));
        peer.raw_view.budget.rays = 0;
        assert!(server.build_raw_view(1, &mut peer, page, false).is_none());
        assert_eq!(peer.raw_view.pending.as_ref().unwrap().view.next_index(), 0);
        assert_eq!(peer.raw_view.pending_request(), Some((page, true)));
        peer.raw_view.budget.rays = 1;
        assert!(server.build_raw_view(1, &mut peer, page, false).is_none());
        assert_eq!(peer.raw_view.pending.as_ref().unwrap().view.next_index(), 1);

        // The first private cell was already visited. Change its source and the
        // observer, then deny all copy capacity: retain the obligation, expose
        // no prefix and do not spend or advance any more work.
        let position = BlockPos {
            space: page.space,
            x: 0,
            y: 128,
            z: 0,
        };
        let mut transaction = world::Transaction::new(Tick(102), page.space);
        transaction.set_cell(position, Cell::from_parts(rules::ids::STONE, 0));
        let change = server.world.commit(transaction).unwrap();
        server.raw_sources.observe(std::slice::from_ref(&change));
        peer.player.teleport([9.5, 129., 8.5]).unwrap();
        peer.raw_view.budget = visibility::Budget::new(0, 0);
        peer.raw_view.copies = 0;
        let before = server.counters().view_cells;
        assert!(server.build_raw_view(1, &mut peer, page, false).is_none());
        assert_eq!(peer.raw_view.pending.as_ref().unwrap().view.next_index(), 1);
        assert_eq!(peer.raw_view.pending_request(), Some((page, true)));
        assert_eq!(peer.page_requests, original);
        assert_eq!(server.counters().view_cells, before);
        peer.raw_view.begin_tick(Tick(103));
        let (_, cells, force) = server.build_raw_view(1, &mut peer, page, false).unwrap();
        assert!(
            force,
            "fresh rebuild lost the admitted full-repair requirement"
        );
        assert_eq!(
            cells[0],
            Cell::from_parts(rules::ids::STONE, 0),
            "stale private prefix leaked into replacement"
        );
        assert!(peer.raw_view.pending_request().is_none());
        assert_eq!(peer.page_requests, original);
    }
}

#[derive(Default)]
pub(super) struct ObserverViews {
    pending: Option<PendingRaw>,
    tick: Option<Tick>,
    budget: visibility::Budget,
    copies: usize,
    // Private freshness stays on the owner and never enters the wire header.
    admitted: BTreeMap<PageId, (u64, Version)>,
}
struct PendingRaw {
    view: visibility::PendingView,
    force: bool,
    policy: Option<Arc<[Cell; core_types::PAGE_VOLUME]>>,
    eye: Option<[f64; 3]>,
    // A ray fan from one16³ page to an eye within64 blocks fits216 pages.
    // Keep a hard256-entry bound, including Unknown availability witnesses.
    dependencies: BTreeMap<PageId, Option<Version>>,
    restart: bool,
}
impl ObserverViews {
    pub(super) fn begin_tick(&mut self, tick: Tick) {
        if self.tick != Some(tick) {
            self.tick = Some(tick);
            self.budget = visibility::Budget::default();
            self.copies = 2;
        }
    }
    pub(super) fn pending_request(&self) -> Option<(PageId, bool)> {
        self.pending
            .as_ref()
            .map(|pending| (pending.view.page(), pending.force))
    }
    /// The active view owns the obligation while its queue slot is free.
    /// Fold stale duplicate rows into it without moving any other FIFO row.
    pub(super) fn coalesce_pending_request(
        &mut self,
        queue: &mut VecDeque<(PageId, bool)>,
        page: PageId,
        force: bool,
    ) -> Option<bool> {
        let pending = self.pending.as_mut().filter(|p| p.view.page() == page)?;
        pending.force |= force;
        queue.retain(|(queued, queued_force)| {
            if *queued == page {
                pending.force |= *queued_force;
                false
            } else {
                true
            }
        });
        Some(pending.force)
    }
    pub(super) fn queue_request(
        &mut self,
        queue: &mut VecDeque<(PageId, bool)>,
        page: PageId,
        force: bool,
    ) -> bool {
        self.coalesce_pending_request(queue, page, force).is_some()
            || queue_request(queue, page, force)
    }
    pub(super) fn admit(&mut self, page: PageId, view: u64, source: Version) {
        self.admitted.insert(page, (view, source));
    }
    pub(super) fn validates(&self, page: PageId, view: u64, source: Option<Version>) -> bool {
        self.admitted
            .get(&page)
            .is_some_and(|&(admitted, private)| admitted == view && Some(private) == source)
    }
    pub(super) fn forget(&mut self, page: PageId) {
        self.admitted.remove(&page);
        if self.pending_request().is_some_and(|(p, _)| p == page) {
            self.pending = None;
        }
    }
    pub(super) fn invalidate_observer(&mut self) {
        if let Some(pending) = &mut self.pending {
            pending.restart = true;
        }
    }
}

impl Server {
    pub(super) fn raw_projection_forget(&mut self, connection: u64, page: PageId) {
        if let Some(owner) = &mut self.geometry.projection {
            owner.forget_raw(connection, page);
        }
    }
    fn raw_eye(&self, peer: &Peer, space: SpaceId) -> Option<[f64; 3]> {
        let eye = peer.player.eye(1.);
        let local = if space == peer.space {
            eye
        } else {
            let from = self.world.space(peer.space)?.transform;
            let to = self.world.space(space)?.transform;
            let global = transform_point(from, eye);
            let translated = std::array::from_fn(|axis| global[axis] - to.translation[axis]);
            let [x, y, z, w] = to.rotation;
            rotate([-x, -y, -z, w], translated)
        };
        local
            .iter()
            .all(|v| v.is_finite() && v.abs() <= 29_000_000.)
            .then_some(local)
    }
    /// None means defer, retaining the previous admitted view. A canonical
    /// worker miss is never replaced with unsanitized cells or a guessed AIR page.
    pub(super) fn build_raw_view(
        &mut self,
        connection: u64,
        peer: &mut Peer,
        page: PageId,
        force: bool,
    ) -> Option<(Version, Box<[Cell; core_types::PAGE_VOLUME]>, bool)> {
        let force = force
            || peer
                .raw_view
                .pending_request()
                .is_some_and(|(pending, force)| pending == page && force);
        let source = self.raw_sources.source(page)?;
        let canonical = page.space == SpaceId(0) && (0..8).contains(&page.y);
        let mut sealed = if canonical {
            let owner = self.geometry.projection.as_mut()?;
            owner.interest_raw(connection, page, self.tick).ok()?;
            Some(owner.raw_cells(
                connection,
                page,
                &self.world,
                self.runtimes.height.index(),
                self.tick,
            )?)
        } else {
            // Outside the private generator profile, build an explicit public
            // structural baseline below; ore values still require observer LOS.
            None
        };
        let eye = self.raw_eye(peer, page.space);
        let mut source_probes = 0;
        let valid = peer.raw_view.pending.as_ref().is_some_and(|pending| {
            !pending.restart
                && pending.view.page() == page
                && pending.view.raw_source() == source.0
                && pending.eye == eye
                && pending.dependencies.iter().all(|(page, version)| {
                    source_probes += 1;
                    self.raw_sources.source(*page) == *version
                })
                && match (pending.policy.as_ref(), sealed.as_ref()) {
                    (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                    (Some(_), None) if !canonical => true,
                    (None, None) => true,
                    _ => false,
                }
        });
        self.counters.visibility_source_probes += source_probes;
        if !valid {
            let public_structure = !canonical;
            let copies = if public_structure { 2 } else { 1 };
            let cells = core_types::PAGE_VOLUME * copies;
            if peer.raw_view.budget.cells < cells || peer.raw_view.copies < copies {
                return None;
            }
            peer.raw_view.copies -= copies;
            let actual = self.world.page(page)?.cells();
            if public_structure {
                // Private canonical generation exists only in space0/Y0..127.
                // Other spaces and heights have public structural values; ores
                // still require observer LOS everywhere. In particular, flight
                // through public AIR above the profile cannot become false stone.
                let mut public = Arc::new(*actual);
                for cell in Arc::get_mut(&mut public)
                    .expect("new public grid image")
                    .iter_mut()
                {
                    if visibility::private_material(cell.id()) {
                        *cell = Cell::from_parts(rules::ids::STONE, 0);
                    }
                }
                peer.raw_view.budget.cells -= core_types::PAGE_VOLUME;
                self.counters.view_cells += core_types::PAGE_VOLUME as u64;
                self.counters.view_copy_bytes +=
                    (core_types::PAGE_VOLUME * std::mem::size_of::<Cell>()) as u64;
                sealed = Some(public);
            }
            self.counters.view_copy_bytes +=
                (core_types::PAGE_VOLUME * std::mem::size_of::<Cell>()) as u64;
            // An earlier observer view does not prove current visibility after
            // an intervening wall, Unknown page, or observer movement. Recheck
            // every value that differs from the certified public baseline.
            let view = visibility::PendingView::new(page, source.0, actual, sealed.clone()).ok()?;
            peer.raw_view.pending = Some(PendingRaw {
                view,
                force,
                policy: sealed,
                eye,
                dependencies: BTreeMap::new(),
                restart: false,
            });
        }
        let mut rays = visibility::ViewWork::default();
        let pending = peer.raw_view.pending.as_mut()?;
        pending.force |= force;
        let dependencies = std::cell::RefCell::new(&mut pending.dependencies);
        let overflow = std::cell::Cell::new(false);
        let cell_reads = std::cell::Cell::new(0u64);
        let source_captures = std::cell::Cell::new(0u64);
        let query = |position: [i32; 3]| {
            let position = BlockPos {
                space: page.space,
                x: position[0],
                y: position[1],
                z: position[2],
            };
            let key = position.page();
            let mut dependencies = dependencies.borrow_mut();
            if !dependencies.contains_key(&key) && dependencies.len() >= 256 {
                overflow.set(true);
                return CellRead::Unknown;
            }
            dependencies.entry(key).or_insert_with(|| {
                source_captures.set(source_captures.get() + 1);
                self.raw_sources.source(key)
            });
            cell_reads.set(cell_reads.get() + 1);
            self.world.read(position)
        };
        let work = pending.view.advance(&mut peer.raw_view.budget, |target| {
            visibility::target_visible(&query, target, pending.eye, &mut rays)
        });
        self.counters.view_cells += work.cells as u64;
        self.counters.visibility_cell_reads += cell_reads.get();
        self.counters.visibility_source_probes += source_captures.get();
        self.counters.visibility_segments += rays.segments;
        self.counters.visibility_rays += work.rays as u64;
        if overflow.get() {
            pending.restart = true;
            return None;
        }
        let cells = pending.view.take_completed()?;
        let force = pending.force;
        peer.raw_view.pending = None;
        Some((source, cells, force))
    }
    pub(super) fn public_summary(
        &mut self,
        connection: u64,
        peer: &Peer,
        page: PageId,
    ) -> Option<net::FarSummary> {
        if page.space == SpaceId(0) && (0..8).contains(&page.y) {
            let owner = self.geometry.projection.as_mut()?;
            owner.interest_raw(connection, page, self.tick).ok()?;
            let cells = owner.raw_cells(
                connection,
                page,
                &self.world,
                self.runtimes.height.index(),
                self.tick,
            )?;
            let source = owner
                .head(page, &self.world, self.runtimes.height.index())?
                .source;
            self.counters.summary_cells += core_types::PAGE_VOLUME as u64;
            let summary = visibility::summary(page, source, &cells);
            if !peer.pages.contains(&page)
                && !peer.page_requests.iter().any(|(p, _)| *p == page)
                && peer
                    .raw_view
                    .pending_request()
                    .is_none_or(|(p, _)| p != page)
            {
                owner.forget_raw(connection, page);
            }
            Some(summary)
        } else {
            let approved = peer.history.confirmed_page(page)?;
            peer.raw_view
                .validates(page, approved.version, self.raw_sources.source(page))
                .then(|| {
                    self.counters.summary_cells += core_types::PAGE_VOLUME as u64;
                    visibility::summary(page, approved.source_version, &approved.cells)
                })
        }
    }
}

#[cfg(test)]
mod tests;
