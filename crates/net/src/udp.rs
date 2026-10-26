//! Authenticated bounded fragmentation and reliable channels over native UDP.
//! Connection establishment belongs to the loopback transport. Production remote
//! transports must provide encrypted/authenticated establishment (e.g. QUIC).

use crate::{MAX_FRAME_BYTES, SecretToken};
use std::collections::{BTreeMap, VecDeque};

pub const MAX_DATAGRAM: usize = 1200;
const PAYLOAD: usize = 1100;
const HEADER: usize = 35;
const TAG: usize = 32;
const MAX_FRAGMENTS: usize = MAX_FRAME_BYTES.div_ceil(PAYLOAD);

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub maximum_messages: usize,
    pub maximum_bytes: usize,
    pub maximum_age_ms: u64,
    pub resend_after_ms: u64,
    pub maximum_retries: u8,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            maximum_messages: 16,
            maximum_bytes: 2 * MAX_FRAME_BYTES,
            maximum_age_ms: 5_000,
            resend_after_ms: 200,
            maximum_retries: 12,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UdpError {
    Invalid,
    Authentication,
    WrongSession,
    Capacity,
    Exhausted,
    Expired,
}
#[derive(Clone, Copy, Debug)]
pub enum Side {
    Client,
    Server,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct Counters {
    pub sent_bytes: u64,
    pub received_bytes: u64,
    pub retransmitted_bytes: u64,
    pub rejected: u64,
    pub peak_reassembly_bytes: usize,
    pub discarded_unreliable_assemblies: u64,
}
pub struct Receive {
    pub frames: Vec<Vec<u8>>,
    pub replies: Vec<Vec<u8>>,
}

struct Pending {
    frame: Vec<u8>,
    created: u64,
    last_sent: u64,
    retries: u8,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExpiredMessage {
    pub message_id: u64,
    pub frame_bytes: usize,
    pub age_ms: u64,
    pub retries: u8,
}
struct Assembly {
    chunks: Vec<Option<Vec<u8>>>,
    bytes: usize,
    created: u64,
    reliable: bool,
}
pub struct Channel {
    session: u64,
    send_key: SecretToken,
    receive_key: SecretToken,
    limits: Limits,
    next_message: u64,
    packet_sequence: u64,
    pending: BTreeMap<u64, Pending>,
    assembling: BTreeMap<u64, Assembly>,
    completed: VecDeque<u64>,
    pending_bytes: usize,
    reassembly_bytes: usize,
    counters: Counters,
    last_expired: Option<ExpiredMessage>,
}

impl Channel {
    pub fn new(session: u64, key: SecretToken, side: Side, limits: Limits) -> Self {
        assert!(
            session != 0
                && limits.maximum_messages > 0
                && limits.maximum_bytes >= MAX_FRAME_BYTES
                && limits.resend_after_ms > 0
        );
        let client = SecretToken::new(blake3::derive_key(
            "cubi UDP client-to-server v1",
            key.as_bytes(),
        ));
        let server = SecretToken::new(blake3::derive_key(
            "cubi UDP server-to-client v1",
            key.as_bytes(),
        ));
        let (send_key, receive_key) = match side {
            Side::Client => (client, server),
            Side::Server => (server, client),
        };
        Self {
            session,
            send_key,
            receive_key,
            limits,
            next_message: 1,
            packet_sequence: 1,
            pending: BTreeMap::new(),
            assembling: BTreeMap::new(),
            completed: VecDeque::new(),
            pending_bytes: 0,
            reassembly_bytes: 0,
            counters: Counters::default(),
            last_expired: None,
        }
    }
    pub fn counters(&self) -> Counters {
        self.counters
    }
    pub fn pending_messages(&self) -> usize {
        self.pending.len()
    }
    pub fn last_expired(&self) -> Option<ExpiredMessage> {
        self.last_expired
    }

    fn packet(
        &mut self,
        id: u64,
        fragment: u16,
        total: u16,
        flags: u8,
        payload: &[u8],
    ) -> Result<Vec<u8>, UdpError> {
        let sequence = self.packet_sequence;
        self.packet_sequence = self
            .packet_sequence
            .checked_add(1)
            .ok_or(UdpError::Exhausted)?;
        let mut out = Vec::with_capacity(HEADER + payload.len() + TAG);
        out.extend(b"CUDP");
        out.extend(self.session.to_le_bytes());
        out.extend(sequence.to_le_bytes());
        out.extend(id.to_le_bytes());
        out.extend(fragment.to_le_bytes());
        out.extend(total.to_le_bytes());
        out.push(flags);
        out.extend((payload.len() as u16).to_le_bytes());
        out.extend(payload);
        let mac = blake3::keyed_hash(self.send_key.as_bytes(), &out);
        out.extend(mac.as_bytes());
        self.counters.sent_bytes += out.len() as u64;
        Ok(out)
    }
    fn fragments(
        &mut self,
        id: u64,
        frame: &[u8],
        reliable: bool,
    ) -> Result<Vec<Vec<u8>>, UdpError> {
        let count = frame.len().div_ceil(PAYLOAD);
        frame
            .chunks(PAYLOAD)
            .enumerate()
            .map(|(index, payload)| {
                self.packet(
                    id,
                    index as u16,
                    count as u16,
                    if reliable { 1 } else { 0 },
                    payload,
                )
            })
            .collect()
    }
    pub fn send(
        &mut self,
        frame: Vec<u8>,
        reliable: bool,
        now_ms: u64,
    ) -> Result<Vec<Vec<u8>>, UdpError> {
        if frame.is_empty() || frame.len() > MAX_FRAME_BYTES {
            return Err(UdpError::Invalid);
        }
        if reliable
            && (self.pending.len() >= self.limits.maximum_messages
                || frame.len() > self.limits.maximum_bytes.saturating_sub(self.pending_bytes))
        {
            return Err(UdpError::Capacity);
        }
        let id = self.next_message;
        self.next_message = self
            .next_message
            .checked_add(1)
            .ok_or(UdpError::Exhausted)?;
        let packets = self.fragments(id, &frame, reliable)?;
        if reliable {
            self.pending_bytes += frame.len();
            self.pending.insert(
                id,
                Pending {
                    frame,
                    created: now_ms,
                    last_sent: now_ms,
                    retries: 0,
                },
            );
        }
        Ok(packets)
    }
    pub fn receive(&mut self, packet: &[u8], now_ms: u64) -> Result<Receive, UdpError> {
        self.receive_inner(packet, now_ms, true)
    }
    /// Service authenticated transport ACKs and already-delivered duplicates
    /// while the owner cannot retain a new application frame. New data remains
    /// unacknowledged and is retried under the existing channel limits.
    pub fn receive_control(&mut self, packet: &[u8], now_ms: u64) -> Result<Receive, UdpError> {
        self.receive_inner(packet, now_ms, false)
    }
    fn receive_inner(
        &mut self,
        packet: &[u8],
        now_ms: u64,
        admit: bool,
    ) -> Result<Receive, UdpError> {
        self.counters.received_bytes += packet.len() as u64;
        if packet.len() < HEADER + TAG || packet.len() > MAX_DATAGRAM || &packet[..4] != b"CUDP" {
            return Err(UdpError::Invalid);
        }
        let session = u64::from_le_bytes(packet[4..12].try_into().expect("header checked"));
        if session != self.session {
            return Err(UdpError::WrongSession);
        }
        let end = packet.len() - TAG;
        let expected = blake3::keyed_hash(self.receive_key.as_bytes(), &packet[..end]);
        if expected
            .as_bytes()
            .iter()
            .zip(&packet[end..])
            .fold(0u8, |difference, (a, b)| difference | (a ^ b))
            != 0
        {
            self.counters.rejected += 1;
            return Err(UdpError::Authentication);
        }
        let id = u64::from_le_bytes(packet[20..28].try_into().expect("header checked"));
        let index = usize::from(u16::from_le_bytes(
            packet[28..30].try_into().expect("header checked"),
        ));
        let total = usize::from(u16::from_le_bytes(
            packet[30..32].try_into().expect("header checked"),
        ));
        let flags = packet[32];
        let length = usize::from(u16::from_le_bytes(
            packet[33..35].try_into().expect("header checked"),
        ));
        if flags > 2 || length != end - HEADER || length > PAYLOAD || id == 0 {
            return Err(UdpError::Invalid);
        }
        if flags == 2 {
            if total != 0 || index != 0 || length != 0 {
                return Err(UdpError::Invalid);
            }
            if let Some(pending) = self.pending.remove(&id) {
                self.pending_bytes -= pending.frame.len();
            }
            return Ok(Receive {
                frames: Vec::new(),
                replies: Vec::new(),
            });
        }
        if total == 0 || total > MAX_FRAGMENTS || index >= total {
            return Err(UdpError::Invalid);
        }
        if self.completed.contains(&id) {
            let replies = if flags == 1 {
                vec![self.packet(id, 0, 0, 2, &[])?]
            } else {
                Vec::new()
            };
            return Ok(Receive {
                frames: Vec::new(),
                replies,
            });
        }
        if !admit {
            return Err(UdpError::Capacity);
        }
        self.expire_assemblies(now_ms);
        if !self.assembling.contains_key(&id) {
            if self.assembling.len() >= self.limits.maximum_messages && !self.discard_unreliable(id)
            {
                return Err(UdpError::Capacity);
            }
            self.assembling.insert(
                id,
                Assembly {
                    chunks: vec![None; total],
                    bytes: 0,
                    created: now_ms,
                    reliable: flags == 1,
                },
            );
        }
        let assembly = self.assembling.get(&id).expect("inserted");
        if assembly.chunks.len() != total || assembly.reliable != (flags == 1) {
            return Err(UdpError::Invalid);
        }
        if assembly.chunks[index].is_none() {
            if assembly.bytes + length > MAX_FRAME_BYTES {
                return Err(UdpError::Capacity);
            }
            while length
                > self
                    .limits
                    .maximum_bytes
                    .saturating_sub(self.reassembly_bytes)
            {
                if !self.discard_unreliable(id) {
                    return Err(UdpError::Capacity);
                }
            }
            let assembly = self
                .assembling
                .get_mut(&id)
                .expect("retained incoming assembly");
            assembly.chunks[index] = Some(packet[HEADER..end].to_vec());
            assembly.bytes += length;
            self.reassembly_bytes += length;
            self.counters.peak_reassembly_bytes = self
                .counters
                .peak_reassembly_bytes
                .max(self.reassembly_bytes);
        }
        let assembly = self.assembling.get(&id).expect("retained assembly");
        if assembly.chunks.iter().any(Option::is_none) {
            return Ok(Receive {
                frames: Vec::new(),
                replies: Vec::new(),
            });
        }
        let assembly = self.assembling.remove(&id).expect("complete");
        self.reassembly_bytes -= assembly.bytes;
        let mut frame = Vec::with_capacity(assembly.bytes);
        for chunk in assembly.chunks {
            frame.extend(chunk.expect("complete"));
        }
        self.completed.push_back(id);
        while self.completed.len() > 256 {
            self.completed.pop_front();
        }
        let replies = if assembly.reliable {
            vec![self.packet(id, 0, 0, 2, &[])?]
        } else {
            Vec::new()
        };
        Ok(Receive {
            frames: vec![frame],
            replies,
        })
    }
    fn discard_unreliable(&mut self, incoming: u64) -> bool {
        // Fragment loss leaves old snapshots incomplete for the full assembly
        // lifetime. They must not occupy every slot and expire a reliable action
        // whose resend deadline is shorter than that lifetime. Reliable partials
        // are retained; all limits and authentication checks remain unchanged.
        let candidate = self
            .assembling
            .iter()
            .filter(|(id, a)| **id != incoming && !a.reliable)
            .min_by_key(|(id, a)| (a.created, **id))
            .map(|(&id, _)| id);
        if let Some(id) = candidate {
            let old = self.assembling.remove(&id).expect("selected assembly");
            self.reassembly_bytes -= old.bytes;
            self.counters.discarded_unreliable_assemblies += 1;
            true
        } else {
            false
        }
    }
    fn expire_assemblies(&mut self, now_ms: u64) {
        self.assembling.retain(|_, assembly| {
            if now_ms.saturating_sub(assembly.created) > self.limits.maximum_age_ms {
                self.reassembly_bytes -= assembly.bytes;
                false
            } else {
                true
            }
        });
    }
    pub fn tick(&mut self, now_ms: u64) -> Result<Vec<Vec<u8>>, UdpError> {
        self.expire_assemblies(now_ms);
        let due: Vec<_> = self
            .pending
            .iter()
            .filter_map(|(&id, pending)| {
                (now_ms.saturating_sub(pending.last_sent) >= self.limits.resend_after_ms)
                    .then_some(id)
            })
            .collect();
        let mut packets = Vec::new();
        for id in due {
            let mut pending = self.pending.remove(&id).expect("pending");
            if pending.retries >= self.limits.maximum_retries {
                self.pending_bytes -= pending.frame.len();
                self.last_expired = Some(ExpiredMessage {
                    message_id: id,
                    frame_bytes: pending.frame.len(),
                    age_ms: now_ms.saturating_sub(pending.created),
                    retries: pending.retries,
                });
                return Err(UdpError::Expired);
            }
            let retry = self.fragments(id, &pending.frame, true)?;
            self.counters.retransmitted_bytes +=
                retry.iter().map(|packet| packet.len() as u64).sum::<u64>();
            packets.extend(retry);
            pending.retries += 1;
            pending.last_sent = now_ms;
            self.pending.insert(id, pending);
        }
        Ok(packets)
    }
}
