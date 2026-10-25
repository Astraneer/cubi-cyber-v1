//! Bounded durable chat history. Accepted ActionId and public line are recovered
//! together; retry after a crash cannot silently acknowledge a missing message.
use net::{
    ActionId, PlayerId,
    presence::{ChatLine, PresenceServerEvent},
};
use std::collections::{BTreeSet, VecDeque};
const HISTORY: usize = 200;
const MAX_DELTA: usize = 128;
const MAX_LINE: usize = 512;
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Entry {
    pub action: ActionId,
    pub line: ChatLine,
}
#[derive(Default)]
pub(super) struct Ledger {
    frontier: u64,
    rows: VecDeque<Entry>,
}
impl Ledger {
    pub fn frontier(&self) -> u64 {
        self.frontier
    }
    pub fn rows(&self) -> impl Iterator<Item = &Entry> {
        self.rows.iter()
    }
    pub fn get(&self, identity: PlayerId, action: ActionId) -> Option<&ChatLine> {
        self.rows
            .iter()
            .find(|e| e.line.identity == identity && e.action == action)
            .map(|e| &e.line)
    }
    pub fn preflight(&self, entries: &[Entry]) -> Result<(), &'static str> {
        let mut frontier = self.frontier;
        let mut actions = BTreeSet::new();
        for entry in entries {
            if entry.action.0 == 0
                || entry.line.sequence <= frontier
                || self.get(entry.line.identity, entry.action).is_some()
                || !actions.insert((entry.line.identity, entry.action))
            {
                return Err("chat delta order/duplicate");
            }
            encode_line(entry)?;
            frontier = entry.line.sequence;
        }
        Ok(())
    }
    pub fn apply(&mut self, entries: &[Entry]) -> Result<(), &'static str> {
        let mut frontier = self.frontier;
        let mut actions = BTreeSet::new();
        for entry in entries {
            if entry.action.0 == 0
                || entry.line.sequence <= frontier
                || self.get(entry.line.identity, entry.action).is_some()
                || !actions.insert((entry.line.identity, entry.action))
            {
                return Err("chat delta order/duplicate");
            }
            encode_line(entry)?;
            frontier = entry.line.sequence;
        }
        for entry in entries {
            self.rows.push_back(entry.clone());
            if self.rows.len() > HISTORY {
                self.rows.pop_front();
            }
        }
        self.frontier = frontier;
        Ok(())
    }
    pub fn snapshot(&self) -> Result<Vec<u8>, &'static str> {
        let mut out = b"CHS1".to_vec();
        out.extend(self.frontier.to_le_bytes());
        write_entries(
            &mut out,
            &self.rows.iter().cloned().collect::<Vec<_>>(),
            HISTORY,
        )?;
        Ok(out)
    }
    pub fn restore(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() < 14 || &bytes[..4] != b"CHS1" {
            return Err("chat snapshot format");
        }
        let frontier = u64::from_le_bytes(bytes[4..12].try_into().map_err(|_| "chat frontier")?);
        let entries = read_entries(&bytes[12..], HISTORY)?;
        let mut ledger = Self::default();
        ledger.apply(&entries)?;
        if ledger.frontier != frontier {
            return Err("chat snapshot frontier");
        }
        Ok(ledger)
    }
}
pub(super) fn encode_delta(entries: &[Entry]) -> Result<Vec<u8>, &'static str> {
    let mut out = b"CHD1".to_vec();
    write_entries(&mut out, entries, MAX_DELTA)?;
    Ok(out)
}
pub(super) fn decode_delta(bytes: &[u8]) -> Result<Vec<Entry>, &'static str> {
    if bytes.len() < 6 || &bytes[..4] != b"CHD1" {
        return Err("chat delta format");
    }
    read_entries(&bytes[4..], MAX_DELTA)
}
fn encode_line(entry: &Entry) -> Result<Vec<u8>, &'static str> {
    if entry.action.0 == 0 {
        return Err("zero chat action");
    }
    let line = net::presence::encode_server(&PresenceServerEvent::Chat(entry.line.clone()))
        .map_err(|_| "invalid chat line")?;
    if line.len() > MAX_LINE {
        return Err("chat line bound");
    }
    Ok(line)
}
fn write_entries(out: &mut Vec<u8>, entries: &[Entry], maximum: usize) -> Result<(), &'static str> {
    if entries.len() > maximum {
        return Err("chat entry bound");
    }
    out.extend((entries.len() as u16).to_le_bytes());
    for entry in entries {
        let bytes = encode_line(entry)?;
        out.extend(entry.action.0.to_le_bytes());
        out.extend((bytes.len() as u16).to_le_bytes());
        out.extend(bytes);
    }
    Ok(())
}
fn read_entries(bytes: &[u8], maximum: usize) -> Result<Vec<Entry>, &'static str> {
    if bytes.len() < 2 {
        return Err("truncated chat count");
    }
    let count = u16::from_le_bytes([bytes[0], bytes[1]]) as usize;
    if count > maximum || count > (bytes.len() - 2) / 10 {
        return Err("chat entry bound");
    }
    let mut offset = 2;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let header = bytes
            .get(offset..offset + 10)
            .ok_or("truncated chat header")?;
        offset += 10;
        let action = ActionId(u64::from_le_bytes(
            header[..8].try_into().map_err(|_| "chat action")?,
        ));
        let len = u16::from_le_bytes([header[8], header[9]]) as usize;
        if len > MAX_LINE {
            return Err("chat line bound");
        }
        let data = bytes
            .get(offset..offset + len)
            .ok_or("truncated chat line")?;
        offset += len;
        let PresenceServerEvent::Chat(line) =
            net::presence::decode_server(data).map_err(|_| "chat line codec")?
        else {
            return Err("chat payload kind");
        };
        let entry = Entry { action, line };
        encode_line(&entry)?;
        out.push(entry);
    }
    if offset != bytes.len() {
        return Err("chat trailing bytes");
    }
    Ok(out)
}

impl super::Server {
    pub(super) fn queue_chat(
        &mut self,
        peer: &super::Peer,
        id: net::ActionId,
        text: &str,
        batch: &mut save::Batch,
    ) -> Result<(), net::RejectReason> {
        let reservation = self
            .presence
            .reserve_chat(peer.identity, id, text, self.now_ms)
            .map_err(|e| match e {
                super::presence::PresenceError::RateLimited => net::RejectReason::RateLimited,
                _ => net::RejectReason::Protocol,
            })?
            .ok_or(net::RejectReason::AlreadyFinal)?;
        let mut entries = match batch.runtimes.iter().find(|r| r.kind == save::RUNTIME_CHAT) {
            Some(r) => {
                decode_delta(&r.payload).map_err(|_| net::RejectReason::StorageUnavailable)?
            }
            None => Vec::new(),
        };
        entries.push(Entry {
            action: id,
            line: reservation.line(self.tick),
        });
        if self.runtimes.chat.preflight(&entries).is_err() {
            self.presence.cancel_chat(reservation);
            return Err(net::RejectReason::StorageUnavailable);
        }
        let payload = encode_delta(&entries).map_err(|_| net::RejectReason::StorageUnavailable)?;
        if let Some(record) = batch
            .runtimes
            .iter_mut()
            .find(|r| r.kind == save::RUNTIME_CHAT)
        {
            record.payload = payload
        } else {
            batch.runtimes.push(save::RuntimeRecord {
                kind: save::RUNTIME_CHAT,
                payload,
            })
        }
        self.staged_chats.push(reservation);
        Ok(())
    }
    pub(super) fn synchronize_presence(&mut self) -> Result<(), super::ServerError> {
        let removed: Vec<_> = self
            .presence
            .identities()
            .filter(|id| !self.peers.values().any(|p| p.ready && p.identity == *id))
            .collect();
        for id in removed {
            self.presence
                .remove(id)
                .map_err(|_| super::ServerError::Invalid("presence removal"))?;
        }
        for peer in self.peers.values().filter(|p| p.ready) {
            let identity = &self.identities[&peer.identity];
            self.presence
                .join(peer.identity, &identity.name, identity.role, self.now_ms)
                .map_err(|_| super::ServerError::Invalid("presence membership"))?;
        }
        Ok(())
    }
    pub(super) fn publish_presence(&mut self) -> Vec<super::Outgoing> {
        let mut output = Vec::new();
        for (connection, peer) in self.peers.iter_mut().filter(|(_, p)| p.ready) {
            if peer.roster_revision != Some(self.presence.revision()) {
                if let Some(event) = self
                    .presence
                    .roster_since(peer.roster_revision.unwrap_or(0))
                {
                    output.push(super::Outgoing::reliable(
                        *connection,
                        net::ServerMessage::Presence(event),
                    ));
                }
                peer.roster_revision = Some(self.presence.revision());
            }
            if let Ok(Some(event)) = self.presence.ping(peer.identity, self.now_ms) {
                output.push(super::Outgoing::reliable(
                    *connection,
                    net::ServerMessage::Presence(event),
                ));
            }
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_types::Tick;
    fn entry(n: u64) -> Entry {
        Entry {
            action: ActionId(n),
            line: ChatLine {
                sequence: n,
                tick: Tick(n),
                identity: PlayerId([1; 16]),
                name: "Игрок".into(),
                text: format!("сообщение {n}"),
            },
        }
    }
    #[test]
    fn history_bound_and_recovery_keep_stable_action_to_line() {
        let mut state = Ledger::default();
        for n in 1..=250 {
            state.apply(&[entry(n)]).unwrap();
        }
        assert_eq!(state.rows().count(), 200);
        assert!(state.get(PlayerId([1; 16]), ActionId(50)).is_none());
        let bytes = state.snapshot().unwrap();
        let restored = Ledger::restore(&bytes).unwrap();
        assert_eq!(restored.frontier(), 250);
        assert_eq!(
            restored.get(PlayerId([1; 16]), ActionId(250)),
            Some(&entry(250).line)
        );
        let delta = encode_delta(&[entry(251)]).unwrap();
        let mut restored = restored;
        restored.apply(&decode_delta(&delta).unwrap()).unwrap();
        assert_eq!(restored.frontier(), 251);
    }
    #[test]
    fn malformed_and_duplicate_deltas_leave_history_unchanged() {
        let mut state = Ledger::default();
        state.apply(&[entry(1)]).unwrap();
        let before = state.snapshot().unwrap();
        assert!(state.apply(&[entry(2), entry(1)]).is_err());
        assert_eq!(state.snapshot().unwrap(), before);
        let bytes = encode_delta(&[entry(2)]).unwrap();
        for n in 0..bytes.len() {
            assert!(decode_delta(&bytes[..n]).is_err());
        }
        let mut trailing = bytes;
        trailing.push(0);
        assert!(decode_delta(&trailing).is_err());
        assert_eq!(state.snapshot().unwrap(), before);
    }
}
