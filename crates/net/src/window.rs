//! Owner-only public window DTO. Persistence uses its own codec; this payload
//! contains no secret token, world seed or imported resource bytes.
use core_types::{BlockPos, Cell, SpaceId};
pub const MAX_WINDOW_BYTES: usize = 4096;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stack {
    pub item: u16,
    pub count: u16,
    pub damage: u16,
    pub payload: u64,
}
impl Stack {
    pub const EMPTY: Self = Self {
        item: 0,
        count: 0,
        damage: 0,
        payload: 0,
    };
    fn valid(self) -> bool {
        self == Self::EMPTY
            || (self.item != 0
                && (1..=64).contains(&self.count)
                && (self.payload == 0 || self.count == 1))
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowGuard {
    pub id: u64,
    pub revision: u64,
    pub inventory_revision: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowKind {
    Inventory,
    Workbench,
    Chest(u8),
    Furnace,
    Machine(u8),
}
impl WindowKind {
    pub fn container_len(self) -> Option<usize> {
        Some(match self {
            Self::Inventory | Self::Workbench => 0,
            Self::Chest(27) => 27,
            Self::Chest(54) => 54,
            Self::Furnace => 3,
            Self::Machine(kind @ 0..=9) => [2, 5, 2, 4, 4, 1, 4, 5, 3, 1][kind as usize],
            _ => return None,
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowIntent {
    OpenInventory,
    OpenBlock {
        position: BlockPos,
        expected: Cell,
    },
    Click {
        guard: WindowGuard,
        slot: u16,
        right: bool,
        shift: bool,
    },
    RecipeFill {
        guard: WindowGuard,
        recipe: u16,
        all: bool,
    },
    TakeCraft {
        guard: WindowGuard,
        all: bool,
    },
    Close {
        guard: WindowGuard,
    },
    MachineSide {
        guard: WindowGuard,
        face: u8,
        mode: u8,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowSnapshot {
    pub guard: WindowGuard,
    pub kind: WindowKind,
    pub closed: bool,
    /// Protected public display: viewing/closing is allowed, mutations are not.
    pub read_only: bool,
    pub binding: Option<BlockPos>,
    pub cursor: Stack,
    pub craft: [Stack; 9],
    pub craft_result: Stack,
    pub container: Vec<Stack>,
    /// Normalized progress/fuel are 0..1000, independent of recipe duration.
    pub progress: u16,
    pub fuel: u16,
    pub energy: u64,
    /// Relative side order: front, left, right, back, top, bottom.
    pub sides: [u8; 6],
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowCodecError {
    Format,
    Limit,
    Value,
    Trailing,
}
fn position_valid(position: BlockPos) -> bool {
    [position.x, position.y, position.z]
        .iter()
        .all(|value| (-crate::MAX_COORDINATE..=crate::MAX_COORDINATE).contains(value))
}
fn slot_valid(slot: u16) -> bool {
    matches!(slot, 0..=94 | 100..=108)
}
fn guard_valid(guard: WindowGuard) -> bool {
    guard.id != 0
}
pub fn validate_intent(intent: WindowIntent) -> Result<(), WindowCodecError> {
    let valid = match intent {
        WindowIntent::OpenInventory => true,
        WindowIntent::OpenBlock { position, expected } => {
            position_valid(position) && expected.id() != 0
        }
        WindowIntent::Click { guard, slot, .. } => guard_valid(guard) && slot_valid(slot),
        WindowIntent::RecipeFill { guard, .. }
        | WindowIntent::TakeCraft { guard, .. }
        | WindowIntent::Close { guard } => guard_valid(guard),
        WindowIntent::MachineSide { guard, face, mode } => {
            guard_valid(guard) && face < 6 && mode < 6
        }
    };
    if valid {
        Ok(())
    } else {
        Err(WindowCodecError::Value)
    }
}
pub fn validate_snapshot(state: &WindowSnapshot) -> Result<(), WindowCodecError> {
    if !guard_valid(state.guard)
        || (state.read_only && (state.closed || state.binding.is_none()))
        || state.kind.container_len() != Some(state.container.len())
        || state.binding.is_some_and(|at| !position_valid(at))
        || state.progress > 1000
        || state.fuel > 1000
        || state.sides.iter().any(|side| *side > 5)
        || !std::iter::once(state.cursor)
            .chain(state.craft)
            .chain(std::iter::once(state.craft_result))
            .chain(state.container.iter().copied())
            .all(Stack::valid)
        || (state.kind != WindowKind::Workbench
            && state
                .craft
                .iter()
                .enumerate()
                .any(|(i, stack)| (i % 3 >= 2 || i / 3 >= 2) && *stack != Stack::EMPTY))
        || (matches!(state.kind, WindowKind::Inventory) != state.binding.is_none())
    {
        return Err(WindowCodecError::Value);
    }
    Ok(())
}
pub fn encode_intent(intent: WindowIntent) -> Result<Vec<u8>, WindowCodecError> {
    validate_intent(intent)?;
    let mut out = Vec::with_capacity(40);
    out.extend_from_slice(b"WIN1");
    match intent {
        WindowIntent::OpenInventory => out.push(0),
        WindowIntent::OpenBlock { position, expected } => {
            out.push(1);
            put_position(&mut out, position);
            out.extend_from_slice(&expected.raw().to_le_bytes());
        }
        WindowIntent::Click {
            guard,
            slot,
            right,
            shift,
        } => {
            out.push(2);
            put_guard(&mut out, guard);
            out.extend_from_slice(&slot.to_le_bytes());
            out.push(u8::from(right) | u8::from(shift) << 1);
        }
        WindowIntent::RecipeFill { guard, recipe, all } => {
            out.push(3);
            put_guard(&mut out, guard);
            out.extend_from_slice(&recipe.to_le_bytes());
            out.push(u8::from(all));
        }
        WindowIntent::TakeCraft { guard, all } => {
            out.push(4);
            put_guard(&mut out, guard);
            out.push(u8::from(all));
        }
        WindowIntent::Close { guard } => {
            out.push(5);
            put_guard(&mut out, guard);
        }
        WindowIntent::MachineSide { guard, face, mode } => {
            out.push(6);
            put_guard(&mut out, guard);
            out.extend_from_slice(&[face, mode]);
        }
    }
    Ok(out)
}
pub fn decode_intent(bytes: &[u8]) -> Result<WindowIntent, WindowCodecError> {
    let mut read = Reader::new(bytes, b"WIN1")?;
    let intent = match read.byte()? {
        0 => WindowIntent::OpenInventory,
        1 => WindowIntent::OpenBlock {
            position: read.position()?,
            expected: Cell::from_raw(read.u32()?),
        },
        2 => {
            let guard = read.guard()?;
            let slot = read.u16()?;
            let flags = read.byte()?;
            if flags > 3 {
                return Err(WindowCodecError::Value);
            }
            WindowIntent::Click {
                guard,
                slot,
                right: flags & 1 != 0,
                shift: flags & 2 != 0,
            }
        }
        3 => WindowIntent::RecipeFill {
            guard: read.guard()?,
            recipe: read.u16()?,
            all: read.boolean()?,
        },
        4 => WindowIntent::TakeCraft {
            guard: read.guard()?,
            all: read.boolean()?,
        },
        5 => WindowIntent::Close {
            guard: read.guard()?,
        },
        6 => WindowIntent::MachineSide {
            guard: read.guard()?,
            face: read.byte()?,
            mode: read.byte()?,
        },
        _ => return Err(WindowCodecError::Value),
    };
    read.finish()?;
    validate_intent(intent)?;
    Ok(intent)
}
pub fn encode_snapshot(state: &WindowSnapshot) -> Result<Vec<u8>, WindowCodecError> {
    validate_snapshot(state)?;
    let mut out = Vec::with_capacity(512);
    out.extend_from_slice(b"WUI1");
    put_guard(&mut out, state.guard);
    let (kind, parameter) = match state.kind {
        WindowKind::Inventory => (0, 0),
        WindowKind::Workbench => (1, 0),
        WindowKind::Chest(slots) => (2, slots),
        WindowKind::Furnace => (3, 0),
        WindowKind::Machine(kind) => (4, kind),
    };
    out.extend_from_slice(&[
        kind,
        parameter,
        u8::from(state.closed) | (u8::from(state.read_only) << 1),
        u8::from(state.binding.is_some()),
    ]);
    if let Some(position) = state.binding {
        put_position(&mut out, position);
    }
    for stack in std::iter::once(state.cursor)
        .chain(state.craft)
        .chain(std::iter::once(state.craft_result))
        .chain(state.container.iter().copied())
    {
        for value in [stack.item, stack.count, stack.damage] {
            out.extend_from_slice(&value.to_le_bytes());
        }
    }
    out.extend_from_slice(&state.progress.to_le_bytes());
    out.extend_from_slice(&state.fuel.to_le_bytes());
    out.extend_from_slice(&state.energy.to_le_bytes());
    out.extend_from_slice(&state.sides);
    let stacks = || {
        std::iter::once(state.cursor)
            .chain(state.craft)
            .chain(std::iter::once(state.craft_result))
            .chain(state.container.iter().copied())
    };
    let refs = stacks().filter(|s| s.payload != 0).count();
    if refs != 0 {
        out.extend_from_slice(b"XRF1");
        out.extend_from_slice(&(refs as u16).to_le_bytes());
        for (index, stack) in stacks().enumerate() {
            if stack.payload != 0 {
                out.extend_from_slice(&(index as u16).to_le_bytes());
                out.extend_from_slice(&stack.payload.to_le_bytes());
            }
        }
    }
    Ok(out)
}
pub fn decode_snapshot(bytes: &[u8]) -> Result<WindowSnapshot, WindowCodecError> {
    let mut read = Reader::new(bytes, b"WUI1")?;
    let guard = read.guard()?;
    let kind = match (read.byte()?, read.byte()?) {
        (0, 0) => WindowKind::Inventory,
        (1, 0) => WindowKind::Workbench,
        (2, n @ (27 | 54)) => WindowKind::Chest(n),
        (3, 0) => WindowKind::Furnace,
        (4, n @ 0..=9) => WindowKind::Machine(n),
        _ => return Err(WindowCodecError::Value),
    };
    let flags = read.byte()?;
    if flags & !3 != 0 {
        return Err(WindowCodecError::Value);
    }
    let closed = flags & 1 != 0;
    let read_only = flags & 2 != 0;
    let binding = if read.boolean()? {
        Some(read.position()?)
    } else {
        None
    };
    let mut cursor = read.stack()?;
    let mut craft = [Stack::EMPTY; 9];
    for cell in &mut craft {
        *cell = read.stack()?;
    }
    let mut craft_result = read.stack()?;
    let length = kind.container_len().ok_or(WindowCodecError::Value)?;
    let mut container = Vec::with_capacity(length);
    for _ in 0..length {
        container.push(read.stack()?);
    }
    let progress = read.u16()?;
    let fuel = read.u16()?;
    let energy = read.u64()?;
    let sides = read.take()?;
    if read.at < read.bytes.len() {
        if read.take::<4>()? != *b"XRF1" {
            return Err(WindowCodecError::Format);
        }
        let count = usize::from(read.u16()?);
        let maximum = 11 + container.len();
        if count == 0 || count > maximum {
            return Err(WindowCodecError::Limit);
        }
        let mut previous = None;
        for _ in 0..count {
            let index = usize::from(read.u16()?);
            let payload = read.u64()?;
            if index >= maximum || payload == 0 || previous.is_some_and(|old| old >= index) {
                return Err(WindowCodecError::Value);
            }
            previous = Some(index);
            match index {
                0 => cursor.payload = payload,
                1..=9 => craft[index - 1].payload = payload,
                10 => craft_result.payload = payload,
                _ => container[index - 11].payload = payload,
            }
        }
    }
    read.finish()?;
    let state = WindowSnapshot {
        guard,
        kind,
        closed,
        read_only,
        binding,
        cursor,
        craft,
        craft_result,
        container,
        progress,
        fuel,
        energy,
        sides,
    };
    validate_snapshot(&state)?;
    Ok(state)
}
fn put_guard(out: &mut Vec<u8>, guard: WindowGuard) {
    for value in [guard.id, guard.revision, guard.inventory_revision] {
        out.extend_from_slice(&value.to_le_bytes());
    }
}
fn put_position(out: &mut Vec<u8>, at: BlockPos) {
    out.extend_from_slice(&at.space.0.to_le_bytes());
    for value in [at.x, at.y, at.z] {
        out.extend_from_slice(&value.to_le_bytes());
    }
}
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8], magic: &[u8; 4]) -> Result<Self, WindowCodecError> {
        if bytes.len() > MAX_WINDOW_BYTES {
            return Err(WindowCodecError::Limit);
        }
        if bytes.get(..4) != Some(magic) {
            return Err(WindowCodecError::Format);
        }
        Ok(Self { bytes, at: 4 })
    }
    fn take<const N: usize>(&mut self) -> Result<[u8; N], WindowCodecError> {
        let result = self
            .bytes
            .get(self.at..self.at + N)
            .ok_or(WindowCodecError::Format)?
            .try_into()
            .unwrap();
        self.at += N;
        Ok(result)
    }
    fn byte(&mut self) -> Result<u8, WindowCodecError> {
        Ok(self.take::<1>()?[0])
    }
    fn boolean(&mut self) -> Result<bool, WindowCodecError> {
        match self.byte()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(WindowCodecError::Value),
        }
    }
    fn u16(&mut self) -> Result<u16, WindowCodecError> {
        Ok(u16::from_le_bytes(self.take()?))
    }
    fn u32(&mut self) -> Result<u32, WindowCodecError> {
        Ok(u32::from_le_bytes(self.take()?))
    }
    fn u64(&mut self) -> Result<u64, WindowCodecError> {
        Ok(u64::from_le_bytes(self.take()?))
    }
    fn guard(&mut self) -> Result<WindowGuard, WindowCodecError> {
        Ok(WindowGuard {
            id: self.u64()?,
            revision: self.u64()?,
            inventory_revision: self.u64()?,
        })
    }
    fn position(&mut self) -> Result<BlockPos, WindowCodecError> {
        Ok(BlockPos {
            space: SpaceId(self.u32()?),
            x: i32::from_le_bytes(self.take()?),
            y: i32::from_le_bytes(self.take()?),
            z: i32::from_le_bytes(self.take()?),
        })
    }
    fn stack(&mut self) -> Result<Stack, WindowCodecError> {
        Ok(Stack {
            item: self.u16()?,
            count: self.u16()?,
            damage: self.u16()?,
            payload: 0,
        })
    }
    fn finish(&self) -> Result<(), WindowCodecError> {
        if self.at == self.bytes.len() {
            Ok(())
        } else {
            Err(WindowCodecError::Trailing)
        }
    }
}
