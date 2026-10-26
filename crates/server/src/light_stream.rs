//! Confirmed-base LIGHT transport. Deltas read changed light entries only;
//! initial and missing-history snapshots alone read all4096 light samples.
use super::*;
use net::light::{Header, Request, Update};
mod projected;
const JOURNAL_ENTRIES: usize = 262_144;
const JOURNAL_BATCHES: usize = 4096;
const HISTORY: usize = 4;
struct ChangeBatch {
    base: Version,
    source: Version,
    changes: Vec<(u16, u32)>,
}
#[derive(Default)]
struct PageHistory {
    headers: VecDeque<Header>,
    confirmed: Option<u64>,
    public_values: Option<projected::Values>,
    public_source: u64,
    private_source: Option<Version>,
    public_cell_view: Option<u64>,
}
#[derive(Default)]
struct PeerHistory {
    pages: BTreeMap<PageId, PageHistory>,
    requests: VecDeque<Request>,
    next: u64,
}
pub(super) struct LightStream {
    peers: BTreeMap<u64, PeerHistory>,
    sources: BTreeMap<PageId, Version>,
    journal: BTreeMap<PageId, VecDeque<ChangeBatch>>,
    order: VecDeque<(PageId, Version)>,
    entries: usize,
    next_peer: u64,
}
impl LightStream {
    pub(super) fn new(world: &World) -> Self {
        Self {
            peers: BTreeMap::new(),
            sources: world
                .pages()
                .map(|(page, data)| {
                    (
                        page,
                        data.channel_version(core_types::ChannelMask::LIGHTING),
                    )
                })
                .collect(),
            journal: BTreeMap::new(),
            order: VecDeque::new(),
            entries: 0,
            next_peer: 0,
        }
    }
    pub(super) fn forget_peer(&mut self, connection: u64) {
        self.peers.remove(&connection);
    }
    pub(super) fn forget(&mut self, connection: u64, page: PageId) {
        if let Some(peer) = self.peers.get_mut(&connection) {
            peer.pages.remove(&page);
            peer.requests.retain(|r| r.page != page);
        }
    }
    pub(super) fn request(&mut self, connection: u64, mut request: Request) {
        if !self.peers.contains_key(&connection) && self.peers.len() >= MAX_CONNECTIONS {
            return;
        }
        let peer = self.peers.entry(connection).or_default();
        if !peer.pages.contains_key(&request.page) && peer.pages.len() >= MAX_AOI_PAGES {
            return;
        }
        let page = peer.pages.entry(request.page).or_default();
        if page
            .headers
            .back()
            .is_some_and(|h| h.cell_view > request.cell_view)
            || peer
                .requests
                .iter()
                .any(|old| old.page == request.page && old.cell_view > request.cell_view)
        {
            return;
        }
        // A queued full repair survives late ACKs of the same binding. Reliable
        // datagrams can finish out of order; an old ACK cannot restore a base
        // that the client explicitly dropped after sending it. The credit is
        // consumed only when publication pops this request.
        if peer.requests.iter().any(|old| {
            old.page == request.page
                && old.cell_view == request.cell_view
                && old.acknowledged.is_none()
        }) {
            request.acknowledged = None;
        }
        if let Some(ack) = request.acknowledged {
            if page
                .headers
                .iter()
                .any(|h| h.version == ack && h.cell_view <= request.cell_view)
                && page.confirmed.is_none_or(|old| ack >= old)
            {
                page.confirmed = Some(ack);
            } else if page.confirmed.is_none_or(|old| ack >= old) {
                page.confirmed = None;
                request.acknowledged = None;
            }
        } else {
            page.confirmed = None;
        }
        if let Some(old) = peer.requests.iter_mut().find(|r| r.page == request.page) {
            if request.cell_view >= old.cell_view {
                *old = request;
            }
        } else if peer.requests.len() < MAX_AOI_PAGES {
            peer.requests.push_back(request);
        }
    }
    pub(super) fn queue_auto(&mut self, connection: u64, page: PageId, cell_view: u64) {
        // One automatic image per unacknowledged page supplies stream credit.
        // The eventual ACK request reads the latest source; reliable transport
        // owns retransmission, so a slow loading frame cannot grow a snapshot log.
        if self
            .peers
            .get(&connection)
            .and_then(|p| p.pages.get(&page))
            .is_some_and(|p| {
                p.headers.back().is_some_and(|h| {
                    h.cell_view == cell_view && p.confirmed.is_none_or(|v| v < h.version)
                })
            })
        {
            return;
        }
        let acknowledged = self
            .peers
            .get(&connection)
            .and_then(|p| p.pages.get(&page))
            .and_then(|p| p.confirmed);
        self.request(
            connection,
            Request {
                page,
                cell_view,
                acknowledged,
            },
        );
    }
    fn observe(&mut self, changes: &[WorldChange]) -> Result<BTreeSet<PageId>, ServerError> {
        let mut dirty = BTreeSet::new();
        for change in changes {
            for page in &change.pages {
                if page.created {
                    self.sources.insert(page.page, page.after);
                }
                if page.removed {
                    self.sources.remove(&page.page);
                    if let Some(records) = self.journal.remove(&page.page) {
                        self.entries = self
                            .entries
                            .saturating_sub(records.iter().map(|r| r.changes.len()).sum::<usize>());
                    }
                    dirty.insert(page.page);
                }
            }
            let mut grouped: BTreeMap<PageId, Vec<(u16, u32)>> = BTreeMap::new();
            for entry in &change.lights {
                let value = net::light::pack(entry.new.sky, entry.new.block, entry.new.rgb)
                    .ok_or(ServerError::Invalid("public light sample"))?;
                grouped
                    .entry(entry.page)
                    .or_default()
                    .push((entry.index.get(), value));
            }
            for (page, mut entries) in grouped {
                entries.sort_unstable_by_key(|p| p.0);
                let source = change
                    .pages
                    .iter()
                    .find(|p| p.page == page)
                    .ok_or(ServerError::Invalid("light page source record"))?
                    .after;
                let base = self.sources.insert(page, source).unwrap_or(Version(0));
                self.entries += entries.len();
                self.order.push_back((page, source));
                self.journal
                    .entry(page)
                    .or_default()
                    .push_back(ChangeBatch {
                        base,
                        source,
                        changes: entries,
                    });
                dirty.insert(page);
            }
        }
        while self.entries > JOURNAL_ENTRIES || self.order.len() > JOURNAL_BATCHES {
            let Some((page, source)) = self.order.pop_front() else {
                break;
            };
            if let Some(history) = self.journal.get_mut(&page) {
                if history.front().is_some_and(|r| r.source == source) {
                    let old = history.pop_front().expect("checked record");
                    self.entries -= old.changes.len();
                }
                if history.is_empty() {
                    self.journal.remove(&page);
                }
            }
        }
        Ok(dirty)
    }
    fn delta(&self, page: PageId, base: Version, source: Version) -> Option<Vec<(u16, u32)>> {
        if base == source {
            return Some(Vec::new());
        }
        let mut cursor = base;
        let mut changes = BTreeMap::new();
        for record in self
            .journal
            .get(&page)?
            .iter()
            .filter(|r| r.source > base && r.source <= source)
        {
            if record.base != cursor {
                return None;
            }
            cursor = record.source;
            changes.extend(record.changes.iter().copied());
        }
        (cursor == source).then(|| changes.into_iter().collect())
    }
    #[cfg(test)]
    fn build(
        &mut self,
        connection: u64,
        request: Request,
        page: &world::Page,
    ) -> Result<Option<Update>, ServerError> {
        self.build_admitted(connection, request, page, &page.cells())
    }
    fn build_admitted(
        &mut self,
        connection: u64,
        request: Request,
        page: &world::Page,
        admitted: &[Cell; core_types::PAGE_VOLUME],
    ) -> Result<Option<Update>, ServerError> {
        self.build_projected(connection, request, page, admitted)
    }
}
impl Server {
    pub(super) fn observe_lights(&mut self, changes: &[WorldChange]) -> Result<(), ServerError> {
        let dirty = self.light_stream.observe(changes)?;
        for (connection, peer) in self.peers.iter().filter(|(_, p)| p.ready) {
            for page in &dirty {
                if peer.pages.contains(page)
                    && let Some(view) = peer.history.confirmed_version(*page)
                {
                    self.light_stream.queue_auto(*connection, *page, view);
                }
            }
        }
        Ok(())
    }
    pub(super) fn publish_lights(&mut self) -> Vec<Outgoing> {
        let mut output = Vec::new();
        if !self
            .light_stream
            .peers
            .values()
            .any(|p| !p.requests.is_empty())
        {
            return output;
        }
        let mut connections: Vec<_> = self.light_stream.peers.keys().copied().collect();
        connections.sort_by_key(|c| (*c <= self.light_stream.next_peer, *c));
        for connection in connections {
            for _ in 0..2 {
                let request = self
                    .light_stream
                    .peers
                    .get_mut(&connection)
                    .and_then(|p| p.requests.pop_front());
                let Some(mut request) = request else { break };
                self.light_stream.next_peer = connection;
                let allowed = self
                    .peers
                    .get(&connection)
                    .filter(|p| {
                        p.ready
                            && p.pages.contains(&request.page)
                            && self.in_aoi(p, request.page, true)
                    })
                    .and_then(|p| p.history.confirmed_version(request.page));
                let Some(cell_view) = allowed else {
                    self.light_stream.forget(connection, request.page);
                    continue;
                };
                request.cell_view = cell_view;
                let Some(admitted) = self
                    .peers
                    .get(&connection)
                    .and_then(|peer| peer.history.confirmed_page(request.page))
                else {
                    continue;
                };
                if !self.peers[&connection].raw_view.validates(
                    request.page,
                    admitted.version,
                    self.raw_sources.source(request.page),
                ) {
                    // New cell truth has not been admitted yet. Preserve the
                    // request and old LIGHT; never bind new samples to old cells.
                    self.light_stream.request(connection, request);
                    continue;
                }
                let Some(page) = self.world.page(request.page) else {
                    output.push(Outgoing::reliable(
                        connection,
                        ServerMessage::LightUnavailable(request.page),
                    ));
                    continue;
                };
                match self
                    .light_stream
                    .build_admitted(connection, request, page, &admitted.cells)
                {
                    Ok(Some(update)) => {
                        match &update {
                            Update::Snapshot(_) => {
                                self.counters.light_snapshot_cells += 4096;
                                self.counters.light_snapshots += 1;
                            }
                            Update::Delta(d) => {
                                self.counters.light_delta_cells += d.changes.len() as u64;
                                self.counters.light_deltas += 1;
                            }
                        }
                        output.push(Outgoing::reliable(
                            connection,
                            ServerMessage::Light(Box::new(update)),
                        ));
                    }
                    Ok(None) => {}
                    Err(_) => {
                        self.fail("publish_lights");
                        break;
                    }
                }
            }
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn position() -> BlockPos {
        BlockPos {
            space: SpaceId(0),
            x: 2,
            y: 3,
            z: 4,
        }
    }
    fn fixture() -> World {
        let mut w = World::new(rules::properties, 128);
        let mut tx = Transaction::new(Tick(1), SpaceId(0));
        tx.create_page(position().page(), vec![Cell::AIR; 4096]);
        w.commit(tx).unwrap();
        w
    }
    fn request(view: u64, acknowledged: Option<u64>) -> Request {
        Request {
            page: position().page(),
            cell_view: view,
            acknowledged,
        }
    }
    #[test]
    fn same_binding_late_ack_cannot_retire_a_queued_missing_base_repair() {
        let world = fixture();
        let mut stream = LightStream::new(&world);
        stream.request(1, request(8, None));
        let initial = stream
            .build(1, request(8, None), world.page(position().page()).unwrap())
            .unwrap()
            .unwrap();
        stream.peers.get_mut(&1).unwrap().requests.clear();
        let ack = request(8, Some(initial.header().version));
        stream.request(1, ack);
        stream.peers.get_mut(&1).unwrap().requests.clear();

        // Reliable UDP messages can finish out of order. An older ACK of the
        // same binding does not prove the client's lost base has been restored.
        stream.request(1, request(8, None));
        stream.request(1, ack);
        stream.queue_auto(1, position().page(), 8);
        let pending = stream
            .peers
            .get_mut(&1)
            .unwrap()
            .requests
            .pop_front()
            .unwrap();
        assert_eq!(pending, request(8, None));
        assert_eq!(stream.peers[&1].pages[&position().page()].confirmed, None);
        let repair = stream
            .build(1, pending, world.page(position().page()).unwrap())
            .unwrap()
            .unwrap();
        assert!(matches!(repair, Update::Snapshot(_)));
        assert_eq!(repair.header(), initial.header());
        // After that credit is serviced, a fresh ACK resumes delta/no-op use.
        stream.request(1, ack);
        let next = stream
            .peers
            .get_mut(&1)
            .unwrap()
            .requests
            .pop_front()
            .unwrap();
        assert_eq!(next.acknowledged, ack.acknowledged);
        assert!(
            stream
                .build(1, next, world.page(position().page()).unwrap())
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn light_binding_rebase_and_sparse_delta_are_independent_of_cell_versions() {
        let mut world = fixture();
        let mut stream = LightStream::new(&world);
        stream.request(1, request(8, None));
        let first = stream
            .build(1, request(8, None), world.page(position().page()).unwrap())
            .unwrap()
            .unwrap();
        assert!(matches!(first, Update::Snapshot(_)));
        let initial = first.header();
        stream.request(1, request(8, Some(initial.version)));
        let mut tx = Transaction::new(Tick(2), SpaceId(0));
        let value = world::LightSample {
            sky: 4,
            block: 7,
            rgb: [255, 17, 240],
        };
        tx.set_light(position(), value);
        let record = world.commit(tx).unwrap();
        stream.observe(&[record]).unwrap();
        let next = stream
            .build(
                1,
                request(8, Some(initial.version)),
                world.page(position().page()).unwrap(),
            )
            .unwrap()
            .unwrap();
        let Update::Delta(delta) = next else {
            panic!("retained light base must produce sparse delta")
        };
        assert_eq!(delta.base, initial.version);
        assert_eq!(delta.header.cell_view, 8);
        assert_eq!(
            delta.changes,
            vec![(
                position().local_index().get(),
                net::light::pack(4, 7, [255, 17, 240]).unwrap()
            )]
        );
        stream.request(1, request(9, Some(delta.header.version)));
        let rebound = stream
            .build(
                1,
                request(9, Some(delta.header.version)),
                world.page(position().page()).unwrap(),
            )
            .unwrap()
            .unwrap();
        let Update::Snapshot(rebound) = rebound else {
            panic!("new raw binding requires a complete admitted image")
        };
        assert_eq!(
            rebound.values[usize::from(position().local_index().get())],
            net::light::pack(4, 7, [255, 17, 240]).unwrap()
        );
        assert_eq!(rebound.header.source, delta.header.source);
        assert_eq!(rebound.header.cell_view, 9);
    }
    #[test]
    fn admitted_cells_mask_hidden_glow_and_visibility_rebase_uses_a_full_image() {
        let mut world = fixture();
        let page_id = position().page();
        let index = usize::from(position().local_index().get());
        let lava = Cell::from_parts(rules::ids::LAVA, 0);
        let mut transaction = Transaction::new(Tick(2), SpaceId(0));
        transaction.set_cell(position(), lava);
        transaction.set_light(
            position(),
            world::LightSample {
                sky: 0,
                block: 15,
                rgb: [255, 13, 0],
            },
        );
        world.commit(transaction).unwrap();
        let mut admitted = world.page(page_id).unwrap().cells();
        admitted[index] = Cell::from_parts(rules::ids::STONE, 0);
        let mut stream = LightStream::new(&world);
        stream.request(1, request(1, None));
        let image = stream
            .build_admitted(1, request(1, None), world.page(page_id).unwrap(), &admitted)
            .unwrap()
            .unwrap();
        let first = image.header();
        let Update::Snapshot(image) = image else {
            panic!("initial snapshot")
        };
        assert_eq!(
            image.values[index], 0,
            "closed lava sample must not cross the raw mask"
        );
        assert_ne!(
            world
                .page(page_id)
                .unwrap()
                .light(position().local_index())
                .rgb,
            [0; 3]
        );
        stream.request(1, request(1, Some(first.version)));
        let mut transaction = Transaction::new(Tick(3), SpaceId(0));
        transaction.set_light(
            position(),
            world::LightSample {
                sky: 0,
                block: 14,
                rgb: [251, 34, 50],
            },
        );
        let change = world.commit(transaction).unwrap();
        stream.observe(&[change]).unwrap();
        let hidden = stream
            .build_admitted(
                1,
                request(1, Some(first.version)),
                world.page(page_id).unwrap(),
                &admitted,
            )
            .unwrap();
        assert!(
            hidden.is_none(),
            "hidden-only updates cannot change packets or public sources"
        );
        let hidden_header = first;
        admitted[index] = lava;
        stream.request(1, request(2, Some(hidden_header.version)));
        let revealed = stream
            .build_admitted(
                1,
                request(2, Some(hidden_header.version)),
                world.page(page_id).unwrap(),
                &admitted,
            )
            .unwrap()
            .unwrap();
        let revealed_header = revealed.header();
        let Update::Snapshot(revealed) = revealed else {
            panic!("new raw mask requires a full LIGHT image")
        };
        assert_eq!(
            revealed.values[index],
            net::light::pack(0, 14, [251, 34, 50]).unwrap()
        );
        assert!(
            revealed_header.source > hidden_header.source,
            "public values changed after real visibility"
        );
        assert_eq!(
            world
                .page(page_id)
                .unwrap()
                .channel_version(core_types::ChannelMask::LIGHTING),
            Version(3),
            "visibility alone cannot change canonical light"
        );
        admitted[index] = Cell::from_parts(rules::ids::STONE, 0);
        stream.request(1, request(3, Some(revealed_header.version)));
        let sealed = stream
            .build_admitted(
                1,
                request(3, Some(revealed_header.version)),
                world.page(page_id).unwrap(),
                &admitted,
            )
            .unwrap()
            .unwrap();
        let Update::Snapshot(sealed) = sealed else {
            panic!("mask removal requires snapshot")
        };
        assert_eq!(sealed.values[index], 0);
    }
    #[test]
    fn new_cell_binding_bypasses_an_old_unacknowledgeable_light_credit() {
        let mut world = fixture();
        let mut stream = LightStream::new(&world);
        stream.request(1, request(1, None));
        let initial = stream
            .build(1, request(1, None), world.page(position().page()).unwrap())
            .unwrap()
            .unwrap();
        let mut replica = net::light::Replica::new(1).unwrap();
        replica.apply(initial.clone(), Some(1)).unwrap();
        let initial_version = initial.header().version;
        stream.request(1, request(1, Some(initial_version)));
        let mut tx = Transaction::new(Tick(2), SpaceId(0));
        tx.set_light(
            position(),
            world::LightSample {
                sky: 7,
                block: 4,
                rgb: [17, 23, 255],
            },
        );
        stream.observe(&[world.commit(tx).unwrap()]).unwrap();
        let unacknowledged = stream
            .build(
                1,
                request(1, Some(initial_version)),
                world.page(position().page()).unwrap(),
            )
            .unwrap()
            .unwrap();
        // Raw cells rebind before the old LIGHT packet arrives. Rejecting it is
        // mandatory, so that obsolete packet can never earn a later ACK.
        assert_eq!(
            replica.apply(unacknowledged.clone(), Some(2)).unwrap(),
            net::light::Apply::Stale
        );
        stream.request(1, request(2, Some(initial_version)));
        let repair = stream
            .build(
                1,
                request(2, Some(initial_version)),
                world.page(position().page()).unwrap(),
            )
            .unwrap()
            .expect("new binding must not wait for obsolete LIGHT credit");
        assert!(repair.header().version > unacknowledged.header().version);
        assert_eq!(repair.header().cell_view, 2);
        replica.apply(repair, Some(2)).unwrap();
        assert_eq!(
            replica.page(position().page()).unwrap().values
                [position().local_index().get() as usize],
            net::light::pack(7, 4, [17, 23, 255]).unwrap()
        );
    }
    #[test]
    fn pruned_light_journal_forces_complete_snapshot_instead_of_partial_delta() {
        let mut world = fixture();
        let mut stream = LightStream::new(&world);
        stream.request(1, request(1, None));
        let original = stream
            .build(1, request(1, None), world.page(position().page()).unwrap())
            .unwrap()
            .unwrap()
            .header();
        stream.request(1, request(1, Some(original.version)));
        for tick in 2..=70 {
            let mut tx = Transaction::new(Tick(tick), SpaceId(0));
            for index in 0..4096 {
                let local = core_types::LocalIdx::new(index).unwrap();
                let (x, y, z) = local.xyz();
                tx.set_light(
                    BlockPos {
                        space: SpaceId(0),
                        x: i32::from(x),
                        y: i32::from(y),
                        z: i32::from(z),
                    },
                    world::LightSample {
                        sky: (tick % 15) as u8,
                        block: 0,
                        rgb: [tick as u8, 0, 255],
                    },
                );
            }
            let record = world.commit(tx).unwrap();
            stream.observe(&[record]).unwrap();
        }
        assert!(stream.entries <= JOURNAL_ENTRIES);
        assert!(stream.order.len() <= JOURNAL_BATCHES);
        let Update::Snapshot(snapshot) = stream
            .build(
                1,
                request(1, Some(original.version)),
                world.page(position().page()).unwrap(),
            )
            .unwrap()
            .unwrap()
        else {
            panic!("missing history cannot emit an incomplete delta")
        };
        assert_eq!(
            snapshot.values.as_ref(),
            &[net::light::pack(10, 0, [70, 0, 255]).unwrap(); 4096]
        );
    }
}

#[cfg(test)]
mod binding_fallback_contract {
    use super::*;
    #[test]
    fn discarded_current_binding_image_is_recovered_by_an_explicit_no_ack_fallback() {
        let page = PageId {
            space: SpaceId(0),
            x: 0,
            y: 0,
            z: 0,
        };
        let mut world = World::new(rules::properties, 64);
        let mut tx = Transaction::new(Tick(1), SpaceId(0));
        tx.create_page(page, vec![Cell::AIR; 4096]);
        world.commit(tx).unwrap();
        let mut stream = LightStream::new(&world);
        let request = |cell_view, acknowledged| Request {
            page,
            cell_view,
            acknowledged,
        };
        stream.request(1, request(1, None));
        let a = stream
            .build(1, request(1, None), world.page(page).unwrap())
            .unwrap()
            .unwrap();
        stream.request(1, request(1, Some(a.header().version)));
        stream.request(1, request(2, Some(a.header().version)));
        let b = stream
            .build(
                1,
                request(2, Some(a.header().version)),
                world.page(page).unwrap(),
            )
            .unwrap()
            .unwrap();
        // The receiver no longer has that raw binding and cannot accept B.
        // Repeating the obsolete A ACK correctly cannot grant B stream credit.
        stream.request(1, request(2, Some(a.header().version)));
        assert!(
            stream
                .build(
                    1,
                    request(2, Some(a.header().version)),
                    world.page(page).unwrap()
                )
                .unwrap()
                .is_none()
        );
        // Once raw2 is restored, the client now sends None instead of A.
        stream.request(1, request(2, None));
        let repair = stream
            .build(1, request(2, None), world.page(page).unwrap())
            .unwrap()
            .unwrap();
        assert!(matches!(&repair, Update::Snapshot(_)));
        assert_eq!(repair.header(), b.header());
        let mut replica = net::light::Replica::new(1).unwrap();
        replica.apply(a, Some(1)).unwrap();
        replica.apply(repair, Some(2)).unwrap();
        assert_eq!(replica.page(page).unwrap().header.cell_view, 2);
    }
}

#[cfg(test)]
mod stale_binding_request_contract {
    use super::*;
    #[test]
    fn delayed_old_binding_ack_cannot_change_or_delta_encode_a_queued_full_repair() {
        let page = PageId {
            space: SpaceId(0),
            x: 0,
            y: 0,
            z: 0,
        };
        let mut world = World::new(rules::properties, 64);
        let mut tx = Transaction::new(Tick(1), SpaceId(0));
        tx.create_page(page, vec![Cell::AIR; 4096]);
        world.commit(tx).unwrap();
        let mut stream = LightStream::new(&world);
        let old = Request {
            page,
            cell_view: 1,
            acknowledged: None,
        };
        stream.request(1, old);
        let first = stream
            .build(1, old, world.page(page).unwrap())
            .unwrap()
            .unwrap();
        stream.peers.get_mut(&1).unwrap().requests.clear();
        let repair = Request {
            page,
            cell_view: 2,
            acknowledged: None,
        };
        stream.request(1, repair);
        stream.request(
            1,
            Request {
                acknowledged: Some(first.header().version),
                ..old
            },
        );
        let pending = stream
            .peers
            .get_mut(&1)
            .unwrap()
            .requests
            .pop_front()
            .unwrap();
        assert_eq!(pending, repair);
        assert_eq!(stream.peers[&1].pages[&page].confirmed, None);
        // Even a retained known base cannot turn an explicit full request into a
        // delta after another response has updated internal confirmation metadata.
        stream
            .peers
            .get_mut(&1)
            .unwrap()
            .pages
            .get_mut(&page)
            .unwrap()
            .confirmed = Some(first.header().version);
        let image = stream
            .build(1, pending, world.page(page).unwrap())
            .unwrap()
            .unwrap();
        assert!(matches!(image, Update::Snapshot(_)));
        let mut replica = net::light::Replica::new(1).unwrap();
        replica.apply(image, Some(2)).unwrap();
    }
}
