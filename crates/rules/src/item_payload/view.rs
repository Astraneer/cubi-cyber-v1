//! Read-only effective payload data for a larger owner transaction. New records
//! can be resolved before publication without copying the cold ledger.
use super::*;
/// Free persistent-record storage in the effective owner transaction. Existing
/// identical records remain reusable when every one of these budgets is zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capacity {
    pub records: usize,
    pub bytes: usize,
    pub changes: usize,
}
impl Capacity {
    pub fn allows(self, records: usize, bytes: usize) -> bool {
        records <= self.records && records <= self.changes && bytes <= self.bytes
    }
}
pub trait Resolver {
    fn resolve(&self, id: PayloadId, item: ItemId) -> Result<&Payload, Error>;
    /// Source-neutral fixtures can omit storage admission. Canonical Ledger
    /// and pending View always provide their exact remaining quota.
    fn remaining_capacity(&self) -> Option<Capacity> {
        None
    }
}
impl Resolver for Ledger {
    fn resolve(&self, id: PayloadId, item: ItemId) -> Result<&Payload, Error> {
        Ledger::resolve(self, id, item)
    }
    fn remaining_capacity(&self) -> Option<Capacity> {
        Some(Capacity {
            records: self.config.records - self.records.len(),
            bytes: self.config.bytes - self.bytes,
            changes: MAX_DELTA_RECORDS,
        })
    }
}
pub struct View<'a> {
    ledger: &'a Ledger,
    delta: &'a Delta,
    capacity: Capacity,
}
impl Ledger {
    pub fn view<'a>(&'a self, delta: &'a Delta) -> Result<View<'a>, Error> {
        self.validate(delta)?;
        let bytes_removed: usize = delta
            .removals
            .iter()
            .map(|r| 10 + r.payload.bytes().len())
            .sum();
        let bytes_added: usize = delta
            .insertions
            .iter()
            .map(|r| 10 + r.payload.bytes().len())
            .sum();
        let used_bytes = self.bytes - bytes_removed + bytes_added;
        let used_records = self.records.len() - delta.removals.len() + delta.insertions.len();
        Ok(View {
            ledger: self,
            delta,
            capacity: Capacity {
                records: self.config.records - used_records,
                bytes: self.config.bytes - used_bytes,
                changes: MAX_DELTA_RECORDS - delta.removals.len() - delta.insertions.len(),
            },
        })
    }
}
impl Resolver for View<'_> {
    fn remaining_capacity(&self) -> Option<Capacity> {
        Some(self.capacity)
    }
    fn resolve(&self, id: PayloadId, item: ItemId) -> Result<&Payload, Error> {
        let payload = if let Ok(i) = self.delta.insertions.binary_search_by_key(&id, |r| r.id) {
            &self.delta.insertions[i].payload
        } else if self
            .delta
            .removals
            .binary_search_by_key(&id, |r| r.id)
            .is_ok()
        {
            return Err(Error::Missing);
        } else {
            return self.ledger.resolve(id, item);
        };
        if payload.item() != item {
            return Err(Error::Item);
        }
        Ok(payload)
    }
}
