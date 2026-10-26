//! Bounded near-model state, derived only after the durable owner frontier.
use super::*;
use core_types::{ChannelMask, ComponentId};
use net::props::Frame;
use std::sync::Arc;
#[cfg(test)]
#[path = "forget_page_tests.rs"]
mod forget_page_tests;
const MAX_HINTS_PER_PEER: usize = 8192;
#[derive(Default)]
struct PeerProps {
    heads: BTreeMap<PageId, Arc<Frame>>,
    queue: VecDeque<(PageId, bool)>,
    next: u64,
    hints: usize,
}
struct Lid {
    page: PageId,
    value: u16,
    target: bool,
    last: Tick,
}
#[derive(Default)]
pub(super) struct PropsStream {
    peers: BTreeMap<u64, PeerProps>,
    lids: BTreeMap<ComponentId, Lid>,
    last_tick: Option<Tick>,
}
impl PropsStream {
    pub(super) fn forget(&mut self, connection: u64, page: PageId) {
        if let Some(state) = self.peers.get_mut(&connection) {
            if let Some(head) = state.heads.remove(&page) {
                state.hints -= head.hints.len();
            }
            state.queue.retain(|(pending, _)| *pending != page);
        }
    }
    pub(super) fn queue(&mut self, connection: u64, page: PageId, force: bool) {
        if self.peers.len() >= MAX_CONNECTIONS && !self.peers.contains_key(&connection) {
            return;
        }
        let state = self.peers.entry(connection).or_default();
        if let Some((_, old)) = state.queue.iter_mut().find(|(p, _)| *p == page) {
            *old |= force;
        } else if state.queue.len() < MAX_AOI_PAGES {
            state.queue.push_back((page, force));
        }
    }
    pub(super) fn dirty(
        &mut self,
        peers: &BTreeMap<u64, Peer>,
        pages: impl IntoIterator<Item = PageId>,
    ) {
        for page in pages {
            for (&connection, peer) in peers {
                if peer.ready && peer.pages.contains(&page) {
                    self.queue(connection, page, false);
                }
            }
        }
    }
    fn animate(&mut self, peers: &BTreeMap<u64, Peer>, world: &World, tick: Tick) {
        if self.last_tick == Some(tick) {
            return;
        }
        self.last_tick = Some(tick);
        for lid in self.lids.values_mut() {
            lid.target = false;
        }
        for peer in peers
            .values()
            .filter(|p| p.ready && !p.window.state.closed())
        {
            let Some(id) = peer.window.binding.and_then(|b| b.component) else {
                continue;
            };
            let Some(component) = world
                .component(id)
                .filter(|c| c.data.kind == survival::CHEST_COMPONENT)
            else {
                continue;
            };
            if self.lids.len() >= 1024 && !self.lids.contains_key(&id) {
                continue;
            }
            self.lids
                .entry(id)
                .or_insert(Lid {
                    page: component.data.anchor.page(),
                    value: 0,
                    target: true,
                    last: tick,
                })
                .target = true;
        }
        let mut changed = [None; 1024];
        let mut count = 0;
        for lid in self.lids.values_mut() {
            let step = tick
                .0
                .saturating_sub(lid.last.0)
                .saturating_mul(6554)
                .min(65535) as u16;
            let value = if lid.target {
                lid.value.saturating_add(step)
            } else {
                lid.value.saturating_sub(step)
            };
            lid.last = tick;
            if value != lid.value {
                lid.value = value;
                changed[count] = Some(lid.page);
                count += 1;
            }
        }
        self.dirty(peers, changed[..count].iter().flatten().copied());
        self.lids.retain(|_, lid| lid.target || lid.value != 0);
    }
}
fn same_visual(old: &Frame, new: &Frame) -> bool {
    old.cell_view == new.cell_view
        && old.hints.len() == new.hints.len()
        && old.hints.iter().zip(&new.hints).all(|(a, b)| {
            if a.index != b.index
                || a.kind != b.kind
                || a.flags != b.flags
                || a.ports != b.ports
                || a.lid != b.lid
                || a.rotor_velocity != b.rotor_velocity
            {
                return false;
            }
            if a.flags & net::props::ROTOR == 0 {
                return a.rotor == b.rotor;
            }
            let elapsed = new.tick.0.saturating_sub(old.tick.0);
            let predicted = a
                .rotor
                .wrapping_add((i128::from(a.rotor_velocity) * i128::from(elapsed)) as u16);
            let delta = predicted.wrapping_sub(b.rotor);
            delta <= 2 || delta >= u16::MAX - 1
        })
}
impl Server {
    pub(super) fn observe_props(&mut self, changes: &[WorldChange]) {
        let channels = ChannelMask(
            ChannelMask::SHAPE.0 | ChannelMask::CONTENTS.0 | ChannelMask::CONNECTIONS.0,
        );
        for change in changes {
            self.props_stream.dirty(
                &self.peers,
                change
                    .pages
                    .iter()
                    .filter(|p| p.created || p.removed || p.channels.intersects(channels))
                    .map(|p| p.page),
            );
        }
        while let Some(page) = self.runtimes.props_dirty.pop_first() {
            self.props_stream.dirty(&self.peers, [page]);
        }
    }
    pub(super) fn publish_props(&mut self) -> Vec<Outgoing> {
        if self.pending.is_some() || self.failed {
            return Vec::new();
        }
        let stream = &mut self.props_stream;
        stream.peers.retain(|id, _| self.peers.contains_key(id));
        stream.animate(&self.peers, &self.world, self.tick);
        let mut output = Vec::new();
        for (&connection, state) in &mut stream.peers {
            let Some(peer) = self.peers.get(&connection).filter(|p| p.ready) else {
                continue;
            };
            state.heads.retain(|page, head| {
                if !peer.pages.contains(page) {
                    state.hints -= head.hints.len();
                    false
                } else {
                    true
                }
            });
            for _ in 0..2 {
                let Some((page, force)) = state.queue.pop_front() else {
                    break;
                };
                if !peer.pages.contains(&page) {
                    continue;
                }
                let Some(public) = peer.history.confirmed_page(page) else {
                    continue;
                };
                let Some(next) = state.next.checked_add(1) else {
                    self.fail("publish_props");
                    return Vec::new();
                };
                let stamp = props::Stamp {
                    page,
                    cell_view: public.version,
                    view: next,
                    source: Version(next),
                    tick: self.tick,
                };
                let result = props::build(
                    &self.world,
                    &self.runtimes.machines,
                    stamp,
                    &|index, cell| public.cells[usize::from(index.get())] == cell,
                    &|id| {
                        stream
                            .lids
                            .get(&id)
                            .map_or(0., |lid| f32::from(lid.value) / 65535.)
                    },
                    props::Budget::default(),
                );
                let Ok((frame, work)) = result else { continue };
                self.counters.props_cell_reads += work.cell_reads;
                self.counters.props_anchors += work.component_anchors + work.transmitter_anchors;
                if let Some(old) = state.heads.get(&page)
                    && same_visual(old, &frame)
                {
                    if force {
                        output.push(Outgoing::reliable(
                            connection,
                            ServerMessage::Props(Box::new((**old).clone())),
                        ));
                    }
                    continue;
                }
                let hints = state.hints - state.heads.get(&page).map_or(0, |h| h.hints.len())
                    + frame.hints.len();
                if hints > MAX_HINTS_PER_PEER {
                    state.queue.push_back((page, force));
                    break;
                }
                state.hints = hints;
                state.next = next;
                self.counters.props_frames += 1;
                self.counters.props_hints += frame.hints.len() as u64;
                output.push(Outgoing::reliable(
                    connection,
                    ServerMessage::Props(Box::new(frame.clone())),
                ));
                state.heads.insert(page, Arc::new(frame));
            }
        }
        output
    }
}
