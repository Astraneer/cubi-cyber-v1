//! Confirmed near-page pipe-item courier. Source rows are read only after WAL
//! acknowledgement. One ordinary in-flight image per page bounds slow-client history;
//! dirty pages coalesce while transport owns reliable retransmission.
use super::*;
use crate::machines::replication::PacketHistory;
use net::machine_packets::Request;
pub(super) const MAX_ROWS: usize = 2048;
const PAGE_ROWS: usize = net::machine_packets::MAX_PACKETS;
const MAX_PLANS: usize = 64;
const MAX_FRAMES: usize = 32;
const MAX_BYTES: usize = 128 * 1024;
const MAX_PROBES: usize = 8192;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    pub frames: u64,
    pub upserts: u64,
    pub removes: u64,
    pub probes: u64,
    pub bytes: u64,
    pub credit_waits: u64,
    pub capacity_waits: u64,
    pub oldest_age: u64,
    pub queue_peak: usize,
}
struct PageState {
    history: PacketHistory,
    rows: usize,
}
#[derive(Clone, Copy)]
struct Dirty {
    page: PageId,
    force: bool,
    first: Tick,
}
#[derive(Default)]
struct PeerState {
    pages: BTreeMap<PageId, PageState>,
    queue: VecDeque<Dirty>,
    rows: usize,
}
#[derive(Default)]
pub(super) struct Owner {
    peers: BTreeMap<u64, PeerState>,
    next_peer: u64,
    pub(super) counters: Counters,
}
impl Owner {
    pub(super) fn new() -> Self {
        Self::default()
    }
    pub(super) fn queue(&mut self, connection: u64, page: PageId, force: bool, now: Tick) {
        if !self.peers.contains_key(&connection) && self.peers.len() == MAX_CONNECTIONS {
            return;
        }
        let peer = self.peers.entry(connection).or_default();
        if let Some(old) = peer.queue.iter_mut().find(|p| p.page == page) {
            old.force |= force;
            old.first = old.first.min(now);
        } else if peer.queue.len() < MAX_AOI_PAGES {
            peer.queue.push_back(Dirty {
                page,
                force,
                first: now,
            });
            self.counters.queue_peak = self.counters.queue_peak.max(peer.queue.len());
        }
    }
    /// The caller checks the local (temporarily removed) Peer's ready/AOI/raw
    /// certificate before admitting this request; no client chooses source rows.
    pub(super) fn request(&mut self, connection: u64, request: Request, now: Tick) {
        let accepted = if let Some(page) = self
            .peers
            .get_mut(&connection)
            .and_then(|p| p.pages.get_mut(&request.page()))
        {
            page.history.request(request)
        } else {
            matches!(request, Request::Full { .. })
        };
        if accepted {
            self.queue(
                connection,
                request.page(),
                matches!(request, Request::Full { .. }),
                now,
            );
        }
    }
    pub(super) fn forget(&mut self, connection: u64, page: PageId) {
        if let Some(peer) = self.peers.get_mut(&connection) {
            if let Some(old) = peer.pages.remove(&page) {
                peer.rows -= old.rows;
            }
            peer.queue.retain(|p| p.page != page);
        }
    }
    pub(super) fn forget_peer(&mut self, connection: u64) {
        self.peers.remove(&connection);
    }
    fn dirty(
        &mut self,
        peers: &BTreeMap<u64, Peer>,
        pages: impl IntoIterator<Item = PageId>,
        now: Tick,
    ) {
        for page in pages {
            for (&connection, peer) in peers {
                if peer.ready && peer.pages.contains(&page) {
                    self.queue(connection, page, false, now);
                }
            }
        }
    }
}
impl Server {
    pub fn packet_stream_counters(&self) -> Counters {
        self.packet_stream.counters
    }
    pub(super) fn observe_packets(&mut self, changes: &[WorldChange]) -> Result<(), ServerError> {
        for change in changes {
            self.packet_stream.dirty(
                &self.peers,
                change
                    .pages
                    .iter()
                    .filter(|p| {
                        p.created
                            || p.removed
                            || p.channels.intersects(
                                core_types::ChannelMask::SHAPE
                                    | core_types::ChannelMask::CONNECTIONS,
                            )
                    })
                    .map(|p| p.page),
                self.tick,
            );
        }
        while let Some(page) = self.runtimes.packet_dirty.pop_first() {
            self.packet_stream.dirty(&self.peers, [page], self.tick);
        }
        Ok(())
    }
    pub(super) fn publish_packets(&mut self) -> Vec<Outgoing> {
        if self.pending.is_some() || self.failed {
            return Vec::new();
        }
        self.packet_stream
            .peers
            .retain(|id, _| self.peers.contains_key(id));
        if !self
            .packet_stream
            .peers
            .values()
            .any(|p| !p.queue.is_empty())
        {
            return Vec::new();
        }
        let mut connections: Vec<_> = self.packet_stream.peers.keys().copied().collect();
        connections.sort_by_key(|c| (*c <= self.packet_stream.next_peer, *c));
        let mut output = Vec::new();
        let mut plans = 0;
        let mut bytes = 0;
        for connection in connections {
            if let Some(peer) = self.peers.get(&connection)
                && let Some(state) = self.packet_stream.peers.get_mut(&connection)
            {
                let rows = &mut state.rows;
                state.pages.retain(|page, old| {
                    let keep = peer.pages.contains(page);
                    if !keep {
                        *rows -= old.rows;
                    }
                    keep
                });
                state.queue.retain(|dirty| peer.pages.contains(&dirty.page));
            }
            for _ in 0..4 {
                if plans == MAX_PLANS || output.len() == MAX_FRAMES || bytes >= MAX_BYTES {
                    return output;
                }
                let Some(dirty) = self
                    .packet_stream
                    .peers
                    .get_mut(&connection)
                    .and_then(|p| p.queue.pop_front())
                else {
                    break;
                };
                let previous_peer = self.packet_stream.next_peer;
                self.packet_stream.next_peer = connection;
                plans += 1;
                let allowed = self.peers.get(&connection).is_some_and(|peer| {
                    peer.ready
                        && peer.pages.contains(&dirty.page)
                        && self.in_aoi(peer, dirty.page, true)
                });
                if !allowed {
                    self.packet_stream.forget(connection, dirty.page);
                    continue;
                }
                let Some(public) = self
                    .peers
                    .get(&connection)
                    .and_then(|p| p.history.confirmed_page(dirty.page))
                else {
                    continue;
                };
                let view = Version(public.version);
                let state = self
                    .packet_stream
                    .peers
                    .get_mut(&connection)
                    .expect("queued peer");
                let was_present = state.pages.contains_key(&dirty.page);
                if !was_present
                    && self
                        .runtimes
                        .machines
                        .packets_in_page(dirty.page)
                        .next()
                        .is_none()
                {
                    continue;
                }
                let mut page = state
                    .pages
                    .remove(&dirty.page)
                    .unwrap_or_else(|| PageState {
                        history: PacketHistory::new(dirty.page, view, PAGE_ROWS, 2)
                            .expect("bounded page history"),
                        rows: 0,
                    });
                if page.history.view_version() != view {
                    page.history.set_view_version(view);
                }
                self.packet_stream.counters.oldest_age = self
                    .packet_stream
                    .counters
                    .oldest_age
                    .max(self.tick.0.saturating_sub(dirty.first.0));
                if page.history.has_unconfirmed() && !dirty.force {
                    self.packet_stream.counters.credit_waits += 1;
                    state.pages.insert(dirty.page, page);
                    state.queue.push_back(dirty);
                    continue;
                }
                if dirty.force {
                    page.history.request(Request::Full {
                        page: dirty.page,
                        view_version: view,
                        have: page.history.confirmed(),
                    });
                }
                let available = MAX_ROWS
                    .saturating_sub(state.rows)
                    .saturating_add(page.rows)
                    .min(PAGE_ROWS);
                // Budget enough for a worst-case frame BEFORE history mutates.
                let worst = net::machine_packets::HEADER_BYTES
                    + PAGE_ROWS * (net::machine_packets::ROW_BYTES + 8);
                if bytes + worst > MAX_BYTES {
                    state.pages.insert(dirty.page, page);
                    state.queue.push_back(dirty);
                    self.packet_stream.next_peer = previous_peer;
                    return output;
                }
                let result=page.history.plan_filtered(&self.runtimes.machines,self.tick,&|position|{
                    let index=position.local_index();matches!(self.world.read(position),CellRead::Known(cell) if cell.id()==rules::ids::LOGISTICAL_TRANSPORTER&&public.cells[usize::from(index.get())]==cell)
                },available,MAX_PROBES);
                match result {
                    Ok((frame, work)) => {
                        self.packet_stream.counters.probes += work.indexed_rows;
                        let count = page.history.latest_count();
                        state.rows = state.rows - page.rows + count;
                        page.rows = count;
                        if let Some(frame) = frame
                            && (was_present || count != 0)
                        {
                            let size = net::machine_packets::HEADER_BYTES
                                + frame.upserts.len() * net::machine_packets::ROW_BYTES
                                + frame.removes.len() * 8;
                            bytes += size;
                            self.packet_stream.counters.frames += 1;
                            self.packet_stream.counters.bytes += size as u64;
                            self.packet_stream.counters.upserts += frame.upserts.len() as u64;
                            self.packet_stream.counters.removes += frame.removes.len() as u64;
                            output.push(Outgoing::reliable(
                                connection,
                                ServerMessage::MachinePackets(Box::new(frame)),
                            ));
                        }
                        // Empty unseen pages reveal nothing and retain no row
                        // history. Existing empty pages keep monotonic revisions.
                        if was_present || count != 0 {
                            state.pages.insert(dirty.page, page);
                        }
                    }
                    Err(machines::MachineError::Bounds) => {
                        self.packet_stream.counters.capacity_waits += 1;
                        state.pages.insert(dirty.page, page);
                        state.queue.push_back(dirty);
                    }
                    Err(_) => {
                        state.pages.insert(dirty.page, page);
                        self.fail("publish_packets");
                        return output;
                    }
                }
            }
        }
        output
    }
}
