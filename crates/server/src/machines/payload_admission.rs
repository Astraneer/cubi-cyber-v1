//! Admission belongs to the individual transfer. A full immutable ledger must
//! keep the source stack while unrelated recipes finish their logical tick.
use super::{view::Stage, *};
use rules::item_payload::{Payload, PayloadId};
pub(super) enum Admission {
    Reused,
    Inserted(PayloadId),
    Full,
}
impl Stage<'_> {
    pub(super) fn admit_payload(
        &mut self,
        payload: Payload,
        persistent: bool,
    ) -> Result<Admission, MachineError> {
        let id = payload.id();
        let existing = self
            .local
            .payloads
            .get(&id)
            .or_else(|| self.parent.payloads.get(&id));
        if let Some(existing) = existing {
            return if existing == &payload {
                Ok(Admission::Reused)
            } else {
                Err(MachineError::Encoding)
            };
        }
        if let Some(resolver) = self.owner.payloads {
            match resolver.resolve(id, payload.item()) {
                Ok(existing) => {
                    return if existing == &payload {
                        Ok(Admission::Reused)
                    } else {
                        Err(MachineError::Encoding)
                    };
                }
                Err(rules::item_payload::Error::Missing) => {}
                Err(_) => return Err(MachineError::Encoding),
            }
            if persistent && let Some(capacity) = resolver.remaining_capacity() {
                let records = self.parent.payloads.len() + self.local.payloads.len() + 1;
                let bytes = self.parent.payload_bytes
                    + self.local.payload_bytes
                    + 10
                    + payload.bytes().len();
                if !capacity.allows(records, bytes) {
                    self.work.payload_deferred += 1;
                    return Ok(Admission::Full);
                }
            }
        }
        self.local.payload_bytes += 10 + payload.bytes().len();
        self.local.payloads.insert(id, payload);
        Ok(Admission::Inserted(id))
    }
    pub(super) fn release_payload(&mut self, id: PayloadId) {
        if let Some(payload) = self.local.payloads.remove(&id) {
            self.local.payload_bytes -= 10 + payload.bytes().len();
        }
    }
}
