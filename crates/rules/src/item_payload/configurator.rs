//! The configurator is not a durable tool. Its five donor modes are typed item
//! data, never damage. Owners publish the returned record and changed inventory
//! in one durable batch after checking the actual held stack and its revision.
use super::{Error, Payload, Resolver};
use crate::{items::I_CONFIGURATOR, stack::ItemStack};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum ConfiguratorMode {
    #[default]
    Items = 0,
    Empty = 1,
    Rotate = 2,
    Wrench = 3,
    Gas = 4,
}
impl ConfiguratorMode {
    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Items),
            1 => Some(Self::Empty),
            2 => Some(Self::Rotate),
            3 => Some(Self::Wrench),
            4 => Some(Self::Gas),
            _ => None,
        }
    }
    pub const fn cycle(self, back: bool) -> Self {
        let value = (self as u8 + if back { 4 } else { 1 }) % 5;
        match Self::from_u8(value) {
            Some(mode) => mode,
            None => unreachable!(),
        }
    }
}
impl Payload {
    pub fn configurator(mode: ConfiguratorMode) -> Self {
        let item = I_CONFIGURATOR.to_le_bytes();
        Self {
            bytes: vec![3, item[0], item[1], mode as u8],
        }
    }
    pub fn configurator_mode(&self) -> Result<ConfiguratorMode, Error> {
        if self.bytes.len() != 4 || self.bytes[0] != 3 || self.item() != I_CONFIGURATOR {
            return Err(Error::Item);
        }
        ConfiguratorMode::from_u8(self.bytes[3]).ok_or(Error::Encoding)
    }
}
pub fn resolve_configurator<R: Resolver + ?Sized>(
    stack: ItemStack,
    resolver: &R,
) -> Result<ConfiguratorMode, Error> {
    if stack.item() != I_CONFIGURATOR || stack.count() != 1 || stack.damage() != 0 {
        return Err(Error::Item);
    }
    match stack.payload() {
        Some(id) => resolver.resolve(id, I_CONFIGURATOR)?.configurator_mode(),
        None => Ok(ConfiguratorMode::Items),
    }
}
pub fn cycle_configurator<R: Resolver + ?Sized>(
    stack: ItemStack,
    back: bool,
    resolver: &R,
) -> Result<(ItemStack, Payload), Error> {
    let mode = resolve_configurator(stack, resolver)?.cycle(back);
    let payload = Payload::configurator(mode);
    let after = stack
        .without_payload()
        .with_payload(payload.id())
        .map_err(|_| Error::Item)?;
    Ok((after, payload))
}
