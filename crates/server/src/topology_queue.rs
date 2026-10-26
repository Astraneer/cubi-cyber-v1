//! Durable dirty connection positions. Plans touch affected rows only; Unknown
//! retries preserve original age and creation order across checkpoint/recovery.
use core_types::{BlockPos, SpaceId, Tick};
use std::collections::{BTreeMap, BTreeSet};
pub const MAX_PENDING: usize = 131_072;
pub const MAX_PATCHES: usize = 4096;
pub const MAX_BYTES: usize = 8 * 1024 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Bounds,
    Stale,
    Malformed,
    Truncated,
    Trailing,
    Overflow,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub position: BlockPos,
    pub due: Tick,
    pub first_due: Tick,
    pub order: u64,
}
impl Entry {
    fn valid(self) -> bool {
        self.order > 0 && self.first_due <= self.due && self.due.0 < u64::MAX
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Patch {
    pub position: BlockPos,
    pub before: Option<Entry>,
    pub after: Option<Entry>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub revision: u64,
    pub next_order: u64,
    pub entries: Vec<Entry>,
}
impl Default for Snapshot {
    fn default() -> Self {
        Self {
            revision: 0,
            next_order: 1,
            entries: Vec::new(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delta {
    pub revision: u64,
    pub next_revision: u64,
    pub order_before: u64,
    pub order_after: u64,
    pub patches: Vec<Patch>,
}
impl Delta {
    pub fn is_empty(&self) -> bool {
        self.patches.is_empty() && self.order_before == self.order_after
    }
}
pub struct Queue {
    maximum: usize,
    revision: u64,
    next_order: u64,
    entries: BTreeMap<BlockPos, Entry>,
    due: BTreeMap<(Tick, u64), BlockPos>,
}
impl Queue {
    pub fn new(maximum: usize) -> Result<Self, Error> {
        if maximum == 0 || maximum > MAX_PENDING {
            return Err(Error::Bounds);
        }
        Ok(Self {
            maximum,
            revision: 0,
            next_order: 1,
            entries: BTreeMap::new(),
            due: BTreeMap::new(),
        })
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn get(&self, position: BlockPos) -> Option<Entry> {
        self.entries.get(&position).copied()
    }
    pub fn due(&self, now: Tick, limit: usize) -> impl Iterator<Item = Entry> + '_ {
        self.due
            .range(..=(now, u64::MAX))
            .take(limit.min(MAX_PATCHES))
            .map(|(_, p)| self.entries[p])
    }
    pub fn plan(&self) -> Result<Delta, Error> {
        Ok(Delta {
            revision: self.revision,
            next_revision: self.revision.checked_add(1).ok_or(Error::Overflow)?,
            order_before: self.next_order,
            order_after: self.next_order,
            patches: Vec::new(),
        })
    }
    pub fn extend_registration(
        &self,
        mut delta: Delta,
        positions: impl IntoIterator<Item = BlockPos>,
        now: Tick,
    ) -> Result<Delta, Error> {
        self.validate(&delta)?;
        if now.0 == u64::MAX {
            return Err(Error::Bounds);
        }
        let mut patches: BTreeMap<_, _> =
            delta.patches.into_iter().map(|p| (p.position, p)).collect();
        for (count, position) in positions.into_iter().enumerate() {
            if count >= MAX_PATCHES {
                return Err(Error::Bounds);
            }
            let before = self.get(position);
            let effective = patches.get(&position).map(|p| p.after).unwrap_or(before);
            let after = if let Some(old) = effective {
                Entry {
                    due: old.due.min(now),
                    first_due: old.first_due.min(now),
                    ..old
                }
            } else {
                let order = delta.order_after;
                delta.order_after = order.checked_add(1).ok_or(Error::Overflow)?;
                Entry {
                    position,
                    due: now,
                    first_due: now,
                    order,
                }
            };
            if before != Some(after) {
                patches.insert(
                    position,
                    Patch {
                        position,
                        before,
                        after: Some(after),
                    },
                );
            }
            if patches.len() > MAX_PATCHES {
                return Err(Error::Bounds);
            }
        }
        delta.patches = patches.into_values().collect();
        self.validate(&delta)?;
        Ok(delta)
    }
    pub fn extend_completion(
        &self,
        delta: Delta,
        positions: impl IntoIterator<Item = BlockPos>,
    ) -> Result<Delta, Error> {
        self.change_due(delta, positions, None)
    }
    pub fn extend_retry(
        &self,
        delta: Delta,
        positions: impl IntoIterator<Item = BlockPos>,
        due: Tick,
    ) -> Result<Delta, Error> {
        self.change_due(delta, positions, Some(due))
    }
    fn change_due(
        &self,
        mut delta: Delta,
        positions: impl IntoIterator<Item = BlockPos>,
        due: Option<Tick>,
    ) -> Result<Delta, Error> {
        self.validate(&delta)?;
        let mut patches: BTreeMap<_, _> =
            delta.patches.into_iter().map(|p| (p.position, p)).collect();
        for (count, position) in positions.into_iter().enumerate() {
            if count >= MAX_PATCHES {
                return Err(Error::Bounds);
            }
            let before = self.get(position);
            let old = patches
                .get(&position)
                .map(|p| p.after)
                .unwrap_or(before)
                .ok_or(Error::Stale)?;
            let after = due.map(|due| Entry { due, ..old });
            if after.is_some_and(|e| !e.valid()) {
                return Err(Error::Bounds);
            }
            if before == after {
                patches.remove(&position);
            } else {
                patches.insert(
                    position,
                    Patch {
                        position,
                        before,
                        after,
                    },
                );
            }
            if patches.len() > MAX_PATCHES {
                return Err(Error::Bounds);
            }
        }
        delta.patches = patches.into_values().collect();
        self.validate(&delta)?;
        Ok(delta)
    }
    pub fn validate(&self, delta: &Delta) -> Result<(), Error> {
        if delta.revision != self.revision
            || delta.next_revision != self.revision.checked_add(1).ok_or(Error::Overflow)?
            || delta.order_before != self.next_order
        {
            return Err(Error::Stale);
        }
        if delta.order_after < delta.order_before
            || delta.order_after == 0
            || delta.patches.len() > MAX_PATCHES
        {
            return Err(Error::Bounds);
        }
        let mut count = self.len();
        let mut last = None;
        let mut orders = BTreeSet::new();
        for patch in &delta.patches {
            if last.is_some_and(|p| p >= patch.position)
                || patch.before != self.get(patch.position)
                || patch.before == patch.after
            {
                return Err(Error::Stale);
            }
            last = Some(patch.position);
            if patch.before.is_none() && patch.after.is_none() {
                return Err(Error::Malformed);
            }
            for e in [patch.before, patch.after].into_iter().flatten() {
                if !e.valid() || e.position != patch.position {
                    return Err(Error::Malformed);
                }
            }
            if let Some(after) = patch.after {
                if let Some(before) = patch.before {
                    if after.order != before.order || after.first_due != before.first_due {
                        return Err(Error::Malformed);
                    }
                } else if after.order < delta.order_before
                    || after.order >= delta.order_after
                    || !orders.insert(after.order)
                {
                    return Err(Error::Malformed);
                }
            }
            count = count
                .checked_sub(usize::from(patch.before.is_some()))
                .and_then(|n| n.checked_add(usize::from(patch.after.is_some())))
                .ok_or(Error::Bounds)?;
        }
        if count > self.maximum {
            return Err(Error::Bounds);
        }
        Ok(())
    }
    pub fn acknowledge(&mut self, delta: Delta) -> Result<(), Error> {
        self.validate(&delta)?;
        for patch in delta.patches {
            if let Some(before) = patch.before {
                self.entries.remove(&patch.position);
                self.due.remove(&(before.due, before.order));
            }
            if let Some(after) = patch.after {
                self.entries.insert(after.position, after);
                self.due.insert((after.due, after.order), after.position);
            }
        }
        self.revision = delta.next_revision;
        self.next_order = delta.order_after;
        Ok(())
    }
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            revision: self.revision,
            next_order: self.next_order,
            entries: self.entries.values().copied().collect(),
        }
    }
    pub fn restore(maximum: usize, snapshot: &Snapshot) -> Result<Self, Error> {
        let mut queue = Self::new(maximum)?;
        if snapshot.entries.len() > maximum
            || snapshot.next_order == 0
            || snapshot.revision == u64::MAX
        {
            return Err(Error::Bounds);
        }
        let mut previous = None;
        for entry in &snapshot.entries {
            if !entry.valid()
                || entry.order >= snapshot.next_order
                || previous.is_some_and(|p| p >= entry.position)
                || queue
                    .due
                    .insert((entry.due, entry.order), entry.position)
                    .is_some()
            {
                return Err(Error::Malformed);
            }
            previous = Some(entry.position);
            queue.entries.insert(entry.position, *entry);
        }
        let mut orders = BTreeSet::new();
        if snapshot.entries.iter().any(|e| !orders.insert(e.order)) {
            return Err(Error::Malformed);
        }
        queue.revision = snapshot.revision;
        queue.next_order = snapshot.next_order;
        Ok(queue)
    }
}
fn position(out: &mut Vec<u8>, p: BlockPos) {
    out.extend_from_slice(&p.space.0.to_le_bytes());
    for x in [p.x, p.y, p.z] {
        out.extend_from_slice(&x.to_le_bytes());
    }
}
fn values(out: &mut Vec<u8>, e: Entry) {
    for x in [e.due.0, e.first_due.0, e.order] {
        out.extend_from_slice(&x.to_le_bytes());
    }
}
fn u64w(out: &mut Vec<u8>, x: u64) {
    out.extend_from_slice(&x.to_le_bytes());
}
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        let end = self.at.checked_add(N).ok_or(Error::Bounds)?;
        let b = self.bytes.get(self.at..end).ok_or(Error::Truncated)?;
        self.at = end;
        Ok(b.try_into().expect("fixed slice"))
    }
    fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(self.take()?))
    }
    fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(self.take()?))
    }
    fn position(&mut self) -> Result<BlockPos, Error> {
        Ok(BlockPos {
            space: SpaceId(self.u32()?),
            x: i32::from_le_bytes(self.take()?),
            y: i32::from_le_bytes(self.take()?),
            z: i32::from_le_bytes(self.take()?),
        })
    }
    fn entry(&mut self, p: BlockPos) -> Result<Entry, Error> {
        let e = Entry {
            position: p,
            due: Tick(self.u64()?),
            first_due: Tick(self.u64()?),
            order: self.u64()?,
        };
        if !e.valid() {
            return Err(Error::Malformed);
        }
        Ok(e)
    }
    fn optional(&mut self, p: BlockPos) -> Result<Option<Entry>, Error> {
        match self.take::<1>()?[0] {
            0 => Ok(None),
            1 => Ok(Some(self.entry(p)?)),
            _ => Err(Error::Malformed),
        }
    }
}
impl Snapshot {
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        Queue::restore(MAX_PENDING, self)?;
        let size = 24 + self.entries.len() * 40;
        if size > MAX_BYTES {
            return Err(Error::Bounds);
        }
        let mut out = Vec::with_capacity(size);
        out.extend_from_slice(b"TPQ1");
        u64w(&mut out, self.revision);
        u64w(&mut out, self.next_order);
        out.extend_from_slice(&(self.entries.len() as u32).to_le_bytes());
        for e in &self.entries {
            position(&mut out, e.position);
            values(&mut out, *e);
        }
        Ok(out)
    }
    pub fn decode(bytes: &[u8], maximum: usize) -> Result<Self, Error> {
        if bytes.len() > MAX_BYTES {
            return Err(Error::Bounds);
        }
        let mut r = Reader { bytes, at: 0 };
        if r.take::<4>()? != *b"TPQ1" {
            return Err(Error::Malformed);
        }
        let revision = r.u64()?;
        let next_order = r.u64()?;
        let count = r.u32()? as usize;
        if count > maximum
            || count > MAX_PENDING
            || count.checked_mul(40) != Some(bytes.len().saturating_sub(r.at))
        {
            return Err(Error::Bounds);
        }
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            let p = r.position()?;
            entries.push(r.entry(p)?);
        }
        if r.at != bytes.len() {
            return Err(Error::Trailing);
        }
        let out = Self {
            revision,
            next_order,
            entries,
        };
        Queue::restore(maximum, &out)?;
        Ok(out)
    }
}
impl Delta {
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        self.check_shape()?;
        let mut out = Vec::new();
        out.extend_from_slice(b"TPD1");
        for x in [
            self.revision,
            self.next_revision,
            self.order_before,
            self.order_after,
        ] {
            u64w(&mut out, x);
        }
        out.extend_from_slice(&(self.patches.len() as u32).to_le_bytes());
        for patch in &self.patches {
            position(&mut out, patch.position);
            for value in [patch.before, patch.after] {
                out.push(u8::from(value.is_some()));
                if let Some(e) = value {
                    values(&mut out, e);
                }
            }
        }
        Ok(out)
    }
    fn check_shape(&self) -> Result<(), Error> {
        if self.revision.checked_add(1) != Some(self.next_revision)
            || self.order_before == 0
            || self.order_after < self.order_before
            || self.patches.len() > MAX_PATCHES
        {
            return Err(Error::Malformed);
        }
        let mut last = None;
        let mut orders = BTreeSet::new();
        for p in &self.patches {
            if last.is_some_and(|v| v >= p.position) || p.before == p.after {
                return Err(Error::Malformed);
            }
            last = Some(p.position);
            if let Some(after) = p.after {
                if let Some(before) = p.before {
                    if after.order != before.order || after.first_due != before.first_due {
                        return Err(Error::Malformed);
                    }
                } else if after.order < self.order_before
                    || after.order >= self.order_after
                    || !orders.insert(after.order)
                {
                    return Err(Error::Malformed);
                }
            }
            for e in [p.before, p.after].into_iter().flatten() {
                if e.position != p.position || !e.valid() {
                    return Err(Error::Malformed);
                }
            }
        }
        Ok(())
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() > MAX_BYTES {
            return Err(Error::Bounds);
        }
        let mut r = Reader { bytes, at: 0 };
        if r.take::<4>()? != *b"TPD1" {
            return Err(Error::Malformed);
        }
        let revision = r.u64()?;
        let next_revision = r.u64()?;
        let order_before = r.u64()?;
        let order_after = r.u64()?;
        let count = r.u32()? as usize;
        if count > MAX_PATCHES
            || count
                .checked_mul(18)
                .is_none_or(|n| n > bytes.len().saturating_sub(r.at))
        {
            return Err(Error::Bounds);
        }
        let mut patches = Vec::with_capacity(count);
        for _ in 0..count {
            let position = r.position()?;
            patches.push(Patch {
                position,
                before: r.optional(position)?,
                after: r.optional(position)?,
            });
        }
        if r.at != bytes.len() {
            return Err(Error::Trailing);
        }
        let out = Self {
            revision,
            next_revision,
            order_before,
            order_after,
            patches,
        };
        out.check_shape()?;
        Ok(out)
    }
}
