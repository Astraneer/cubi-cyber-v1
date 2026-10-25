use crate::*;
use std::collections::BTreeMap;

const MAGIC: &[u8; 4] = b"CUBI";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodecError {
    TooLarge,
    Truncated,
    Magic,
    Version,
    Checksum,
    InvalidEnum,
    InvalidBounds,
    InvalidUtf8,
    Trailing,
}
impl std::fmt::Display for CodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "bounded protocol error: {self:?}")
    }
}
impl std::error::Error for CodecError {}

fn checksum(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
    })
}

struct Writer(Vec<u8>);
impl Writer {
    fn byte(&mut self, value: u8) {
        self.0.push(value);
    }
    fn bool(&mut self, value: bool) {
        self.byte(u8::from(value));
    }
    fn u16(&mut self, value: u16) {
        self.0.extend(value.to_le_bytes());
    }
    fn u32(&mut self, value: u32) {
        self.0.extend(value.to_le_bytes());
    }
    fn i32(&mut self, value: i32) {
        self.0.extend(value.to_le_bytes());
    }
    fn u64(&mut self, value: u64) {
        self.0.extend(value.to_le_bytes());
    }
    fn f32(&mut self, value: f32) {
        self.u32(value.to_bits());
    }
    fn f64(&mut self, value: f64) {
        self.u64(value.to_bits());
    }
    fn page(&mut self, page: PageId) {
        self.u32(page.space.0);
        self.i32(page.x);
        self.i32(page.y);
        self.i32(page.z);
    }
    fn position(&mut self, p: BlockPos) {
        self.u32(p.space.0);
        self.i32(p.x);
        self.i32(p.y);
        self.i32(p.z);
    }
    fn token(&mut self, token: Option<SecretToken>) {
        self.bool(token.is_some());
        if let Some(token) = token {
            self.0.extend(token.as_bytes());
        }
    }
    fn string(&mut self, text: &str) {
        self.u16(text.len() as u16);
        self.0.extend(text.as_bytes());
    }
    fn player(&mut self, player: PlayerState) {
        self.0.extend(player.identity.0);
        self.u32(player.space.0);
        self.u64(player.input_sequence);
        for value in player.position {
            self.f64(value);
        }
        for value in player.velocity {
            self.f64(value);
        }
        self.f32(player.yaw);
        self.f32(player.pitch);
        self.f32(player.health);
        self.byte(player.mode);
        self.bool(player.on_ground);
        self.bool(player.flying);
        self.bool(player.noclip);
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], CodecError> {
        let end = self.offset.checked_add(n).ok_or(CodecError::TooLarge)?;
        let bytes = self
            .bytes
            .get(self.offset..end)
            .ok_or(CodecError::Truncated)?;
        self.offset = end;
        Ok(bytes)
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N], CodecError> {
        Ok(self.take(N)?.try_into().expect("exact checked length"))
    }
    fn byte(&mut self) -> Result<u8, CodecError> {
        Ok(self.take(1)?[0])
    }
    fn bool(&mut self) -> Result<bool, CodecError> {
        match self.byte()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(CodecError::InvalidEnum),
        }
    }
    fn u16(&mut self) -> Result<u16, CodecError> {
        Ok(u16::from_le_bytes(self.array()?))
    }
    fn u32(&mut self) -> Result<u32, CodecError> {
        Ok(u32::from_le_bytes(self.array()?))
    }
    fn i32(&mut self) -> Result<i32, CodecError> {
        Ok(i32::from_le_bytes(self.array()?))
    }
    fn u64(&mut self) -> Result<u64, CodecError> {
        Ok(u64::from_le_bytes(self.array()?))
    }
    fn f32(&mut self) -> Result<f32, CodecError> {
        let value = f32::from_bits(self.u32()?);
        value
            .is_finite()
            .then_some(value)
            .ok_or(CodecError::InvalidBounds)
    }
    fn f64(&mut self) -> Result<f64, CodecError> {
        let value = f64::from_bits(self.u64()?);
        value
            .is_finite()
            .then_some(value)
            .ok_or(CodecError::InvalidBounds)
    }
    fn page(&mut self) -> Result<PageId, CodecError> {
        let page = PageId {
            space: SpaceId(self.u32()?),
            x: self.i32()?,
            y: self.i32()?,
            z: self.i32()?,
        };
        if [page.x, page.y, page.z]
            .iter()
            .any(|&value| value.unsigned_abs() > (MAX_COORDINATE as u32).div_ceil(16))
        {
            return Err(CodecError::InvalidBounds);
        }
        Ok(page)
    }
    fn position(&mut self) -> Result<BlockPos, CodecError> {
        let p = BlockPos {
            space: SpaceId(self.u32()?),
            x: self.i32()?,
            y: self.i32()?,
            z: self.i32()?,
        };
        if [p.x, p.y, p.z]
            .iter()
            .any(|&value| value.unsigned_abs() >= MAX_COORDINATE as u32)
        {
            return Err(CodecError::InvalidBounds);
        }
        Ok(p)
    }
    fn token(&mut self) -> Result<Option<SecretToken>, CodecError> {
        if self.bool()? {
            Ok(Some(SecretToken::new(self.array()?)))
        } else {
            Ok(None)
        }
    }
    fn string(&mut self, limit: usize) -> Result<String, CodecError> {
        let len = usize::from(self.u16()?);
        if len > limit {
            return Err(CodecError::TooLarge);
        }
        Ok(std::str::from_utf8(self.take(len)?)
            .map_err(|_| CodecError::InvalidUtf8)?
            .to_owned())
    }
    fn player(&mut self) -> Result<PlayerState, CodecError> {
        let identity = PlayerId(self.array()?);
        let space = SpaceId(self.u32()?);
        let input_sequence = self.u64()?;
        let position = [self.f64()?, self.f64()?, self.f64()?];
        let velocity = [self.f64()?, self.f64()?, self.f64()?];
        let player = PlayerState {
            identity,
            space,
            input_sequence,
            position,
            velocity,
            yaw: self.f32()?,
            pitch: self.f32()?,
            health: self.f32()?,
            mode: self.byte()?,
            on_ground: self.bool()?,
            flying: self.bool()?,
            noclip: self.bool()?,
        };
        if !valid_position(player.position)
            || player.velocity.iter().any(|value| value.abs() > 64.0)
            || !matches!(player.mode, 0 | 1 | 3)
            || player.pitch.abs() > 90.0
            || !(0.0..=20.0).contains(&player.health)
        {
            return Err(CodecError::InvalidBounds);
        }
        Ok(player)
    }
}

fn wrap(writer: Writer) -> Result<Vec<u8>, CodecError> {
    if writer.0.len() + 18 > MAX_FRAME_BYTES {
        return Err(CodecError::TooLarge);
    }
    let mut out = Vec::with_capacity(writer.0.len() + 18);
    out.extend(MAGIC);
    out.extend(PROTOCOL_VERSION.to_le_bytes());
    out.extend((writer.0.len() as u32).to_le_bytes());
    out.extend(checksum(&writer.0).to_le_bytes());
    out.extend(writer.0);
    Ok(out)
}

fn unwrap(bytes: &[u8]) -> Result<Reader<'_>, CodecError> {
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(CodecError::TooLarge);
    }
    let mut reader = Reader { bytes, offset: 0 };
    if reader.take(4)? != MAGIC {
        return Err(CodecError::Magic);
    }
    if reader.u16()? != PROTOCOL_VERSION {
        return Err(CodecError::Version);
    }
    let len = reader.u32()? as usize;
    let expected = reader.u64()?;
    if len != bytes.len().saturating_sub(18) {
        return Err(CodecError::InvalidBounds);
    }
    let payload = reader.take(len)?;
    if checksum(payload) != expected {
        return Err(CodecError::Checksum);
    }
    Ok(Reader {
        bytes: payload,
        offset: 0,
    })
}

fn finish(reader: Reader<'_>) -> Result<(), CodecError> {
    if reader.offset == reader.bytes.len() {
        Ok(())
    } else {
        Err(CodecError::Trailing)
    }
}

fn reason(value: u8) -> Result<RejectReason, CodecError> {
    Ok(match value {
        0 => RejectReason::Protocol,
        1 => RejectReason::Unauthorized,
        2 => RejectReason::Banned,
        3 => RejectReason::Capacity,
        4 => RejectReason::Unknown,
        5 => RejectReason::OutOfReach,
        6 => RejectReason::Occluded,
        7 => RejectReason::Protected,
        8 => RejectReason::Unbreakable,
        9 => RejectReason::Occupied,
        10 => RejectReason::Stale,
        11 => RejectReason::InvalidCell,
        12 => RejectReason::RateLimited,
        13 => RejectReason::DebugRequired,
        14 => RejectReason::Unsupported,
        15 => RejectReason::Canceled,
        16 => RejectReason::AlreadyFinal,
        17 => RejectReason::StorageUnavailable,
        _ => return Err(CodecError::InvalidEnum),
    })
}

pub fn encode_client(message: &ClientMessage) -> Result<Vec<u8>, CodecError> {
    let mut out = Writer(Vec::new());
    match message {
        ClientMessage::Hello(hello) => {
            if hello.name.is_empty()
                || hello.name.len() > MAX_NAME_BYTES
                || hello.name.chars().any(char::is_control)
            {
                return Err(CodecError::InvalidBounds);
            }
            out.byte(0);
            out.0.extend(hello.identity.0);
            out.string(&hello.name);
            out.token(hello.credential);
        }
        ClientMessage::Inputs(inputs) => {
            if inputs.is_empty() || inputs.len() > MAX_INPUT_HISTORY {
                return Err(CodecError::InvalidBounds);
            }
            out.byte(1);
            out.byte(inputs.len() as u8);
            for input in inputs {
                if input.sequence == 0
                    || !input.yaw.is_finite()
                    || !input.pitch.is_finite()
                    || input.pitch.abs() > 90.0
                {
                    return Err(CodecError::InvalidBounds);
                }
                out.u64(input.sequence);
                out.u64(input.target_tick.0);
                out.byte(input.keys);
                out.f32(input.yaw);
                out.f32(input.pitch);
            }
        }
        ClientMessage::Action { id, action } => {
            if id.0 == 0 {
                return Err(CodecError::InvalidBounds);
            }
            out.byte(2);
            out.u64(id.0);
            match action {
                Action::Mine {
                    position,
                    expected,
                    tool,
                } => {
                    out.byte(0);
                    out.position(*position);
                    out.u32(expected.raw());
                    out.u16(*tool);
                }
                Action::StopMining => out.byte(1),
                Action::Place {
                    position,
                    expected,
                    cell,
                } => {
                    out.byte(2);
                    out.position(*position);
                    out.u32(expected.raw());
                    out.u32(cell.raw());
                }
                Action::Flight(enabled) => {
                    out.byte(3);
                    out.bool(*enabled);
                }
                Action::DebugNoclip(enabled) => {
                    out.byte(4);
                    out.bool(*enabled);
                }
                Action::SelectSlot(slot) => {
                    if *slot >= 9 {
                        return Err(CodecError::InvalidBounds);
                    }
                    out.byte(6);
                    out.byte(*slot);
                }
                Action::PlaceOwned {
                    position,
                    expected,
                    cell,
                    inventory,
                } => {
                    if inventory.selected >= 9 {
                        return Err(CodecError::InvalidBounds);
                    }
                    out.byte(7);
                    out.position(*position);
                    out.u32(expected.raw());
                    out.u32(cell.raw());
                    out.u64(inventory.revision);
                    out.byte(inventory.selected);
                    out.u16(inventory.item);
                    out.u16(inventory.count);
                    out.u16(inventory.damage);
                }
                Action::Command(command) => {
                    if command.len() > MAX_COMMAND_BYTES || command.chars().any(char::is_control) {
                        return Err(CodecError::InvalidBounds);
                    }
                    out.byte(5);
                    out.string(command);
                }
            }
        }
        ClientMessage::RequestPage { page, acknowledged } => {
            out.byte(3);
            out.page(*page);
            out.bool(acknowledged.is_some());
            if let Some(version) = acknowledged {
                out.u64(*version);
            }
        }
        ClientMessage::AckPage { page, version } => {
            out.byte(4);
            out.page(*page);
            out.u64(*version);
        }
        ClientMessage::RequestSummary { page } => {
            out.byte(5);
            out.page(*page);
        }
        ClientMessage::Ping(value) => {
            out.byte(6);
            out.u64(*value);
        }
    }
    let frame = wrap(out)?;
    decode_client(&frame)?;
    Ok(frame)
}

pub fn decode_client(bytes: &[u8]) -> Result<ClientMessage, CodecError> {
    let mut input = unwrap(bytes)?;
    let message = match input.byte()? {
        0 => {
            let identity = PlayerId(input.array()?);
            let name = input.string(MAX_NAME_BYTES)?;
            let credential = input.token()?;
            if name.is_empty() || name.chars().any(char::is_control) {
                return Err(CodecError::InvalidBounds);
            }
            ClientMessage::Hello(Hello {
                identity,
                name,
                credential,
            })
        }
        1 => {
            let count = usize::from(input.byte()?);
            if count == 0 || count > MAX_INPUT_HISTORY {
                return Err(CodecError::InvalidBounds);
            }
            let mut inputs = Vec::with_capacity(count);
            for _ in 0..count {
                let value = InputFrame {
                    sequence: input.u64()?,
                    target_tick: Tick(input.u64()?),
                    keys: input.byte()?,
                    yaw: input.f32()?,
                    pitch: input.f32()?,
                };
                if value.sequence == 0
                    || value.pitch.abs() > 90.0
                    || inputs
                        .last()
                        .is_some_and(|previous: &InputFrame| previous.sequence >= value.sequence)
                {
                    return Err(CodecError::InvalidBounds);
                }
                inputs.push(value);
            }
            ClientMessage::Inputs(inputs)
        }
        2 => {
            let id = ActionId(input.u64()?);
            if id.0 == 0 {
                return Err(CodecError::InvalidBounds);
            }
            let action = match input.byte()? {
                0 => Action::Mine {
                    position: input.position()?,
                    expected: Cell::from_raw(input.u32()?),
                    tool: input.u16()?,
                },
                1 => Action::StopMining,
                2 => Action::Place {
                    position: input.position()?,
                    expected: Cell::from_raw(input.u32()?),
                    cell: Cell::from_raw(input.u32()?),
                },
                3 => Action::Flight(input.bool()?),
                4 => Action::DebugNoclip(input.bool()?),
                5 => {
                    let command = input.string(MAX_COMMAND_BYTES)?;
                    if command.chars().any(char::is_control) {
                        return Err(CodecError::InvalidBounds);
                    }
                    Action::Command(command)
                }
                6 => {
                    let slot = input.byte()?;
                    if slot >= 9 {
                        return Err(CodecError::InvalidBounds);
                    }
                    Action::SelectSlot(slot)
                }
                7 => {
                    let position = input.position()?;
                    let expected = Cell::from_raw(input.u32()?);
                    let cell = Cell::from_raw(input.u32()?);
                    let inventory = InventoryGuard {
                        revision: input.u64()?,
                        selected: input.byte()?,
                        item: input.u16()?,
                        count: input.u16()?,
                        damage: input.u16()?,
                    };
                    if inventory.selected >= 9 {
                        return Err(CodecError::InvalidBounds);
                    }
                    Action::PlaceOwned {
                        position,
                        expected,
                        cell,
                        inventory,
                    }
                }
                _ => return Err(CodecError::InvalidEnum),
            };
            ClientMessage::Action { id, action }
        }
        3 => {
            let page = input.page()?;
            let acknowledged = if input.bool()? {
                Some(input.u64()?)
            } else {
                None
            };
            ClientMessage::RequestPage { page, acknowledged }
        }
        4 => ClientMessage::AckPage {
            page: input.page()?,
            version: input.u64()?,
        },
        5 => ClientMessage::RequestSummary {
            page: input.page()?,
        },
        6 => ClientMessage::Ping(input.u64()?),
        _ => return Err(CodecError::InvalidEnum),
    };
    finish(input)?;
    Ok(message)
}

fn write_cells(out: &mut Writer, cells: &[Cell; PAGE_VOLUME]) {
    let mut palette = BTreeMap::new();
    for cell in cells {
        if palette.len() > 256 {
            break;
        }
        let next = palette.len() as u16;
        palette.entry(cell.raw()).or_insert(next);
    }
    if palette.len() == 1 {
        out.byte(0);
        out.u32(cells[0].raw());
    } else if palette.len() <= 256 {
        out.byte(1);
        out.u16(palette.len() as u16);
        let mut ordered = vec![0; palette.len()];
        for (&cell, &index) in &palette {
            ordered[usize::from(index)] = cell;
        }
        for cell in ordered {
            out.u32(cell);
        }
        let bits = (usize::BITS - (palette.len() - 1).leading_zeros()) as usize;
        let mut packed = vec![0u8; (PAGE_VOLUME * bits).div_ceil(8)];
        for (index, cell) in cells.iter().enumerate() {
            let value = palette[&cell.raw()];
            for bit in 0..bits {
                packed[(index * bits + bit) / 8] |=
                    (((value >> bit) & 1) as u8) << ((index * bits + bit) % 8);
            }
        }
        out.0.extend(packed);
    } else {
        out.byte(2);
        for cell in cells {
            out.u32(cell.raw());
        }
    }
}

fn read_cells(input: &mut Reader<'_>) -> Result<Box<[Cell; PAGE_VOLUME]>, CodecError> {
    Ok(match input.byte()? {
        0 => Box::new([Cell::from_raw(input.u32()?); PAGE_VOLUME]),
        1 => {
            let count = usize::from(input.u16()?);
            if !(2..=256).contains(&count) {
                return Err(CodecError::InvalidBounds);
            }
            let mut palette = Vec::with_capacity(count);
            for _ in 0..count {
                let value = Cell::from_raw(input.u32()?);
                if palette.contains(&value) {
                    return Err(CodecError::InvalidBounds);
                }
                palette.push(value);
            }
            let bits = (usize::BITS - (count - 1).leading_zeros()) as usize;
            let packed = input.take((PAGE_VOLUME * bits).div_ceil(8))?;
            let mut cells = Box::new([Cell::AIR; PAGE_VOLUME]);
            for (index, cell) in cells.iter_mut().enumerate() {
                let mut value = 0usize;
                for bit in 0..bits {
                    value |= usize::from(
                        (packed[(index * bits + bit) / 8] >> ((index * bits + bit) % 8)) & 1,
                    ) << bit;
                }
                *cell = *palette.get(value).ok_or(CodecError::InvalidBounds)?;
            }
            cells
        }
        2 => {
            let mut cells = Box::new([Cell::AIR; PAGE_VOLUME]);
            for cell in cells.iter_mut() {
                *cell = Cell::from_raw(input.u32()?);
            }
            cells
        }
        _ => return Err(CodecError::InvalidEnum),
    })
}

pub fn encode_server(message: &ServerMessage) -> Result<Vec<u8>, CodecError> {
    let mut out = Writer(Vec::new());
    match message {
        ServerMessage::Welcome(value) => {
            out.byte(0);
            out.u64(value.session);
            out.0.extend(value.identity.0);
            out.token(value.new_credential);
            out.byte(value.role as u8);
            out.u64(value.tick.0);
            out.u64(value.next_action.0);
            out.u32(value.public_seed);
            out.u32(value.generator_version);
            out.u32(value.rules_version);
            out.player(value.player);
        }
        ServerMessage::State { tick, player } => {
            out.byte(1);
            out.u64(tick.0);
            out.player(*player);
        }
        ServerMessage::Inventory { data } => {
            if !(14..=512).contains(&data.len()) {
                return Err(CodecError::InvalidBounds);
            }
            out.byte(12);
            out.u16(data.len() as u16);
            out.0.extend(data);
        }
        ServerMessage::Players { tick, players } => {
            if players.len() > 64 {
                return Err(CodecError::TooLarge);
            }
            out.byte(2);
            out.u64(tick.0);
            out.byte(players.len() as u8);
            for player in players {
                out.player(*player);
            }
        }
        ServerMessage::Page(PageUpdate::Snapshot(value)) => {
            out.byte(3);
            out.page(value.page);
            out.u64(value.version);
            out.u64(value.source_version.0);
            write_cells(&mut out, &value.cells);
        }
        ServerMessage::Page(PageUpdate::Delta(value)) => {
            if value.changes.len() > PAGE_VOLUME {
                return Err(CodecError::TooLarge);
            }
            out.byte(4);
            out.page(value.page);
            out.u64(value.base);
            out.u64(value.version);
            out.u64(value.source_version.0);
            out.u16(value.changes.len() as u16);
            for (index, cell) in &value.changes {
                out.u16(*index);
                out.u32(cell.raw());
            }
        }
        ServerMessage::PageUnavailable(page) => {
            out.byte(5);
            out.page(*page);
        }
        ServerMessage::Action(reply) => {
            out.byte(6);
            out.u64(reply.id.0);
            match reply.status {
                ActionStatus::Pending => out.byte(0),
                ActionStatus::Accepted => out.byte(1),
                ActionStatus::Rejected(reason) => {
                    out.byte(2);
                    out.byte(reason as u8);
                }
            }
            out.bool(reply.transaction.is_some());
            if let Some(tx) = reply.transaction {
                out.u64(tx.0);
            }
            out.u64(reply.source_version.0);
            out.bool(reply.position.is_some());
            if let Some(position) = reply.position {
                out.position(position);
            }
            out.bool(reply.cell.is_some());
            if let Some(cell) = reply.cell {
                out.u32(cell.raw());
            }
        }
        ServerMessage::Transform {
            space,
            version,
            transform,
        } => {
            out.byte(7);
            out.u32(space.0);
            out.u64(version.0);
            for value in transform.translation {
                out.f64(value);
            }
            for value in transform.rotation {
                out.f64(value);
            }
        }
        ServerMessage::Summary(summary) => {
            out.byte(8);
            out.page(summary.page);
            out.u64(summary.source_version.0);
            out.u64(summary.occupied);
            for material in summary.materials {
                out.u16(material);
            }
        }
        ServerMessage::Pong(value) => {
            out.byte(9);
            out.u64(*value);
        }
        ServerMessage::Error(reason) => {
            out.byte(10);
            out.byte(*reason as u8);
        }
        ServerMessage::Prediction {
            tick,
            player,
            continuation,
        } => {
            if continuation.len() < 6 || continuation.len() > MAX_PREDICTION_BYTES {
                return Err(CodecError::InvalidBounds);
            }
            out.byte(11);
            out.u64(tick.0);
            out.player(*player);
            out.u16(continuation.len() as u16);
            out.0.extend(continuation);
        }
    }
    let frame = wrap(out)?;
    decode_server(&frame)?;
    Ok(frame)
}

pub fn decode_server(bytes: &[u8]) -> Result<ServerMessage, CodecError> {
    let mut input = unwrap(bytes)?;
    let message = match input.byte()? {
        0 => {
            let session = input.u64()?;
            let identity = PlayerId(input.array()?);
            let new_credential = input.token()?;
            let role = match input.byte()? {
                0 => Role::Player,
                1 => Role::Admin,
                2 => Role::Owner,
                _ => return Err(CodecError::InvalidEnum),
            };
            ServerMessage::Welcome(Welcome {
                session,
                identity,
                new_credential,
                role,
                tick: Tick(input.u64()?),
                next_action: ActionId(input.u64()?),
                public_seed: input.u32()?,
                generator_version: input.u32()?,
                rules_version: input.u32()?,
                player: input.player()?,
            })
        }
        1 => ServerMessage::State {
            tick: Tick(input.u64()?),
            player: input.player()?,
        },
        2 => {
            let tick = Tick(input.u64()?);
            let count = usize::from(input.byte()?);
            if count > 64 {
                return Err(CodecError::TooLarge);
            }
            let mut players = Vec::with_capacity(count);
            for _ in 0..count {
                players.push(input.player()?);
            }
            ServerMessage::Players { tick, players }
        }
        3 => {
            let page = input.page()?;
            let version = input.u64()?;
            let source_version = Version(input.u64()?);
            let cells = read_cells(&mut input)?;
            ServerMessage::Page(PageUpdate::Snapshot(PageSnapshot {
                page,
                version,
                source_version,
                cells,
            }))
        }
        4 => {
            let page = input.page()?;
            let base = input.u64()?;
            let version = input.u64()?;
            if version <= base {
                return Err(CodecError::InvalidBounds);
            }
            let source_version = Version(input.u64()?);
            let count = usize::from(input.u16()?);
            if count > PAGE_VOLUME {
                return Err(CodecError::TooLarge);
            }
            let mut changes = Vec::with_capacity(count);
            let mut seen = [false; PAGE_VOLUME];
            for _ in 0..count {
                let index = input.u16()?;
                let valid = seen
                    .get_mut(usize::from(index))
                    .ok_or(CodecError::InvalidBounds)?;
                if *valid {
                    return Err(CodecError::InvalidBounds);
                }
                *valid = true;
                changes.push((index, Cell::from_raw(input.u32()?)));
            }
            ServerMessage::Page(PageUpdate::Delta(PageDelta {
                page,
                base,
                version,
                source_version,
                changes,
            }))
        }
        5 => ServerMessage::PageUnavailable(input.page()?),
        6 => {
            let id = ActionId(input.u64()?);
            let status = match input.byte()? {
                0 => ActionStatus::Pending,
                1 => ActionStatus::Accepted,
                2 => ActionStatus::Rejected(reason(input.byte()?)?),
                _ => return Err(CodecError::InvalidEnum),
            };
            let transaction = if input.bool()? {
                Some(TxId(input.u64()?))
            } else {
                None
            };
            let source_version = Version(input.u64()?);
            let position = if input.bool()? {
                Some(input.position()?)
            } else {
                None
            };
            let cell = if input.bool()? {
                Some(Cell::from_raw(input.u32()?))
            } else {
                None
            };
            ServerMessage::Action(ActionReply {
                id,
                status,
                transaction,
                source_version,
                position,
                cell,
            })
        }
        7 => {
            let space = SpaceId(input.u32()?);
            let version = Version(input.u64()?);
            let translation = [input.f64()?, input.f64()?, input.f64()?];
            let rotation = [input.f64()?, input.f64()?, input.f64()?, input.f64()?];
            let transform = SpaceTransform {
                translation,
                rotation,
            };
            if !transform.is_valid() {
                return Err(CodecError::InvalidBounds);
            }
            ServerMessage::Transform {
                space,
                version,
                transform,
            }
        }
        8 => {
            let page = input.page()?;
            let source_version = Version(input.u64()?);
            let occupied = input.u64()?;
            let mut materials = [0; 64];
            for material in &mut materials {
                *material = input.u16()?;
            }
            ServerMessage::Summary(FarSummary {
                page,
                source_version,
                occupied,
                materials,
            })
        }
        9 => ServerMessage::Pong(input.u64()?),
        10 => ServerMessage::Error(reason(input.byte()?)?),
        11 => {
            let tick = Tick(input.u64()?);
            let player = input.player()?;
            let length = usize::from(input.u16()?);
            if !(6..=MAX_PREDICTION_BYTES).contains(&length) {
                return Err(CodecError::InvalidBounds);
            }
            ServerMessage::Prediction {
                tick,
                player,
                continuation: input.take(length)?.to_vec(),
            }
        }
        12 => {
            let len = usize::from(input.u16()?);
            if !(14..=512).contains(&len) {
                return Err(CodecError::InvalidBounds);
            }
            ServerMessage::Inventory {
                data: input.take(len)?.to_vec(),
            }
        }
        _ => return Err(CodecError::InvalidEnum),
    };
    finish(input)?;
    Ok(message)
}
