//! Bounded shared history with explicit independent consumer acknowledgements.
use crate::WorldChange;
use core_types::{ChannelMask, PageId, Tick, Version};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};

#[derive(Clone, Copy, Debug)]
pub struct JournalLimits {
    pub max_records: usize,
    pub max_bytes: usize,
    pub max_age_ticks: u64,
}
impl Default for JournalLimits {
    fn default() -> Self {
        Self {
            max_records: 4096,
            max_bytes: 16 * 1024 * 1024,
            max_age_ticks: 1200,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CursorId(pub u64);
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CursorError {
    UnknownCursor,
    Lagged {
        expected: u64,
        oldest_available: u64,
    },
    InvalidAcknowledgement,
}
impl std::fmt::Display for CursorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "journal cursor error: {self:?}")
    }
}
impl std::error::Error for CursorError {}
#[derive(Clone, Debug)]
pub struct JournalBatch {
    pub first: u64,
    pub next: u64,
    pub changes: Vec<Arc<WorldChange>>,
}
struct Entry {
    sequence: u64,
    bytes: usize,
    change: Arc<WorldChange>,
}
pub(crate) struct Journal {
    limits: JournalLimits,
    entries: VecDeque<Entry>,
    bytes: usize,
    next: u64,
    next_cursor: u64,
    cursors: BTreeMap<CursorId, u64>,
}
impl Journal {
    pub fn new(limits: JournalLimits) -> Self {
        assert!(
            limits.max_records > 0 && limits.max_bytes > 0,
            "journal bounds must be positive"
        );
        Self {
            limits,
            entries: VecDeque::new(),
            bytes: 0,
            next: 0,
            next_cursor: 0,
            cursors: BTreeMap::new(),
        }
    }
    pub fn can_retain(&self, bytes: usize) -> bool {
        bytes <= self.limits.max_bytes && self.next != u64::MAX
    }
    pub fn next(&self) -> u64 {
        self.next
    }
    pub fn oldest(&self) -> u64 {
        self.entries
            .front()
            .map_or(self.next, |entry| entry.sequence)
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn bytes(&self) -> usize {
        self.bytes
    }
    fn evict(&mut self) {
        if let Some(entry) = self.entries.pop_front() {
            self.bytes -= entry.bytes;
        }
    }
    pub fn prune(&mut self, tick: Tick) {
        let oldest_tick = tick.0.saturating_sub(self.limits.max_age_ticks);
        while self
            .entries
            .front()
            .is_some_and(|entry| entry.change.tick.0 < oldest_tick)
        {
            self.evict();
        }
    }
    pub fn push(&mut self, change: WorldChange) {
        let bytes = change.retained_bytes();
        assert!(self.can_retain(bytes), "commit prevalidated journal bounds");
        while self.entries.len() >= self.limits.max_records
            || self.bytes + bytes > self.limits.max_bytes
        {
            self.evict();
        }
        self.entries.push_back(Entry {
            sequence: self.next,
            bytes,
            change: Arc::new(change),
        });
        self.next += 1;
        self.bytes += bytes;
    }
    pub fn register(&mut self, sequence: u64) -> Result<CursorId, CursorError> {
        if sequence < self.oldest() {
            return Err(CursorError::Lagged {
                expected: sequence,
                oldest_available: self.oldest(),
            });
        }
        if sequence > self.next {
            return Err(CursorError::InvalidAcknowledgement);
        }
        let id = CursorId(self.next_cursor);
        self.next_cursor = self
            .next_cursor
            .checked_add(1)
            .expect("cursor ID exhausted");
        self.cursors.insert(id, sequence);
        Ok(id)
    }
    pub fn unregister(&mut self, id: CursorId) -> bool {
        self.cursors.remove(&id).is_some()
    }
    pub fn read(&self, id: CursorId, limit: usize) -> Result<JournalBatch, CursorError> {
        let sequence = *self.cursors.get(&id).ok_or(CursorError::UnknownCursor)?;
        if sequence < self.oldest() {
            return Err(CursorError::Lagged {
                expected: sequence,
                oldest_available: self.oldest(),
            });
        }
        let changes: Vec<_> = self
            .entries
            .iter()
            .filter(|entry| entry.sequence >= sequence)
            .take(limit)
            .map(|entry| entry.change.clone())
            .collect();
        let next = sequence + changes.len() as u64;
        Ok(JournalBatch {
            first: sequence,
            next,
            changes,
        })
    }
    pub fn acknowledge(&mut self, id: CursorId, sequence: u64) -> Result<(), CursorError> {
        let previous = *self.cursors.get(&id).ok_or(CursorError::UnknownCursor)?;
        if previous < self.oldest() {
            return Err(CursorError::Lagged {
                expected: previous,
                oldest_available: self.oldest(),
            });
        }
        if sequence < previous || sequence > self.next {
            return Err(CursorError::InvalidAcknowledgement);
        }
        self.cursors.insert(id, sequence);
        Ok(())
    }
    /// A consumer explicitly installs a snapshot and then seeks to its boundary.
    pub fn recover(&mut self, id: CursorId, sequence: u64) -> Result<(), CursorError> {
        if !self.cursors.contains_key(&id) {
            return Err(CursorError::UnknownCursor);
        }
        if sequence < self.oldest() || sequence > self.next {
            return Err(CursorError::InvalidAcknowledgement);
        }
        self.cursors.insert(id, sequence);
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoalescerFull;
impl std::fmt::Display for CoalescerFull {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("derived page queue is full")
    }
}
impl std::error::Error for CoalescerFull {}
/// Derived work merges dirty versions. Gameplay events stay in JournalBatch.
pub struct DerivedChanges {
    channels: ChannelMask,
    capacity: usize,
    pages: BTreeMap<PageId, Version>,
}
impl DerivedChanges {
    pub fn new(channels: ChannelMask, capacity: usize) -> Self {
        Self {
            channels,
            capacity,
            pages: BTreeMap::new(),
        }
    }
    pub fn observe(&mut self, change: &WorldChange) -> Result<(), CoalescerFull> {
        let fresh = change
            .pages
            .iter()
            .filter(|page| {
                page.channels.intersects(self.channels) && !self.pages.contains_key(&page.page)
            })
            .count();
        if self.pages.len() + fresh > self.capacity {
            return Err(CoalescerFull);
        }
        for page in &change.pages {
            if page.channels.intersects(self.channels) {
                self.pages.insert(page.page, page.after);
            }
        }
        Ok(())
    }
    pub fn len(&self) -> usize {
        self.pages.len()
    }
    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }
    pub fn pop_first(&mut self) -> Option<(PageId, Version)> {
        self.pages.pop_first()
    }
}
