//! Fixed-capacity incremental spatial hash. Updates/query callbacks allocate
//! nothing, bucket traversal is deterministic, and every query has explicit caps.
use super::EntityError;
const NONE: u32 = u32::MAX;
#[derive(Clone, Copy, Debug)]
struct Bucket {
    key: Option<[i32; 3]>,
    used: bool,
    head: u32,
}
#[derive(Clone, Copy, Debug)]
struct Node {
    bucket: u32,
    previous: u32,
    next: u32,
}
impl Node {
    const EMPTY: Self = Self {
        bucket: NONE,
        previous: NONE,
        next: NONE,
    };
}
#[derive(Clone, Copy, Debug)]
pub struct QueryBudget {
    pub buckets: u64,
    pub candidates: u64,
    pub probes: u64,
}
impl Default for QueryBudget {
    fn default() -> Self {
        Self {
            buckets: 4096,
            candidates: 4096,
            probes: 16_384,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QueryWork {
    pub buckets: u64,
    pub candidates: u64,
    pub probes: u64,
    pub overflow: bool,
}
#[derive(Debug)]
pub struct SpatialHash {
    buckets: Vec<Bucket>,
    nodes: Vec<Node>,
    edge: f64,
    len: usize,
}
impl SpatialHash {
    pub fn new(maximum_entities: usize, edge: f64) -> Result<Self, EntityError> {
        if maximum_entities > 1_000_000 || !edge.is_finite() || edge <= 0.0 || edge > 1024.0 {
            return Err(EntityError::Capacity);
        }
        let capacity = (maximum_entities.max(1) * 2).next_power_of_two();
        Ok(Self {
            buckets: vec![
                Bucket {
                    key: None,
                    used: false,
                    head: NONE
                };
                capacity
            ],
            nodes: vec![Node::EMPTY; maximum_entities],
            edge,
            len: 0,
        })
    }
    pub const fn len(&self) -> usize {
        self.len
    }
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
    fn key(&self, position: [f64; 3]) -> Result<[i32; 3], EntityError> {
        let mut key = [0; 3];
        for (out, value) in key.iter_mut().zip(position) {
            let bucket = (value / self.edge).floor();
            if !bucket.is_finite() || bucket < f64::from(i32::MIN) || bucket > f64::from(i32::MAX) {
                return Err(EntityError::InvalidPosition);
            }
            *out = bucket as i32;
        }
        Ok(key)
    }
    fn hash(key: [i32; 3]) -> u64 {
        let mut value = 0x9e3779b97f4a7c15u64;
        for part in key {
            value ^= (part as u32 as u64)
                .wrapping_add(0x9e3779b97f4a7c15)
                .wrapping_add(value << 6)
                .wrapping_add(value >> 2);
            value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        }
        value ^ (value >> 27)
    }
    fn slot(&self, key: [i32; 3]) -> Option<(usize, bool)> {
        let start = Self::hash(key) as usize & (self.buckets.len() - 1);
        let mut tombstone = None;
        for offset in 0..self.buckets.len() {
            let index = (start + offset) & (self.buckets.len() - 1);
            let bucket = &self.buckets[index];
            if bucket.key == Some(key) {
                return Some((index, true));
            }
            if bucket.key.is_none() {
                if !bucket.used {
                    return Some((tombstone.unwrap_or(index), false));
                }
                if tombstone.is_none() {
                    tombstone = Some(index);
                }
            }
        }
        tombstone.map(|index| (index, false))
    }
    pub fn update(&mut self, index: usize, position: [f64; 3]) -> Result<bool, EntityError> {
        let old = *self.nodes.get(index).ok_or(EntityError::Capacity)?;
        let key = self.key(position)?;
        if old.bucket != NONE && self.buckets[old.bucket as usize].key == Some(key) {
            return Ok(false);
        }
        self.remove(index)?;
        let (slot, found) = self.slot(key).ok_or(EntityError::Capacity)?;
        let head = if found { self.buckets[slot].head } else { NONE };
        self.buckets[slot] = Bucket {
            key: Some(key),
            used: true,
            head: index as u32,
        };
        self.nodes[index] = Node {
            bucket: slot as u32,
            previous: NONE,
            next: head,
        };
        if head != NONE {
            self.nodes[head as usize].previous = index as u32;
        }
        self.len += 1;
        Ok(true)
    }
    pub fn remove(&mut self, index: usize) -> Result<bool, EntityError> {
        let node = *self.nodes.get(index).ok_or(EntityError::UnknownId)?;
        if node.bucket == NONE {
            return Ok(false);
        }
        if node.previous == NONE {
            self.buckets[node.bucket as usize].head = node.next;
        } else {
            self.nodes[node.previous as usize].next = node.next;
        }
        if node.next != NONE {
            self.nodes[node.next as usize].previous = node.previous;
        }
        self.nodes[index] = Node::EMPTY;
        if self.buckets[node.bucket as usize].head == NONE {
            self.close_gap(node.bucket as usize);
        }
        self.len -= 1;
        Ok(true)
    }
    // Backward-shift deletion avoids a table full of tombstones after travel.
    // Every lookup still reaches a never-used slot at the ordinary load factor.
    fn close_gap(&mut self, mut hole: usize) {
        let mask = self.buckets.len() - 1;
        self.buckets[hole] = Bucket {
            key: None,
            used: false,
            head: NONE,
        };
        let mut cursor = (hole + 1) & mask;
        while let Some(key) = self.buckets[cursor].key {
            let home = Self::hash(key) as usize & mask;
            if hole.wrapping_sub(home) & mask < cursor.wrapping_sub(home) & mask {
                self.buckets[hole] = self.buckets[cursor];
                let mut node = self.buckets[hole].head;
                while node != NONE {
                    self.nodes[node as usize].bucket = hole as u32;
                    node = self.nodes[node as usize].next;
                }
                self.buckets[cursor] = Bucket {
                    key: None,
                    used: false,
                    head: NONE,
                };
                hole = cursor;
            }
            cursor = (cursor + 1) & mask;
        }
    }
    /// Repair links after a dense store moves its last element into a removed row.
    pub fn move_index(&mut self, from: usize, to: usize) -> Result<(), EntityError> {
        if from == to {
            return Ok(());
        }
        let node = *self.nodes.get(from).ok_or(EntityError::UnknownId)?;
        if self.nodes.get(to).ok_or(EntityError::UnknownId)?.bucket != NONE {
            return Err(EntityError::DuplicateId);
        }
        if node.bucket != NONE {
            if node.previous == NONE {
                self.buckets[node.bucket as usize].head = to as u32;
            } else {
                self.nodes[node.previous as usize].next = to as u32;
            }
            if node.next != NONE {
                self.nodes[node.next as usize].previous = to as u32;
            }
        }
        self.nodes[to] = node;
        self.nodes[from] = Node::EMPTY;
        Ok(())
    }
    pub fn query(
        &self,
        low: [f64; 3],
        high: [f64; 3],
        budget: QueryBudget,
        mut visit: impl FnMut(usize),
    ) -> Result<QueryWork, EntityError> {
        if (0..3).any(|axis| low[axis] > high[axis]) {
            return Err(EntityError::InvalidPosition);
        }
        let low = self.key(low)?;
        let high = self.key(high)?;
        let volume = (0..3).try_fold(1u64, |volume, axis| {
            volume.checked_mul((i64::from(high[axis]) - i64::from(low[axis]) + 1) as u64)
        });
        if volume.is_none_or(|volume| volume > budget.buckets) {
            return Ok(QueryWork {
                overflow: true,
                ..QueryWork::default()
            });
        }
        let mut work = QueryWork::default();
        for z in low[2]..=high[2] {
            for y in low[1]..=high[1] {
                for x in low[0]..=high[0] {
                    work.buckets += 1;
                    let key = [x, y, z];
                    let start = Self::hash(key) as usize & (self.buckets.len() - 1);
                    let mut found = None;
                    for distance in 0..self.buckets.len() {
                        if work.probes == budget.probes {
                            work.overflow = true;
                            return Ok(work);
                        }
                        work.probes += 1;
                        let slot = (start + distance) & (self.buckets.len() - 1);
                        let bucket = self.buckets[slot];
                        if bucket.key == Some(key) {
                            found = Some(slot);
                            break;
                        }
                        if bucket.key.is_none() && !bucket.used {
                            break;
                        }
                    }
                    let Some(slot) = found else {
                        continue;
                    };
                    let mut next = self.buckets[slot].head;
                    while next != NONE {
                        if work.candidates == budget.candidates {
                            work.overflow = true;
                            return Ok(work);
                        }
                        work.candidates += 1;
                        visit(next as usize);
                        next = self.nodes[next as usize].next;
                    }
                }
            }
        }
        Ok(work)
    }
}
