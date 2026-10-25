//! Immutable typed item data. A reference is never authority: the owner must
//! compare the actual held stack and resolve its item before consuming it.
use crate::{
    ItemId,
    blocks::{self, BeKind},
    items,
    machines::{MACHINE_BYTES, Machine, TABLET_CAPACITY},
};
use std::collections::{BTreeMap, BTreeSet};
pub const MAX_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_RECORD_BYTES: usize = MACHINE_BYTES + 3;
pub const MAX_DELTA_RECORDS: usize = 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PayloadId(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Bounds,
    Encoding,
    Collision,
    Missing,
    Item,
    Stale,
    Overflow,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    pub records: usize,
    pub bytes: usize,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            records: 4096,
            bytes: 4 * 1024 * 1024,
        }
    }
}
/// Canonical encoded data are private so records remain immutable after hashing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Payload {
    bytes: Vec<u8>,
}
impl Payload {
    pub fn machine(machine: &Machine) -> Result<Self, Error> {
        let item = items::block_item(machine_block(machine));
        let mut bytes = vec![1];
        bytes.extend_from_slice(&item.to_le_bytes());
        let mut raw = [0; MACHINE_BYTES];
        let n = machine.encode(&mut raw);
        bytes.extend_from_slice(&raw[..n]);
        let out = Self { bytes };
        out.validate()?;
        Ok(out)
    }
    pub fn charge(joules: u32) -> Result<Self, Error> {
        if joules == 0 || joules > TABLET_CAPACITY || !joules.is_multiple_of(20) {
            return Err(Error::Item);
        }
        let mut bytes = vec![2];
        bytes.extend_from_slice(&items::I_ENERGY_TABLET.to_le_bytes());
        bytes.extend_from_slice(&joules.to_le_bytes());
        Ok(Self { bytes })
    }
    pub fn item(&self) -> ItemId {
        u16::from_le_bytes([self.bytes[1], self.bytes[2]])
    }
    pub fn id(&self) -> PayloadId {
        content_id(&self.bytes)
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() < 3 || bytes.len() > MAX_RECORD_BYTES {
            return Err(Error::Bounds);
        }
        let out = Self {
            bytes: bytes.to_vec(),
        };
        out.validate()?;
        Ok(out)
    }
    pub fn resolve_machine(&self, item: ItemId) -> Result<Machine, Error> {
        if self.item() != item || self.bytes[0] != 1 {
            return Err(Error::Item);
        }
        Machine::decode(&self.bytes[3..]).map_err(|_| Error::Encoding)
    }
    pub fn joules(&self, item: ItemId) -> Result<u32, Error> {
        if self.item() != item || self.bytes[0] != 2 {
            return Err(Error::Item);
        }
        Ok(u32::from_le_bytes(
            self.bytes[3..7].try_into().map_err(|_| Error::Encoding)?,
        ))
    }
    fn validate(&self) -> Result<(), Error> {
        if self.bytes.len() < 3 || self.bytes.len() > MAX_RECORD_BYTES {
            return Err(Error::Encoding);
        }
        match self.bytes[0] {
            1 => {
                let machine = Machine::decode(&self.bytes[3..]).map_err(|_| Error::Encoding)?;
                if items::block_item(machine_block(&machine)) != self.item() {
                    return Err(Error::Item);
                }
                let mut canonical = [0; MACHINE_BYTES];
                let n = machine.encode(&mut canonical);
                if self.bytes[3..] != canonical[..n] {
                    return Err(Error::Encoding);
                }
            }
            2 => {
                if self.bytes.len() != 7 || self.item() != items::I_ENERGY_TABLET {
                    return Err(Error::Item);
                }
                let joules =
                    u32::from_le_bytes(self.bytes[3..7].try_into().map_err(|_| Error::Encoding)?);
                if joules == 0 || joules > TABLET_CAPACITY || !joules.is_multiple_of(20) {
                    return Err(Error::Item);
                }
            }
            _ => return Err(Error::Encoding),
        }
        Ok(())
    }
}
fn machine_block(machine: &Machine) -> u16 {
    // Stable registry data already define the complete allowed MekKind set.
    (0..blocks::BLOCK_COUNT as u16)
        .find(|&id| matches!(blocks::bdef(id).be,Some(BeKind::Mek(k)) if k==machine.kind))
        .expect("registered machine kind")
}
fn content_id(bytes: &[u8]) -> PayloadId {
    // Domain-separated FNV-1a with an avalanche. Collisions are always compared
    // against complete canonical bytes and fail closed; this is not a MAC.
    let mut hash = 0xcbf29ce484222325u64;
    for &b in b"cubi:item-payload:v1".iter().chain(bytes) {
        hash = (hash ^ u64::from(b)).wrapping_mul(0x100000001b3);
    }
    hash ^= hash >> 33;
    hash = hash.wrapping_mul(0xff51afd7ed558ccd);
    hash ^= hash >> 33;
    hash = hash.wrapping_mul(0xc4ceb9fe1a85ec53);
    hash ^= hash >> 33;
    PayloadId(if hash == 0 { 1 } else { hash })
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub id: PayloadId,
    pub payload: Payload,
}
impl Record {
    pub fn new(payload: Payload) -> Self {
        Self {
            id: payload.id(),
            payload,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub revision: u64,
    pub records: Vec<Record>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delta {
    revision: u64,
    next_revision: u64,
    insertions: Vec<Record>,
    removals: Vec<Record>,
}
impl Delta {
    pub fn is_empty(&self) -> bool {
        self.insertions.is_empty() && self.removals.is_empty()
    }
    pub fn insertions(&self) -> &[Record] {
        &self.insertions
    }
    pub fn removals(&self) -> &[Record] {
        &self.removals
    }
}
pub struct Ledger {
    config: Config,
    revision: u64,
    bytes: usize,
    records: BTreeMap<PayloadId, Payload>,
}
impl Ledger {
    pub fn new(config: Config) -> Result<Self, Error> {
        if config.records > 65_536 || !(16..=MAX_PAYLOAD_BYTES).contains(&config.bytes) {
            return Err(Error::Bounds);
        }
        Ok(Self {
            config,
            revision: 0,
            bytes: 16,
            records: BTreeMap::new(),
        })
    }
    pub fn len(&self) -> usize {
        self.records.len()
    }
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
    pub fn byte_len(&self) -> usize {
        self.bytes
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn get(&self, id: PayloadId) -> Option<&Payload> {
        self.records.get(&id)
    }
    pub fn resolve(&self, id: PayloadId, item: ItemId) -> Result<&Payload, Error> {
        let payload = self.get(id).ok_or(Error::Missing)?;
        if payload.item() != item {
            return Err(Error::Item);
        }
        Ok(payload)
    }
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            revision: self.revision,
            records: self
                .records
                .iter()
                .map(|(&id, payload)| Record {
                    id,
                    payload: payload.clone(),
                })
                .collect(),
        }
    }
    pub fn restore(config: Config, snapshot: &Snapshot) -> Result<Self, Error> {
        let mut ledger = Self::new(config)?;
        if snapshot.records.len() > config.records
            || snapshot.records.windows(2).any(|r| r[0].id >= r[1].id)
        {
            return Err(Error::Bounds);
        }
        for record in &snapshot.records {
            validate_record(record)?;
            ledger.bytes = ledger
                .bytes
                .checked_add(10 + record.payload.bytes.len())
                .ok_or(Error::Overflow)?;
            if ledger.bytes > config.bytes {
                return Err(Error::Bounds);
            }
            ledger.records.insert(record.id, record.payload.clone());
        }
        ledger.revision = snapshot.revision;
        Ok(ledger)
    }
    pub fn plan(&self) -> Result<Delta, Error> {
        Ok(Delta {
            revision: self.revision,
            next_revision: self.revision.checked_add(1).ok_or(Error::Overflow)?,
            insertions: Vec::new(),
            removals: Vec::new(),
        })
    }
    /// Reuse identical content idempotently. Explicit removals require proof
    /// from the server owner that no inventory/window/entity/component uses it.
    pub fn extend(
        &self,
        mut delta: Delta,
        add: impl IntoIterator<Item = Payload>,
        remove: impl IntoIterator<Item = PayloadId>,
    ) -> Result<Delta, Error> {
        self.validate(&delta)?;
        let mut additions: BTreeMap<_, _> = delta
            .insertions
            .into_iter()
            .map(|r| (r.id, r.payload))
            .collect();
        let mut removals: BTreeMap<_, _> = delta
            .removals
            .into_iter()
            .map(|r| (r.id, r.payload))
            .collect();
        for id in remove {
            if additions.remove(&id).is_none() {
                let payload = self.records.get(&id).ok_or(Error::Missing)?;
                removals.insert(id, payload.clone());
            }
            if additions.len() + removals.len() > MAX_DELTA_RECORDS {
                return Err(Error::Bounds);
            }
        }
        for payload in add {
            payload.validate()?;
            let id = payload.id();
            if let Some(existing) = additions.get(&id).or_else(|| self.records.get(&id)) {
                if existing != &payload {
                    return Err(Error::Collision);
                }
                removals.remove(&id);
            } else {
                additions.insert(id, payload);
            }
            if additions.len() + removals.len() > MAX_DELTA_RECORDS {
                return Err(Error::Bounds);
            }
        }
        delta.insertions = additions
            .into_iter()
            .map(|(id, payload)| Record { id, payload })
            .collect();
        delta.removals = removals
            .into_iter()
            .map(|(id, payload)| Record { id, payload })
            .collect();
        self.validate(&delta)?;
        Ok(delta)
    }
    pub fn validate(&self, delta: &Delta) -> Result<(), Error> {
        if delta.revision != self.revision
            || delta.next_revision != self.revision.checked_add(1).ok_or(Error::Overflow)?
        {
            return Err(Error::Stale);
        }
        if delta.insertions.len() + delta.removals.len() > MAX_DELTA_RECORDS {
            return Err(Error::Bounds);
        }
        let mut ids = BTreeSet::new();
        let mut bytes = self.bytes;
        let mut count = self.records.len();
        for record in &delta.removals {
            validate_record(record)?;
            if !ids.insert(record.id) || self.get(record.id) != Some(&record.payload) {
                return Err(Error::Stale);
            }
            bytes = bytes
                .checked_sub(10 + record.payload.bytes.len())
                .ok_or(Error::Overflow)?;
            count -= 1;
        }
        for record in &delta.insertions {
            validate_record(record)?;
            if !ids.insert(record.id) || self.get(record.id).is_some() {
                return Err(Error::Collision);
            }
            bytes = bytes
                .checked_add(10 + record.payload.bytes.len())
                .ok_or(Error::Overflow)?;
            count += 1;
        }
        if count > self.config.records || bytes > self.config.bytes {
            return Err(Error::Bounds);
        }
        if delta.insertions.windows(2).any(|r| r[0].id >= r[1].id)
            || delta.removals.windows(2).any(|r| r[0].id >= r[1].id)
        {
            return Err(Error::Encoding);
        }
        Ok(())
    }
    pub fn acknowledge(&mut self, delta: Delta) -> Result<(), Error> {
        self.validate(&delta)?;
        for record in delta.removals {
            self.records.remove(&record.id);
            self.bytes -= 10 + record.payload.bytes.len();
        }
        for record in delta.insertions {
            self.bytes += 10 + record.payload.bytes.len();
            self.records.insert(record.id, record.payload);
        }
        self.revision = delta.next_revision;
        Ok(())
    }
}
fn validate_record(record: &Record) -> Result<(), Error> {
    record.payload.validate()?;
    if record.id.0 == 0 || record.id != record.payload.id() {
        return Err(Error::Collision);
    }
    Ok(())
}
#[path = "item_payload/codec.rs"]
mod codec;
#[path = "item_payload/encoded.rs"]
mod encoded;
#[path = "item_payload/refs.rs"]
pub mod refs;
pub use encoded::Encoded;
