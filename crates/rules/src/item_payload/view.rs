//! Read-only effective payload data for a larger owner transaction. New records
//! can be resolved before publication without copying the cold ledger.
use super::*;
pub trait Resolver {
    fn resolve(&self, id: PayloadId, item: ItemId) -> Result<&Payload, Error>;
}
impl Resolver for Ledger {
    fn resolve(&self, id: PayloadId, item: ItemId) -> Result<&Payload, Error> {
        Ledger::resolve(self, id, item)
    }
}
pub struct View<'a> {
    ledger: &'a Ledger,
    delta: &'a Delta,
}
impl Ledger {
    pub fn view<'a>(&'a self, delta: &'a Delta) -> Result<View<'a>, Error> {
        self.validate(delta)?;
        Ok(View {
            ledger: self,
            delta,
        })
    }
}
impl Resolver for View<'_> {
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
