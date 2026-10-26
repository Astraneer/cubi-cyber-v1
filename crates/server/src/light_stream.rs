//! Confirmed-base LIGHT transport. Deltas read changed light entries only;
//! initial and missing-history snapshots alone read all4096 light samples.
use super::*;
use net::light::{Header, Request, Update};
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
        if let Some(ack) = request.acknowledged {
            if page.headers.iter().any(|h| h.version == ack)
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
    fn build(
        &mut self,
        connection: u64,
        request: Request,
        page: &world::Page,
    ) -> Result<Option<Update>, ServerError> {
        let source = page.channel_version(core_types::ChannelMask::LIGHTING);
        let peer = self
            .peers
            .get(&connection)
            .ok_or(ServerError::Invalid("light peer history"))?;
        let history = peer
            .pages
            .get(&request.page)
            .ok_or(ServerError::Invalid("light page history"))?;
        if request.acknowledged.is_some()
            && history.headers.back().is_some_and(|h| {
                h.cell_view == request.cell_view && history.confirmed.is_none_or(|v| v < h.version)
            })
        {
            return Ok(None);
        }
        let base = history
            .confirmed
            .and_then(|v| history.headers.iter().find(|h| h.version == v))
            .copied();
        let latest = history
            .headers
            .back()
            .copied()
            .filter(|h| h.cell_view == request.cell_view && h.source == source);
        if base.is_some() && base == latest {
            return Ok(None);
        }
        let changes = base.and_then(|h| {
            self.delta(request.page, h.source, source)
                .map(|c| (h.version, c))
        });
        let peer = self.peers.get_mut(&connection).expect("known peer");
        let header = if let Some(h) = latest {
            h
        } else {
            peer.next = peer
                .next
                .checked_add(1)
                .ok_or(ServerError::Invalid("light view revision exhausted"))?;
            let h = Header {
                page: request.page,
                cell_view: request.cell_view,
                version: peer.next,
                source,
            };
            let page = peer.pages.get_mut(&request.page).expect("known page");
            page.headers.push_back(h);
            while page.headers.len() > HISTORY {
                page.headers.pop_front();
            }
            h
        };
        if let Some((base, changes)) = changes {
            return Ok(Some(Update::Delta(net::light::Delta {
                header,
                base,
                changes,
            })));
        }
        let mut values = vec![0; core_types::PAGE_VOLUME].into_boxed_slice();
        for (index, value) in values.iter_mut().enumerate() {
            let sample = page.light(core_types::LocalIdx::new(index as u16).expect("page index"));
            *value = net::light::pack(sample.sky, sample.block, sample.rgb)
                .ok_or(ServerError::Invalid("light snapshot sample"))?;
        }
        Ok(Some(Update::Snapshot(net::light::Snapshot {
            header,
            values: values.try_into().expect("page volume"),
        })))
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
                let Some(page) = self.world.page(request.page) else {
                    output.push(Outgoing::reliable(
                        connection,
                        ServerMessage::LightUnavailable(request.page),
                    ));
                    continue;
                };
                match self.light_stream.build(connection, request, page) {
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
                        self.failed = true;
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
        let Update::Delta(rebound) = rebound else {
            panic!("binding-only change")
        };
        assert!(rebound.changes.is_empty());
        assert_eq!(rebound.header.source, delta.header.source);
        assert_eq!(rebound.header.cell_view, 9);
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
