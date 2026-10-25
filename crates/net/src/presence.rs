//! Bounded public chat, roster and server-measured ping. This payload contains
//! no inventory, private credentials, ore state or generation metadata.
use crate::{PlayerId, Role};
use core_types::Tick;
use std::collections::BTreeSet;
pub const MAX_PRESENCE_BYTES: usize = 16 * 1024;
pub const MAX_ROSTER: usize = 64;
pub const MAX_CHAT_BYTES: usize = 256;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RosterEntry {
    pub identity: PlayerId,
    pub name: String,
    pub role: Role,
    pub ping_ms: Option<u16>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RosterDelta {
    pub base: u64,
    pub revision: u64,
    pub reset: bool,
    pub upserts: Vec<RosterEntry>,
    pub removes: Vec<PlayerId>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatLine {
    pub sequence: u64,
    pub tick: Tick,
    pub identity: PlayerId,
    pub name: String,
    pub text: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PresenceServerEvent {
    Chat(ChatLine),
    RosterDelta(RosterDelta),
    Ping { nonce: u64, sent_ms: u64 },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresenceClientEvent {
    Pong { nonce: u64 },
    RosterRequest { known_revision: u64 },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresenceCodecError {
    Format,
    Limit,
    Utf8,
    Value,
    Trailing,
}
pub fn valid_name(name: &str) -> bool {
    !name.trim().is_empty() && name.len() <= 64 && !name.chars().any(char::is_control)
}
pub fn valid_chat(text: &str) -> bool {
    !text.trim().is_empty() && text.len() <= MAX_CHAT_BYTES && !text.chars().any(char::is_control)
}
pub fn validate_roster(delta: &RosterDelta) -> Result<(), PresenceCodecError> {
    if delta.upserts.len() > MAX_ROSTER
        || delta.removes.len() > MAX_ROSTER
        || (!delta.reset && delta.revision <= delta.base)
        || (delta.reset && (delta.base != 0 || !delta.removes.is_empty()))
    {
        return Err(PresenceCodecError::Value);
    }
    let mut ids = BTreeSet::new();
    for entry in &delta.upserts {
        if !valid_name(&entry.name)
            || entry.ping_ms.is_some_and(|ping| ping > 60000)
            || !ids.insert(entry.identity)
        {
            return Err(PresenceCodecError::Value);
        }
    }
    for id in &delta.removes {
        if !ids.insert(*id) {
            return Err(PresenceCodecError::Value);
        }
    }
    Ok(())
}
pub fn encode_server(event: &PresenceServerEvent) -> Result<Vec<u8>, PresenceCodecError> {
    let mut bytes = b"PRS1".to_vec();
    match event {
        PresenceServerEvent::Chat(line) => {
            if line.sequence == 0 || !valid_name(&line.name) || !valid_chat(&line.text) {
                return Err(PresenceCodecError::Value);
            }
            bytes.push(1);
            u64_write(&mut bytes, line.sequence);
            u64_write(&mut bytes, line.tick.0);
            bytes.extend_from_slice(&line.identity.0);
            short_write(&mut bytes, &line.name);
            bytes.extend_from_slice(&(line.text.len() as u16).to_le_bytes());
            bytes.extend_from_slice(line.text.as_bytes());
        }
        PresenceServerEvent::RosterDelta(delta) => {
            validate_roster(delta)?;
            bytes.push(2);
            u64_write(&mut bytes, delta.base);
            u64_write(&mut bytes, delta.revision);
            bytes.extend_from_slice(&[
                u8::from(delta.reset),
                delta.upserts.len() as u8,
                delta.removes.len() as u8,
            ]);
            for entry in &delta.upserts {
                bytes.extend_from_slice(&entry.identity.0);
                short_write(&mut bytes, &entry.name);
                bytes.push(entry.role as u8);
                bytes.extend_from_slice(&entry.ping_ms.unwrap_or(u16::MAX).to_le_bytes());
            }
            for id in &delta.removes {
                bytes.extend_from_slice(&id.0);
            }
        }
        PresenceServerEvent::Ping { nonce, sent_ms } => {
            bytes.push(3);
            u64_write(&mut bytes, *nonce);
            u64_write(&mut bytes, *sent_ms);
        }
    }
    if bytes.len() > MAX_PRESENCE_BYTES {
        return Err(PresenceCodecError::Limit);
    }
    Ok(bytes)
}
pub fn decode_server(bytes: &[u8]) -> Result<PresenceServerEvent, PresenceCodecError> {
    let mut reader = Reader::new(bytes, b"PRS1")?;
    let event = match reader.byte()? {
        1 => {
            let sequence = reader.u64()?;
            let tick = Tick(reader.u64()?);
            let identity = PlayerId(reader.take(16)?.try_into().unwrap());
            let name = reader.short(64)?;
            let length = reader.u16()? as usize;
            if length > MAX_CHAT_BYTES {
                return Err(PresenceCodecError::Limit);
            }
            let text = reader.text(length)?;
            if sequence == 0 || !valid_name(&name) || !valid_chat(&text) {
                return Err(PresenceCodecError::Value);
            }
            PresenceServerEvent::Chat(ChatLine {
                sequence,
                tick,
                identity,
                name,
                text,
            })
        }
        2 => {
            let base = reader.u64()?;
            let revision = reader.u64()?;
            let reset = match reader.byte()? {
                0 => false,
                1 => true,
                _ => return Err(PresenceCodecError::Value),
            };
            let count = usize::from(reader.byte()?);
            let removed = usize::from(reader.byte()?);
            if count > MAX_ROSTER || removed > MAX_ROSTER {
                return Err(PresenceCodecError::Limit);
            }
            let mut upserts = Vec::with_capacity(count);
            for _ in 0..count {
                let identity = PlayerId(reader.take(16)?.try_into().unwrap());
                let name = reader.short(64)?;
                let role = match reader.byte()? {
                    0 => Role::Player,
                    1 => Role::Admin,
                    2 => Role::Owner,
                    _ => return Err(PresenceCodecError::Value),
                };
                let ping = reader.u16()?;
                upserts.push(RosterEntry {
                    identity,
                    name,
                    role,
                    ping_ms: (ping != u16::MAX).then_some(ping),
                });
            }
            let mut removes = Vec::with_capacity(removed);
            for _ in 0..removed {
                removes.push(PlayerId(reader.take(16)?.try_into().unwrap()));
            }
            let delta = RosterDelta {
                base,
                revision,
                reset,
                upserts,
                removes,
            };
            validate_roster(&delta)?;
            PresenceServerEvent::RosterDelta(delta)
        }
        3 => PresenceServerEvent::Ping {
            nonce: reader.u64()?,
            sent_ms: reader.u64()?,
        },
        _ => return Err(PresenceCodecError::Value),
    };
    reader.finish()?;
    Ok(event)
}
pub fn encode_client(event: PresenceClientEvent) -> Vec<u8> {
    let mut bytes = b"PRC1".to_vec();
    match event {
        PresenceClientEvent::Pong { nonce } => {
            bytes.push(1);
            u64_write(&mut bytes, nonce);
        }
        PresenceClientEvent::RosterRequest { known_revision } => {
            bytes.push(2);
            u64_write(&mut bytes, known_revision);
        }
    }
    bytes
}
pub fn decode_client(bytes: &[u8]) -> Result<PresenceClientEvent, PresenceCodecError> {
    let mut reader = Reader::new(bytes, b"PRC1")?;
    let event = match reader.byte()? {
        1 => PresenceClientEvent::Pong {
            nonce: reader.u64()?,
        },
        2 => PresenceClientEvent::RosterRequest {
            known_revision: reader.u64()?,
        },
        _ => return Err(PresenceCodecError::Value),
    };
    reader.finish()?;
    Ok(event)
}
fn u64_write(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_le_bytes());
}
fn short_write(bytes: &mut Vec<u8>, value: &str) {
    bytes.push(value.len() as u8);
    bytes.extend_from_slice(value.as_bytes());
}
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8], magic: &[u8]) -> Result<Self, PresenceCodecError> {
        if bytes.len() > MAX_PRESENCE_BYTES {
            return Err(PresenceCodecError::Limit);
        }
        if bytes.get(..4) != Some(magic) {
            return Err(PresenceCodecError::Format);
        }
        Ok(Self { bytes, at: 4 })
    }
    fn take(&mut self, length: usize) -> Result<&'a [u8], PresenceCodecError> {
        let value = self
            .bytes
            .get(self.at..self.at + length)
            .ok_or(PresenceCodecError::Format)?;
        self.at += length;
        Ok(value)
    }
    fn byte(&mut self) -> Result<u8, PresenceCodecError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, PresenceCodecError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, PresenceCodecError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn text(&mut self, length: usize) -> Result<String, PresenceCodecError> {
        std::str::from_utf8(self.take(length)?)
            .map(str::to_owned)
            .map_err(|_| PresenceCodecError::Utf8)
    }
    fn short(&mut self, max: usize) -> Result<String, PresenceCodecError> {
        let length = usize::from(self.byte()?);
        if length > max {
            return Err(PresenceCodecError::Limit);
        }
        self.text(length)
    }
    fn finish(&self) -> Result<(), PresenceCodecError> {
        if self.at == self.bytes.len() {
            Ok(())
        } else {
            Err(PresenceCodecError::Trailing)
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn id(n: u8) -> PlayerId {
        PlayerId([n; 16])
    }
    #[test]
    fn public_events_roundtrip_and_every_truncation_is_rejected() {
        let events = [
            PresenceServerEvent::Chat(ChatLine {
                sequence: 1,
                tick: Tick(42),
                identity: id(1),
                name: "Олег".into(),
                text: "Привет".into(),
            }),
            PresenceServerEvent::RosterDelta(RosterDelta {
                base: 1,
                revision: 2,
                reset: false,
                upserts: vec![RosterEntry {
                    identity: id(1),
                    name: "Игрок".into(),
                    role: Role::Admin,
                    ping_ms: Some(42),
                }],
                removes: vec![id(2)],
            }),
            PresenceServerEvent::Ping {
                nonce: 23,
                sent_ms: 1042,
            },
        ];
        for event in events {
            let bytes = encode_server(&event).unwrap();
            assert_eq!(decode_server(&bytes).unwrap(), event);
            for end in 0..bytes.len() {
                assert!(decode_server(&bytes[..end]).is_err());
            }
            let mut extra = bytes;
            extra.push(0);
            assert_eq!(decode_server(&extra), Err(PresenceCodecError::Trailing));
        }
    }
    #[test]
    fn names_payload_limits_and_duplicate_roster_fail_closed() {
        let mut delta = RosterDelta {
            base: 0,
            revision: 1,
            reset: true,
            upserts: vec![RosterEntry {
                identity: id(1),
                name: "A".into(),
                role: Role::Player,
                ping_ms: None,
            }],
            removes: vec![],
        };
        delta.upserts.push(delta.upserts[0].clone());
        assert!(encode_server(&PresenceServerEvent::RosterDelta(delta)).is_err());
        let chat = PresenceServerEvent::Chat(ChatLine {
            sequence: 1,
            tick: Tick(0),
            identity: id(1),
            name: "A".into(),
            text: "x".repeat(257),
        });
        assert!(encode_server(&chat).is_err());
        assert!(decode_server(&vec![0; MAX_PRESENCE_BYTES + 1]).is_err());
        assert!(!valid_chat("hello\n"));
    }
    #[test]
    fn client_ping_and_missing_base_recovery_have_frozen_encoding() {
        assert_eq!(
            encode_client(PresenceClientEvent::Pong { nonce: 1 }),
            b"PRC1\x01\x01\0\0\0\0\0\0\0"
        );
        for event in [
            PresenceClientEvent::Pong { nonce: u64::MAX },
            PresenceClientEvent::RosterRequest { known_revision: 42 },
        ] {
            assert_eq!(decode_client(&encode_client(event)).unwrap(), event);
        }
    }
}
