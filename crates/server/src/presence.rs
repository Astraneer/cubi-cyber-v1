//! Public presence projection. Join/role/chat publication is called at the
//! durable frontier; chat quota can be reserved before its WAL action is queued.
//! This helper owns no World, socket, file or acknowledgement.
use core_types::{DeterministicRng, Tick};
use net::{
    ActionId, PlayerId, Role,
    presence::{ChatLine, PresenceServerEvent, RosterDelta, RosterEntry, valid_chat, valid_name},
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
const HISTORY_LIMIT: usize = 64;
const HISTORY_BYTES: usize = 128 * 1024;
const RECENT_LIMIT: usize = 128;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresenceError {
    Capacity,
    Unknown,
    Invalid,
    RateLimited,
    Clock,
    Overflow,
}
#[derive(Clone)]
struct Rate {
    tokens: u8,
    refilled: u64,
    seen: BTreeSet<ActionId>,
    order: VecDeque<ActionId>,
    highest: u64,
}
impl Rate {
    fn new(now: u64) -> Self {
        Self {
            tokens: 4,
            refilled: now,
            seen: BTreeSet::new(),
            order: VecDeque::with_capacity(128),
            highest: 0,
        }
    }
    fn reserve(&mut self, action: ActionId, now: u64) -> Result<bool, PresenceError> {
        if action.0 <= self.highest.saturating_sub(128) || self.seen.contains(&action) {
            return Ok(false);
        }
        if now < self.refilled {
            return Err(PresenceError::Clock);
        }
        let gained = (now - self.refilled) / 500;
        self.tokens = (u64::from(self.tokens) + gained).min(4) as u8;
        self.refilled += gained * 500;
        if self.tokens == 0 {
            return Err(PresenceError::RateLimited);
        }
        self.tokens -= 1;
        self.highest = self.highest.max(action.0);
        if self.order.len() == 128
            && let Some(id) = self.order.pop_front()
        {
            self.seen.remove(&id);
        }
        self.order.push_back(action);
        self.seen.insert(action);
        Ok(true)
    }
}
struct Peer {
    entry: RosterEntry,
    rate: Rate,
    next_ping: u64,
    pending: Option<(u64, u64)>,
}
pub struct ChatReservation {
    identity: PlayerId,
    name: String,
    text: String,
    sequence: u64,
    action: ActionId,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PresenceWork {
    pub membership_changes: u64,
    pub roster_rows: u64,
    pub chats_reserved: u64,
    pub chats_published: u64,
    pub pings: u64,
}
pub struct Presence {
    peers: BTreeMap<PlayerId, Peer>,
    recent: BTreeMap<PlayerId, Rate>,
    history: VecDeque<RosterDelta>,
    history_bytes: usize,
    revision: u64,
    sequence: u64,
    nonce: DeterministicRng,
    work: PresenceWork,
}
impl ChatReservation {
    pub(crate) fn line(&self, tick: Tick) -> ChatLine {
        ChatLine {
            sequence: self.sequence,
            tick,
            identity: self.identity,
            name: self.name.clone(),
            text: self.text.clone(),
        }
    }
}
impl Presence {
    pub(crate) fn set_chat_frontier(&mut self, frontier: u64) {
        self.sequence = frontier;
    }
    pub(crate) fn identities(&self) -> impl Iterator<Item = PlayerId> + '_ {
        self.peers.keys().copied()
    }
    pub fn new(nonce_seed: u64) -> Self {
        Self {
            peers: BTreeMap::new(),
            recent: BTreeMap::new(),
            history: VecDeque::with_capacity(HISTORY_LIMIT),
            history_bytes: 0,
            revision: 0,
            sequence: 0,
            nonce: DeterministicRng::from_seed(nonce_seed),
            work: PresenceWork::default(),
        }
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn len(&self) -> usize {
        self.peers.len()
    }
    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }
    pub fn entry(&self, id: PlayerId) -> Option<&RosterEntry> {
        self.peers.get(&id).map(|p| &p.entry)
    }
    pub fn take_work(&mut self) -> PresenceWork {
        std::mem::take(&mut self.work)
    }
    pub fn join(
        &mut self,
        id: PlayerId,
        name: &str,
        role: Role,
        now: u64,
    ) -> Result<bool, PresenceError> {
        if !valid_name(name) {
            return Err(PresenceError::Invalid);
        }
        if let Some(peer) = self.peers.get(&id) {
            if peer.entry.name == name && peer.entry.role == role {
                return Ok(false);
            }
        } else if self.peers.len() == 64 {
            return Err(PresenceError::Capacity);
        }
        let revision = self
            .revision
            .checked_add(1)
            .ok_or(PresenceError::Overflow)?;
        let entry = RosterEntry {
            identity: id,
            name: name.to_owned(),
            role,
            ping_ms: self.peers.get(&id).and_then(|p| p.entry.ping_ms),
        };
        if let Some(peer) = self.peers.get_mut(&id) {
            peer.entry = entry.clone();
        } else {
            let rate = self.recent.remove(&id).unwrap_or_else(|| Rate::new(now));
            self.peers.insert(
                id,
                Peer {
                    entry: entry.clone(),
                    rate,
                    next_ping: now,
                    pending: None,
                },
            );
        }
        self.record(RosterDelta {
            base: self.revision,
            revision,
            reset: false,
            upserts: vec![entry],
            removes: Vec::new(),
        });
        self.work.membership_changes += 1;
        Ok(true)
    }
    pub fn remove(&mut self, id: PlayerId) -> Result<bool, PresenceError> {
        if !self.peers.contains_key(&id) {
            return Ok(false);
        }
        let revision = self
            .revision
            .checked_add(1)
            .ok_or(PresenceError::Overflow)?;
        let peer = self.peers.remove(&id).unwrap();
        if self.recent.len() == RECENT_LIMIT {
            let oldest = self
                .recent
                .iter()
                .min_by_key(|(_, r)| r.refilled)
                .map(|(id, _)| *id);
            if let Some(oldest) = oldest {
                self.recent.remove(&oldest);
            }
        }
        self.recent.insert(id, peer.rate);
        self.record(RosterDelta {
            base: self.revision,
            revision,
            reset: false,
            upserts: Vec::new(),
            removes: vec![id],
        });
        self.work.membership_changes += 1;
        Ok(true)
    }
    pub fn set_role(&mut self, id: PlayerId, role: Role) -> Result<bool, PresenceError> {
        let peer = self.peers.get(&id).ok_or(PresenceError::Unknown)?;
        if peer.entry.role == role {
            return Ok(false);
        }
        let revision = self
            .revision
            .checked_add(1)
            .ok_or(PresenceError::Overflow)?;
        let peer = self.peers.get_mut(&id).unwrap();
        peer.entry.role = role;
        let entry = peer.entry.clone();
        self.record(RosterDelta {
            base: self.revision,
            revision,
            reset: false,
            upserts: vec![entry],
            removes: Vec::new(),
        });
        self.work.membership_changes += 1;
        Ok(true)
    }
    pub fn roster_since(&mut self, known: u64) -> Option<PresenceServerEvent> {
        if known == self.revision {
            return None;
        }
        let mut combined = BTreeMap::new();
        let mut next = known;
        for delta in &self.history {
            if delta.base == next {
                for entry in &delta.upserts {
                    combined.insert(entry.identity, Some(entry.clone()));
                }
                for id in &delta.removes {
                    combined.insert(*id, None);
                }
                next = delta.revision;
            }
        }
        let delta = if next == self.revision {
            let mut upserts = Vec::new();
            let mut removes = Vec::new();
            for (id, value) in combined {
                if let Some(entry) = value {
                    upserts.push(entry);
                } else {
                    removes.push(id);
                }
            }
            RosterDelta {
                base: known,
                revision: self.revision,
                reset: false,
                upserts,
                removes,
            }
        } else {
            RosterDelta {
                base: 0,
                revision: self.revision,
                reset: true,
                upserts: self.peers.values().map(|p| p.entry.clone()).collect(),
                removes: Vec::new(),
            }
        };
        self.work.roster_rows += (delta.upserts.len() + delta.removes.len()) as u64;
        Some(PresenceServerEvent::RosterDelta(delta))
    }
    /// Called before WAL acceptance. Reservation consumes only ephemeral rate
    /// quota; it does not broadcast and cannot acknowledge the player's action.
    pub fn reserve_chat(
        &mut self,
        id: PlayerId,
        action: ActionId,
        text: &str,
        now: u64,
    ) -> Result<Option<ChatReservation>, PresenceError> {
        if !valid_chat(text) {
            return Err(PresenceError::Invalid);
        }
        let sequence = self
            .sequence
            .checked_add(1)
            .ok_or(PresenceError::Overflow)?;
        let peer = self.peers.get_mut(&id).ok_or(PresenceError::Unknown)?;
        if !peer.rate.reserve(action, now)? {
            return Ok(None);
        }
        self.sequence = sequence;
        self.work.chats_reserved += 1;
        Ok(Some(ChatReservation {
            identity: id,
            name: peer.entry.name.clone(),
            text: text.to_owned(),
            sequence,
            action,
        }))
    }
    /// Backpressure before the WAL accepts an action returns its reservation.
    /// A retry with that same action ID can then reserve again without loss.
    pub fn cancel_chat(&mut self, reservation: ChatReservation) {
        let rate = self
            .peers
            .get_mut(&reservation.identity)
            .map(|peer| &mut peer.rate)
            .or_else(|| self.recent.get_mut(&reservation.identity));
        if let Some(rate) = rate
            && rate.seen.remove(&reservation.action)
        {
            rate.order.retain(|id| *id != reservation.action);
            rate.tokens = rate.tokens.saturating_add(1).min(4);
        }
    }
    /// The permit is consumed exactly once, only after the corresponding action
    /// and its accepted reply are durable. A failed writer must not call this.
    pub fn publish_chat(
        &mut self,
        reservation: ChatReservation,
        tick: Tick,
    ) -> PresenceServerEvent {
        self.work.chats_published += 1;
        PresenceServerEvent::Chat(ChatLine {
            sequence: reservation.sequence,
            tick,
            identity: reservation.identity,
            name: reservation.name,
            text: reservation.text,
        })
    }
    pub fn ping(
        &mut self,
        id: PlayerId,
        now: u64,
    ) -> Result<Option<PresenceServerEvent>, PresenceError> {
        let peer = self.peers.get_mut(&id).ok_or(PresenceError::Unknown)?;
        if now < peer.next_ping {
            return Ok(None);
        }
        peer.next_ping = now.checked_add(1000).ok_or(PresenceError::Overflow)?;
        let nonce = self.nonce.next_u64();
        peer.pending = Some((nonce, now));
        self.work.pings += 1;
        Ok(Some(PresenceServerEvent::Ping {
            nonce,
            sent_ms: now,
        }))
    }
    pub fn pong(&mut self, id: PlayerId, nonce: u64, now: u64) -> Result<bool, PresenceError> {
        let peer = self.peers.get(&id).ok_or(PresenceError::Unknown)?;
        let Some((expected, sent)) = peer.pending else {
            return Ok(false);
        };
        if expected != nonce || now < sent {
            return Err(PresenceError::Invalid);
        }
        let elapsed = (now - sent).min(60000) as u16;
        let smoothed = peer.entry.ping_ms.map_or(elapsed, |old| {
            ((u32::from(old) * 3 + u32::from(elapsed)) / 4) as u16
        });
        let measured = (((u32::from(smoothed) + 2) / 5) * 5).min(60000) as u16;
        let changed = peer.entry.ping_ms != Some(measured);
        let revision = if changed {
            self.revision
                .checked_add(1)
                .ok_or(PresenceError::Overflow)?
        } else {
            self.revision
        };
        let peer = self.peers.get_mut(&id).unwrap();
        peer.pending = None;
        if !changed {
            return Ok(false);
        }
        peer.entry.ping_ms = Some(measured);
        let entry = peer.entry.clone();
        self.record(RosterDelta {
            base: self.revision,
            revision,
            reset: false,
            upserts: vec![entry],
            removes: Vec::new(),
        });
        Ok(true)
    }
    fn record(&mut self, delta: RosterDelta) {
        self.revision = delta.revision;
        self.history_bytes += footprint(&delta);
        self.history.push_back(delta);
        while self.history.len() > HISTORY_LIMIT || self.history_bytes > HISTORY_BYTES {
            if let Some(old) = self.history.pop_front() {
                self.history_bytes -= footprint(&old);
            }
        }
    }
}
fn footprint(delta: &RosterDelta) -> usize {
    64 + delta.removes.len() * 16
        + delta
            .upserts
            .iter()
            .map(|row| 64 + row.name.len())
            .sum::<usize>()
}
