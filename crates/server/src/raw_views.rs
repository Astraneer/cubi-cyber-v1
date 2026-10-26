//! Near views consume certified worker projection bodies. Per-observer queries
//! are resumable and bounded; no private partial image leaves this owner.
use super::*;
use std::sync::Arc;

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
        self.pending = None;
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
            pending.view.page() == page
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
            peer.raw_view.pending = None;
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
            peer.raw_view.pending = None;
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
