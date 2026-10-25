//! Bounded public metadata cache. Only validated server events reach this owner;
//! missing roster bases request a reset and cannot preserve old privilege grants.
use net::{
    PlayerId, Role,
    presence::{
        ChatLine, PresenceClientEvent, PresenceServerEvent, RosterEntry, valid_chat, valid_name,
        validate_roster,
    },
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresenceError {
    Invalid,
    Capacity,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PresenceCounters {
    pub roster_updates: u64,
    pub chat_messages: u64,
    pub duplicate_chats: u64,
    pub dropped_chats: u64,
    pub missing_bases: u64,
    pub ping_replies: u64,
}
pub struct PresenceCache {
    identity: Option<PlayerId>,
    revision: u64,
    roster: BTreeMap<PlayerId, RosterEntry>,
    chats: VecDeque<ChatLine>,
    seen: BTreeSet<u64>,
    seen_order: VecDeque<u64>,
    requested_reset: bool,
    highest_chat: u64,
    counters: PresenceCounters,
}
impl Default for PresenceCache {
    fn default() -> Self {
        Self::new()
    }
}
impl PresenceCache {
    pub fn new() -> Self {
        Self {
            identity: None,
            revision: 0,
            roster: BTreeMap::new(),
            chats: VecDeque::with_capacity(200),
            seen: BTreeSet::new(),
            seen_order: VecDeque::with_capacity(256),
            requested_reset: false,
            highest_chat: 0,
            counters: PresenceCounters::default(),
        }
    }
    pub fn reset(&mut self, identity: PlayerId) {
        self.identity = Some(identity);
        self.revision = 0;
        self.roster.clear();
        self.chats.clear();
        self.seen.clear();
        self.seen_order.clear();
        self.requested_reset = false;
        self.highest_chat = 0;
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn roster(&self) -> impl Iterator<Item = &RosterEntry> {
        self.roster.values()
    }
    pub fn entry(&self, id: PlayerId) -> Option<&RosterEntry> {
        self.roster.get(&id)
    }
    pub fn own_role(&self) -> Option<Role> {
        if self.requested_reset {
            return None;
        }
        self.identity
            .and_then(|id| self.roster.get(&id).map(|entry| entry.role))
    }
    pub fn pending_chats(&self) -> usize {
        self.chats.len()
    }
    pub fn chats(&self) -> impl Iterator<Item = &ChatLine> {
        self.chats.iter()
    }
    pub fn pop_chat(&mut self) -> Option<ChatLine> {
        self.chats.pop_front()
    }
    pub fn counters(&self) -> PresenceCounters {
        self.counters
    }
    pub fn receive(
        &mut self,
        event: PresenceServerEvent,
    ) -> Result<Option<PresenceClientEvent>, PresenceError> {
        match event {
            PresenceServerEvent::Ping { nonce, .. } => {
                self.counters.ping_replies += 1;
                Ok(Some(PresenceClientEvent::Pong { nonce }))
            }
            PresenceServerEvent::Chat(chat) => {
                if chat.sequence == 0 || !valid_name(&chat.name) || !valid_chat(&chat.text) {
                    return Err(PresenceError::Invalid);
                }
                if chat.sequence <= self.highest_chat.saturating_sub(256)
                    || !self.seen.insert(chat.sequence)
                {
                    self.counters.duplicate_chats += 1;
                    return Ok(None);
                }
                self.highest_chat = self.highest_chat.max(chat.sequence);
                if self.seen_order.len() == 256
                    && let Some(old) = self.seen_order.pop_front()
                {
                    self.seen.remove(&old);
                }
                self.seen_order.push_back(chat.sequence);
                if self.chats.len() == 200 {
                    self.chats.pop_front();
                    self.counters.dropped_chats += 1;
                }
                self.chats.push_back(chat);
                self.counters.chat_messages += 1;
                Ok(None)
            }
            PresenceServerEvent::RosterDelta(delta) => {
                validate_roster(&delta).map_err(|_| PresenceError::Invalid)?;
                if delta.revision < self.revision
                    || (!delta.reset && delta.revision == self.revision)
                {
                    return Ok(None);
                }
                if !delta.reset && delta.base != self.revision {
                    self.counters.missing_bases += 1;
                    if !self.requested_reset {
                        self.requested_reset = true;
                        return Ok(Some(PresenceClientEvent::RosterRequest {
                            known_revision: self.revision,
                        }));
                    }
                    return Ok(None);
                }
                let mut staged = if delta.reset {
                    BTreeMap::new()
                } else {
                    self.roster.clone()
                };
                for id in delta.removes {
                    staged.remove(&id);
                }
                for entry in delta.upserts {
                    staged.insert(entry.identity, entry);
                }
                if staged.len() > 64 {
                    return Err(PresenceError::Capacity);
                }
                self.roster = staged;
                self.revision = delta.revision;
                self.requested_reset = false;
                self.counters.roster_updates += 1;
                Ok(None)
            }
        }
    }
}
