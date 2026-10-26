//! Charge crosses container boundaries as typed immutable item data. The owner
//! interns `to_item`'s optional record in the same WAL batch as the moved stack.
use super::{Error, Payload, Resolver};
use crate::{items::I_ENERGY_TABLET, machines::Slot, stack::ItemStack};
impl Slot {
    pub fn to_item(self) -> Result<(ItemStack, Option<Payload>), Error> {
        if self.charge == 0 {
            return Ok((self.stack, None));
        }
        if self.stack.item() != I_ENERGY_TABLET
            || self.stack.count() != 1
            || self.stack.payload().is_some()
        {
            return Err(Error::Item);
        }
        let payload = Payload::charge(self.charge)?;
        let stack = self
            .stack
            .with_payload(payload.id())
            .map_err(|_| Error::Item)?;
        Ok((stack, Some(payload)))
    }
    pub fn from_item<R: Resolver + ?Sized>(stack: ItemStack, ledger: &R) -> Result<Self, Error> {
        let Some(id) = stack.payload() else {
            return Ok(Self::new(stack));
        };
        let payload = ledger.resolve(id, stack.item())?;
        if stack.item() == I_ENERGY_TABLET {
            let charge = payload.joules(stack.item())?;
            Ok(Self {
                stack: stack.without_payload(),
                charge,
            })
        } else if stack.item() == crate::items::I_CONFIGURATOR {
            payload.configurator_mode()?;
            Ok(Self::new(stack))
        } else {
            payload.resolve_machine(stack.item())?;
            Ok(Self::new(stack))
        }
    }
}
