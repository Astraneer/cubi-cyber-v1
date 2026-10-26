//! Private window persistence extends the exact WND1 image with an optional
//! bounded binding tail. Legacy unbound windows remain closable after recovery.
use super::*;
const PREFIX_BYTES: usize = 407;
const MAX_BYTES: usize = 4096;
impl BoundWindow {
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = self.state.encode();
        bytes.extend_from_slice(b"WBD1");
        bytes.push(u8::from(self.binding.is_some()));
        if let Some(binding) = self.binding {
            put_block(&mut bytes, binding.target, binding.expected_target);
            put_block(&mut bytes, binding.anchor, binding.expected_anchor);
            bytes.push(u8::from(binding.component.is_some()));
            if let Some(component) = binding.component {
                bytes.extend_from_slice(&component.0.to_le_bytes());
            }
        }
        bytes
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.is_empty() {
            return Self::inventory(1);
        }
        if bytes.len() < PREFIX_BYTES || bytes.len() > MAX_BYTES {
            return Err(Error::Encoding);
        }
        let (state, prefix_len) =
            ui::WindowState::decode_prefix(bytes).map_err(|_| Error::Encoding)?;
        if bytes.len() == prefix_len {
            return Ok(Self {
                state,
                binding: None,
            });
        }
        let mut read = Reader {
            bytes: &bytes[prefix_len..],
            at: 0,
        };
        if &read.take::<4>()? != b"WBD1" {
            return Err(Error::Encoding);
        }
        let binding = if read.boolean()? {
            let (target, expected_target) = read.block()?;
            let (anchor, expected_anchor) = read.block()?;
            let component = if read.boolean()? {
                Some(ComponentId(u64::from_le_bytes(read.take()?)))
            } else {
                None
            };
            let binding = Binding {
                target,
                expected_target,
                anchor,
                expected_anchor,
                component,
            };
            if target.space != anchor.space
                || !valid_position(target)
                || !valid_position(anchor)
                || expected_target.id() == 0
                || expected_anchor.id() == 0
                || state.kind() == ui::WindowKind::Inventory
            {
                return Err(Error::Encoding);
            }
            Some(binding)
        } else {
            None
        };
        if read.at != read.bytes.len() {
            return Err(Error::Encoding);
        }
        Ok(Self { state, binding })
    }
}
fn valid_position(at: BlockPos) -> bool {
    [at.x, at.y, at.z]
        .iter()
        .all(|value| (-net::MAX_COORDINATE..=net::MAX_COORDINATE).contains(value))
}
fn put_block(bytes: &mut Vec<u8>, at: BlockPos, cell: Cell) {
    bytes.extend_from_slice(&at.space.0.to_le_bytes());
    for coord in [at.x, at.y, at.z] {
        bytes.extend_from_slice(&coord.to_le_bytes());
    }
    bytes.extend_from_slice(&cell.raw().to_le_bytes());
}
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        let value = self
            .bytes
            .get(self.at..self.at + N)
            .ok_or(Error::Encoding)?
            .try_into()
            .unwrap();
        self.at += N;
        Ok(value)
    }
    fn boolean(&mut self) -> Result<bool, Error> {
        match self.take::<1>()?[0] {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(Error::Encoding),
        }
    }
    fn block(&mut self) -> Result<(BlockPos, Cell), Error> {
        let space = SpaceId(u32::from_le_bytes(self.take()?));
        let x = i32::from_le_bytes(self.take()?);
        let y = i32::from_le_bytes(self.take()?);
        let z = i32::from_le_bytes(self.take()?);
        let cell = Cell::from_raw(u32::from_le_bytes(self.take()?));
        Ok((BlockPos { space, x, y, z }, cell))
    }
}
