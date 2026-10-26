//! Sanitized facts about this owner's currently held inventory values. Immutable
//! ledger records and nested machine inventories never enter this protocol.
pub const MAX_ROWS: usize = 41;
pub const HEADER_BYTES: usize = 16;
pub const ROW_BYTES: usize = 24;
pub const MAX_BYTES: usize = HEADER_BYTES + MAX_ROWS * ROW_BYTES;
/// Stable SPEC registry IDs; net intentionally does not depend on rules.
pub const CONFIGURATOR_ITEM: u16 = 327;
pub const ENERGY_TABLET_ITEM: u16 = 313;
pub const TABLET_CAPACITY: u32 = 1_000_000;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Attribute {
    Configurator(u8),
    Charge(u32),
    SavedMachine(u16),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ItemAttribute {
    pub slot: u8,
    pub item: u16,
    pub payload: u64,
    pub attribute: Attribute,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InventoryAttributes {
    pub inventory_revision: u64,
    pub rows: Vec<ItemAttribute>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Bounds,
    Encoding,
    Order,
    Attribute,
}
impl ItemAttribute {
    pub fn validate(self) -> Result<(), Error> {
        if usize::from(self.slot) >= MAX_ROWS {
            return Err(Error::Bounds);
        }
        let valid = match self.attribute {
            Attribute::Configurator(mode) => {
                self.item == CONFIGURATOR_ITEM && mode <= 4 && (self.payload != 0 || mode == 0)
            }
            Attribute::Charge(joules) => {
                self.item == ENERGY_TABLET_ITEM
                    && joules <= TABLET_CAPACITY
                    && joules.is_multiple_of(20)
                    && (self.payload == 0) == (joules == 0)
            }
            Attribute::SavedMachine(block) => {
                self.payload != 0
                    && self.item == block
                    && matches!(block, 63 | 64 | 66 | 70 | 71 | 72 | 74 | 75 | 76 | 80)
            }
        };
        if !valid {
            return Err(Error::Attribute);
        }
        Ok(())
    }
}
impl InventoryAttributes {
    pub fn validate(&self) -> Result<(), Error> {
        if self.rows.len() > MAX_ROWS {
            return Err(Error::Bounds);
        }
        if self.rows.windows(2).any(|p| p[0].slot >= p[1].slot) {
            return Err(Error::Order);
        }
        for &row in &self.rows {
            row.validate()?;
        }
        Ok(())
    }
    /// A delayed fact cannot describe a replacement stack or a newer inventory.
    pub fn matching(
        &self,
        slot: u8,
        item: u16,
        payload: u64,
        inventory_revision: u64,
    ) -> Option<Attribute> {
        if inventory_revision != self.inventory_revision {
            return None;
        }
        let row = self
            .rows
            .get(self.rows.binary_search_by_key(&slot, |r| r.slot).ok()?)?;
        (row.item == item && row.payload == payload).then_some(row.attribute)
    }
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        self.validate()?;
        let mut bytes = Vec::with_capacity(HEADER_BYTES + self.rows.len() * ROW_BYTES);
        bytes.extend_from_slice(b"IAT1");
        bytes.extend_from_slice(&self.inventory_revision.to_le_bytes());
        bytes.push(self.rows.len() as u8);
        bytes.extend_from_slice(&[0; 3]);
        for row in &self.rows {
            let (tag, value) = match row.attribute {
                Attribute::Configurator(mode) => (0, u32::from(mode)),
                Attribute::Charge(joules) => (1, joules),
                Attribute::SavedMachine(block) => (2, u32::from(block)),
            };
            bytes.extend_from_slice(&[row.slot, tag]);
            bytes.extend_from_slice(&row.item.to_le_bytes());
            bytes.extend_from_slice(&row.payload.to_le_bytes());
            bytes.extend_from_slice(&value.to_le_bytes());
            bytes.extend_from_slice(&[0; 8]);
        }
        Ok(bytes)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() < HEADER_BYTES || bytes.len() > MAX_BYTES {
            return Err(Error::Bounds);
        }
        if &bytes[..4] != b"IAT1" || bytes[13..16] != [0; 3] {
            return Err(Error::Encoding);
        }
        let count = usize::from(bytes[12]);
        if count > MAX_ROWS || bytes.len() != HEADER_BYTES + count * ROW_BYTES {
            return Err(Error::Bounds);
        }
        let inventory_revision =
            u64::from_le_bytes(bytes[4..12].try_into().map_err(|_| Error::Encoding)?);
        let mut rows = Vec::with_capacity(count);
        for raw in bytes[HEADER_BYTES..].chunks_exact(ROW_BYTES) {
            if raw[16..24] != [0; 8] {
                return Err(Error::Encoding);
            }
            let item = u16::from_le_bytes(raw[2..4].try_into().map_err(|_| Error::Encoding)?);
            let payload = u64::from_le_bytes(raw[4..12].try_into().map_err(|_| Error::Encoding)?);
            let value = u32::from_le_bytes(raw[12..16].try_into().map_err(|_| Error::Encoding)?);
            let attribute = match raw[1] {
                0 => Attribute::Configurator(u8::try_from(value).map_err(|_| Error::Attribute)?),
                1 => Attribute::Charge(value),
                2 => Attribute::SavedMachine(u16::try_from(value).map_err(|_| Error::Attribute)?),
                _ => return Err(Error::Encoding),
            };
            rows.push(ItemAttribute {
                slot: raw[0],
                item,
                payload,
                attribute,
            });
        }
        let result = Self {
            inventory_revision,
            rows,
        };
        result.validate()?;
        Ok(result)
    }
}
