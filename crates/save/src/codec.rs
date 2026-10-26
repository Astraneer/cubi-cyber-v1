use crate::*;
use core_types::{
    BlockPos, Cell, ChannelMask, ComponentId, LocalIdx, PAGE_VOLUME, PageId, RegionId, SpaceId,
    SpaceTransform, TxId, Version,
};
use net::{ActionStatus, ServerMessage};
use world::{
    CellChange, CommitWork, ComponentChange, ComponentData, ComponentRecord, GameplayEvent,
    LightChange, LightSample, PageChange, RegionSummary, SpaceChange, SpaceRecord,
    persistence::PersistedPage,
};

struct Writer {
    bytes: Vec<u8>,
    maximum: usize,
    error: Option<SaveError>,
}
impl Writer {
    fn new(maximum: usize) -> Self {
        Self {
            bytes: Vec::new(),
            maximum,
            error: None,
        }
    }
    fn raw(&mut self, bytes: &[u8]) {
        if bytes.len() > self.maximum.saturating_sub(self.bytes.len()) {
            self.error = Some(SaveError::Bounds("encoded bytes"));
        } else if self.error.is_none() {
            self.bytes.extend(bytes);
        }
    }
    fn u8(&mut self, v: u8) {
        self.raw(&[v]);
    }
    fn bool(&mut self, v: bool) {
        self.u8(u8::from(v));
    }
    fn u16(&mut self, v: u16) {
        self.raw(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.raw(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.raw(&v.to_le_bytes());
    }
    fn i32(&mut self, v: i32) {
        self.raw(&v.to_le_bytes());
    }
    fn f64(&mut self, v: f64) {
        self.u64(v.to_bits());
    }
    fn count(&mut self, v: usize, maximum: usize) {
        if v > maximum || v > u32::MAX as usize {
            self.error = Some(SaveError::Bounds("collection length"));
        }
        self.u32(v as u32);
    }
    fn bytes(&mut self, bytes: &[u8], maximum: usize) {
        self.count(bytes.len(), maximum);
        self.raw(bytes);
    }
    fn page(&mut self, p: PageId) {
        self.u32(p.space.0);
        self.i32(p.x);
        self.i32(p.y);
        self.i32(p.z);
    }
    fn pos(&mut self, p: BlockPos) {
        self.u32(p.space.0);
        self.i32(p.x);
        self.i32(p.y);
        self.i32(p.z);
    }
    fn transform(&mut self, v: SpaceTransform) {
        for x in v.translation {
            self.f64(x);
        }
        for x in v.rotation {
            self.f64(x);
        }
    }
    fn cells(&mut self, cells: &[Cell; PAGE_VOLUME]) {
        for cell in cells {
            self.u32(cell.raw());
        }
    }
    fn light(&mut self, v: LightSample) {
        self.u8(v.sky);
        self.u8(v.block);
        self.raw(&v.rgb);
    }
    fn component(&mut self, v: &ComponentRecord) {
        self.pos(v.data.anchor);
        self.u16(v.data.kind);
        self.bytes(&v.data.payload, 65_536);
        self.u64(v.version.0);
    }
    fn identity(&mut self, v: &Identity) {
        self.raw(&v.id.0);
        self.bytes(v.name.as_bytes(), net::MAX_NAME_BYTES);
        self.raw(&v.credential_hash);
        self.u8(v.role as u8);
        self.bool(v.banned);
    }
    fn player(&mut self, v: PlayerState) {
        match net::encode_server(&ServerMessage::State {
            tick: Tick(0),
            player: v,
        }) {
            Ok(bytes) => self.bytes(&bytes, 512),
            Err(_) => self.error = Some(SaveError::Invalid("player state")),
        }
    }
    fn action(&mut self, v: ActionReply) {
        match net::encode_server(&ServerMessage::Action(v)) {
            Ok(bytes) => self.bytes(&bytes, 512),
            Err(_) => self.error = Some(SaveError::Invalid("action result")),
        }
    }
    fn finish(self) -> Result<Vec<u8>, SaveError> {
        if let Some(error) = self.error {
            Err(error)
        } else {
            Ok(self.bytes)
        }
    }
}
struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}
impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8], maximum: usize) -> Result<Self, SaveError> {
        if bytes.len() > maximum {
            return Err(SaveError::Bounds("payload bytes"));
        }
        Ok(Self { bytes, offset: 0 })
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], SaveError> {
        let end = self
            .offset
            .checked_add(n)
            .ok_or(SaveError::Bounds("offset"))?;
        let bytes = self
            .bytes
            .get(self.offset..end)
            .ok_or(SaveError::Invalid("truncated payload"))?;
        self.offset = end;
        Ok(bytes)
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N], SaveError> {
        Ok(self.take(N)?.try_into().expect("checked slice"))
    }
    fn u8(&mut self) -> Result<u8, SaveError> {
        Ok(self.take(1)?[0])
    }
    fn bool(&mut self) -> Result<bool, SaveError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(SaveError::Invalid("boolean")),
        }
    }
    fn u16(&mut self) -> Result<u16, SaveError> {
        Ok(u16::from_le_bytes(self.array()?))
    }
    fn u32(&mut self) -> Result<u32, SaveError> {
        Ok(u32::from_le_bytes(self.array()?))
    }
    fn i32(&mut self) -> Result<i32, SaveError> {
        Ok(i32::from_le_bytes(self.array()?))
    }
    fn u64(&mut self) -> Result<u64, SaveError> {
        Ok(u64::from_le_bytes(self.array()?))
    }
    fn f64(&mut self) -> Result<f64, SaveError> {
        let value = f64::from_bits(self.u64()?);
        if !value.is_finite() {
            return Err(SaveError::Invalid("nonfinite transform"));
        }
        Ok(value)
    }
    fn count(&mut self, maximum: usize, minimum_bytes: usize) -> Result<usize, SaveError> {
        let count = self.u32()? as usize;
        if count > maximum
            || count > self.bytes.len().saturating_sub(self.offset) / minimum_bytes.max(1)
        {
            return Err(SaveError::Bounds("collection length"));
        }
        Ok(count)
    }
    fn bytes(&mut self, maximum: usize) -> Result<Vec<u8>, SaveError> {
        let n = self.count(maximum, 1)?;
        Ok(self.take(n)?.to_vec())
    }
    fn page(&mut self) -> Result<PageId, SaveError> {
        Ok(PageId {
            space: SpaceId(self.u32()?),
            x: self.i32()?,
            y: self.i32()?,
            z: self.i32()?,
        })
    }
    fn pos(&mut self) -> Result<BlockPos, SaveError> {
        Ok(BlockPos {
            space: SpaceId(self.u32()?),
            x: self.i32()?,
            y: self.i32()?,
            z: self.i32()?,
        })
    }
    fn transform(&mut self) -> Result<SpaceTransform, SaveError> {
        Ok(SpaceTransform {
            translation: [self.f64()?, self.f64()?, self.f64()?],
            rotation: [self.f64()?, self.f64()?, self.f64()?, self.f64()?],
        })
    }
    fn cells(&mut self) -> Result<Box<[Cell; PAGE_VOLUME]>, SaveError> {
        let bytes = self.take(PAGE_VOLUME * 4)?;
        let mut cells = Box::new([Cell::AIR; PAGE_VOLUME]);
        for (cell, bytes) in cells.iter_mut().zip(bytes.chunks_exact(4)) {
            *cell = Cell::from_raw(u32::from_le_bytes(bytes.try_into().expect("four bytes")));
        }
        Ok(cells)
    }
    fn light(&mut self) -> Result<LightSample, SaveError> {
        Ok(LightSample {
            sky: self.u8()?,
            block: self.u8()?,
            rgb: self.array()?,
        })
    }
    fn component(&mut self) -> Result<ComponentRecord, SaveError> {
        let anchor = self.pos()?;
        let kind = self.u16()?;
        let payload = self.bytes(65_536)?;
        Ok(ComponentRecord {
            data: ComponentData {
                anchor,
                kind,
                payload,
            },
            version: Version(self.u64()?),
        })
    }
    fn index(&mut self) -> Result<LocalIdx, SaveError> {
        LocalIdx::new(self.u16()?).ok_or(SaveError::Invalid("local index"))
    }
    fn identity(&mut self) -> Result<Identity, SaveError> {
        let id = PlayerId(self.array()?);
        let name = String::from_utf8(self.bytes(net::MAX_NAME_BYTES)?)
            .map_err(|_| SaveError::Invalid("identity UTF8"))?;
        if name.is_empty() || name.chars().any(char::is_control) {
            return Err(SaveError::Invalid("identity name"));
        }
        let credential_hash = self.array()?;
        let role = match self.u8()? {
            0 => Role::Player,
            1 => Role::Admin,
            2 => Role::Owner,
            _ => return Err(SaveError::Invalid("role")),
        };
        Ok(Identity {
            id,
            name,
            credential_hash,
            role,
            banned: self.bool()?,
        })
    }
    fn player(&mut self) -> Result<PlayerState, SaveError> {
        match net::decode_server(&self.bytes(512)?)
            .map_err(|_| SaveError::Invalid("player codec"))?
        {
            ServerMessage::State { player, .. } => Ok(player),
            _ => Err(SaveError::Invalid("player kind")),
        }
    }
    fn action(&mut self) -> Result<ActionReply, SaveError> {
        match net::decode_server(&self.bytes(512)?)
            .map_err(|_| SaveError::Invalid("action codec"))?
        {
            ServerMessage::Action(reply) if !matches!(reply.status, ActionStatus::Pending) => {
                Ok(reply)
            }
            _ => Err(SaveError::Invalid("terminal action kind")),
        }
    }
    fn finish(self) -> Result<(), SaveError> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err(SaveError::Invalid("trailing payload"))
        }
    }
}

fn write_change(w: &mut Writer, change: &WorldChange) {
    w.u64(change.tx.0);
    w.u64(change.tick.0);
    w.u32(change.space.0);
    w.u16(change.channels.0);
    w.count(change.pages.len(), 65_536);
    for page in &change.pages {
        w.page(page.page);
        w.u64(page.before.0);
        w.u64(page.after.0);
        w.u16(page.channels.0);
        w.bool(page.created);
        w.bool(page.removed);
        w.bool(page.initial_cells.is_some());
        if let Some(cells) = &page.initial_cells {
            w.cells(cells);
        }
        w.bool(page.removed_cells.is_some());
        if let Some(cells) = &page.removed_cells {
            w.cells(cells);
        }
    }
    w.count(change.cells.len(), 1_000_000);
    for cell in &change.cells {
        w.page(cell.page);
        w.u16(cell.index.get());
        w.u32(cell.old.raw());
        w.u32(cell.new.raw());
    }
    w.count(change.lights.len(), 1_000_000);
    for light in &change.lights {
        w.page(light.page);
        w.u16(light.index.get());
        w.light(light.old);
        w.light(light.new);
    }
    w.count(change.components.len(), 100_000);
    for component in &change.components {
        w.u64(component.id.0);
        w.u16(component.channels.0);
        w.bool(component.before.is_some());
        if let Some(value) = &component.before {
            w.component(value);
        }
        w.bool(component.after.is_some());
        if let Some(value) = &component.after {
            w.component(value);
        }
    }
    w.count(change.events.len(), 100_000);
    for event in &change.events {
        w.u16(event.kind);
        w.bool(event.subject.is_some());
        if let Some(id) = event.subject {
            w.u64(id.0);
        }
        w.bool(event.position.is_some());
        if let Some(position) = event.position {
            w.pos(position);
        }
        w.bytes(&event.payload, 65_536);
    }
    w.bool(change.transform.is_some());
    if let Some(value) = &change.transform {
        w.bool(value.before.is_some());
        if let Some(value) = value.before {
            w.transform(value);
        }
        w.transform(value.after);
    }
}
fn read_change(r: &mut Reader<'_>) -> Result<WorldChange, SaveError> {
    let tx = TxId(r.u64()?);
    let tick = Tick(r.u64()?);
    let space = SpaceId(r.u32()?);
    let channels = ChannelMask(r.u16()?);
    let count = r.count(65_536, 38)?;
    let mut pages = Vec::with_capacity(count);
    for _ in 0..count {
        let page = r.page()?;
        let before = Version(r.u64()?);
        let after = Version(r.u64()?);
        let channels = ChannelMask(r.u16()?);
        let created = r.bool()?;
        let removed = r.bool()?;
        let initial_cells = if r.bool()? { Some(r.cells()?) } else { None };
        let removed_cells = if r.bool()? { Some(r.cells()?) } else { None };
        pages.push(PageChange {
            page,
            before,
            after,
            channels,
            created,
            removed,
            initial_cells,
            removed_cells,
        });
    }
    let count = r.count(1_000_000, 26)?;
    let mut cells = Vec::with_capacity(count);
    for _ in 0..count {
        cells.push(CellChange {
            page: r.page()?,
            index: r.index()?,
            old: Cell::from_raw(r.u32()?),
            new: Cell::from_raw(r.u32()?),
        });
    }
    let count = r.count(1_000_000, 28)?;
    let mut lights = Vec::with_capacity(count);
    for _ in 0..count {
        lights.push(LightChange {
            page: r.page()?,
            index: r.index()?,
            old: r.light()?,
            new: r.light()?,
        });
    }
    let count = r.count(100_000, 12)?;
    let mut components = Vec::with_capacity(count);
    for _ in 0..count {
        let id = ComponentId(r.u64()?);
        let channels = ChannelMask(r.u16()?);
        let before = if r.bool()? {
            Some(r.component()?)
        } else {
            None
        };
        let after = if r.bool()? {
            Some(r.component()?)
        } else {
            None
        };
        components.push(ComponentChange {
            id,
            before,
            after,
            channels,
        });
    }
    let count = r.count(100_000, 8)?;
    let mut events = Vec::with_capacity(count);
    for _ in 0..count {
        let kind = r.u16()?;
        let subject = if r.bool()? {
            Some(ComponentId(r.u64()?))
        } else {
            None
        };
        let position = if r.bool()? { Some(r.pos()?) } else { None };
        let payload = r.bytes(65_536)?;
        events.push(GameplayEvent {
            kind,
            subject,
            position,
            payload,
        });
    }
    let transform = if r.bool()? {
        let before = if r.bool()? {
            Some(r.transform()?)
        } else {
            None
        };
        Some(SpaceChange {
            before,
            after: r.transform()?,
        })
    } else {
        None
    };
    Ok(WorldChange {
        tx,
        tick,
        space,
        pages,
        cells,
        lights,
        components,
        events,
        transform,
        channels,
        work: CommitWork::default(),
    })
}

pub fn encode_batch(batch: &Batch) -> Result<Vec<u8>, SaveError> {
    let mut w = Writer::new(MAX_RECORD_BYTES);
    w.u64(batch.tick.0);
    w.count(batch.changes.len(), 4096);
    for change in &batch.changes {
        write_change(&mut w, change);
    }
    w.count(batch.identities.len(), MAX_IDENTITIES);
    for identity in &batch.identities {
        w.identity(identity);
    }
    w.count(batch.players.len(), MAX_IDENTITIES);
    for player in &batch.players {
        w.player(player.state);
        w.bytes(&player.simulation, 4096);
        w.u64(player.place_ready_at.0);
        w.u64(player.break_ready_at.0);
    }
    w.count(batch.actions.len(), 65_536);
    for (id, reply) in &batch.actions {
        w.raw(&id.0);
        w.action(*reply);
    }
    write_inventories(&mut w, &batch.players);
    write_windows(&mut w, &batch.players);
    write_runtimes(&mut w, &batch.runtimes)?;
    w.finish()
}
pub fn decode_batch(bytes: &[u8]) -> Result<Batch, SaveError> {
    let mut r = Reader::new(bytes, MAX_RECORD_BYTES)?;
    let tick = Tick(r.u64()?);
    let count = r.count(4096, 43)?;
    let mut changes = Vec::with_capacity(count);
    for _ in 0..count {
        changes.push(read_change(&mut r)?);
    }
    let count = r.count(MAX_IDENTITIES, 54)?;
    let mut identities = Vec::with_capacity(count);
    for _ in 0..count {
        identities.push(r.identity()?);
    }
    let count = r.count(MAX_IDENTITIES, 4)?;
    let mut players = Vec::with_capacity(count);
    for _ in 0..count {
        players.push(SavedPlayer {
            state: r.player()?,
            simulation: r.bytes(4096)?,
            place_ready_at: Tick(r.u64()?),
            break_ready_at: Tick(r.u64()?),
            inventory: Vec::new(),
            window: Vec::new(),
        });
    }
    let count = r.count(65_536, 20)?;
    let mut actions = Vec::with_capacity(count);
    for _ in 0..count {
        actions.push((PlayerId(r.array()?), r.action()?));
    }
    read_inventories(&mut r, &mut players)?;
    read_windows(&mut r, &mut players)?;
    let runtimes = read_runtimes(&mut r)?;
    r.finish()?;
    Ok(Batch {
        tick,
        changes,
        identities,
        players,
        actions,
        runtimes,
    })
}

pub fn encode_snapshot(snapshot: &Snapshot) -> Result<Vec<u8>, SaveError> {
    let mut w = Writer::new(MAX_SNAPSHOT_BYTES);
    w.u64(snapshot.record_sequence);
    w.u64(snapshot.server_tick.0);
    w.u32(snapshot.public_seed);
    w.u32(snapshot.generator_version);
    w.u32(snapshot.rules_version);
    let state = &snapshot.world;
    w.u32(state.schema);
    w.u64(state.tick.0);
    w.u64(state.version.0);
    w.u64(state.next_tx);
    w.count(state.pages.len(), 65_536);
    for page in &state.pages {
        w.page(page.id);
        w.u64(page.version.0);
        for version in page.channel_versions {
            w.u64(version.0);
        }
        w.cells(&page.cells);
        w.bool(page.lights.is_some());
        if let Some(lights) = &page.lights {
            for light in lights.iter() {
                w.light(*light);
            }
        }
        w.count(page.components.len(), PAGE_VOLUME);
        for (index, id) in &page.components {
            w.u16(index.get());
            w.u64(id.0);
        }
    }
    w.count(state.tombstones.len(), 1_000_000);
    for (id, version) in &state.tombstones {
        w.page(*id);
        w.u64(version.0);
    }
    w.count(state.components.len(), 1_000_000);
    for (id, record) in &state.components {
        w.u64(id.0);
        w.component(record);
    }
    w.count(state.spaces.len(), 65_536);
    for (id, space) in &state.spaces {
        w.u32(id.0);
        w.transform(space.transform);
        w.u64(space.version.0);
    }
    w.count(state.regions.len(), 65_536);
    for (id, region) in &state.regions {
        w.u32(id.space.0);
        w.i32(id.x);
        w.i32(id.z);
        w.count(region.loaded_pages, 65_536);
        w.u64(region.version.0);
    }
    w.count(snapshot.identities.len(), MAX_IDENTITIES);
    for identity in &snapshot.identities {
        w.identity(identity);
    }
    w.count(snapshot.players.len(), MAX_IDENTITIES);
    for player in &snapshot.players {
        w.player(player.state);
        w.bytes(&player.simulation, 4096);
        w.u64(player.place_ready_at.0);
        w.u64(player.break_ready_at.0);
    }
    w.count(snapshot.dedup.len(), MAX_IDENTITIES);
    for (id, state) in &snapshot.dedup {
        w.raw(&id.0);
        w.u64(state.contiguous);
        w.u64(state.highest_seen);
        w.count(state.replies.len(), MAX_RECENT_ACTIONS);
        for reply in &state.replies {
            w.action(*reply);
        }
    }
    write_inventories(&mut w, &snapshot.players);
    write_windows(&mut w, &snapshot.players);
    write_runtimes(&mut w, &snapshot.runtimes)?;
    w.finish()
}
pub fn decode_snapshot(bytes: &[u8]) -> Result<Snapshot, SaveError> {
    let mut r = Reader::new(bytes, MAX_SNAPSHOT_BYTES)?;
    let record_sequence = r.u64()?;
    let server_tick = Tick(r.u64()?);
    let public_seed = r.u32()?;
    let generator_version = r.u32()?;
    let rules_version = r.u32()?;
    let schema = r.u32()?;
    let tick = Tick(r.u64()?);
    let version = Version(r.u64()?);
    let next_tx = r.u64()?;
    let count = r.count(65_536, 16493)?;
    let mut pages = Vec::with_capacity(count);
    for _ in 0..count {
        let id = r.page()?;
        let version = Version(r.u64()?);
        let mut channel_versions = [Version(0); 10];
        for version in &mut channel_versions {
            *version = Version(r.u64()?);
        }
        let cells = r.cells()?;
        let lights = if r.bool()? {
            let mut lights = Box::new([LightSample::default(); PAGE_VOLUME]);
            for light in lights.iter_mut() {
                *light = r.light()?;
            }
            Some(lights)
        } else {
            None
        };
        let count = r.count(PAGE_VOLUME, 10)?;
        let mut components = Vec::with_capacity(count);
        for _ in 0..count {
            components.push((r.index()?, ComponentId(r.u64()?)));
        }
        pages.push(PersistedPage {
            id,
            version,
            channel_versions,
            cells,
            lights,
            components,
        });
    }
    let count = r.count(1_000_000, 24)?;
    let mut tombstones = Vec::with_capacity(count);
    for _ in 0..count {
        tombstones.push((r.page()?, Version(r.u64()?)));
    }
    let count = r.count(1_000_000, 38)?;
    let mut components = Vec::with_capacity(count);
    for _ in 0..count {
        components.push((ComponentId(r.u64()?), r.component()?));
    }
    let count = r.count(65_536, 68)?;
    let mut spaces = Vec::with_capacity(count);
    for _ in 0..count {
        spaces.push((
            SpaceId(r.u32()?),
            SpaceRecord {
                transform: r.transform()?,
                version: Version(r.u64()?),
            },
        ));
    }
    let count = r.count(65_536, 24)?;
    let mut regions = Vec::with_capacity(count);
    for _ in 0..count {
        let id = RegionId {
            space: SpaceId(r.u32()?),
            x: r.i32()?,
            z: r.i32()?,
        };
        let loaded_pages = r.u32()? as usize;
        if loaded_pages > 65_536 {
            return Err(SaveError::Bounds("region pages"));
        }
        regions.push((
            id,
            RegionSummary {
                loaded_pages,
                version: Version(r.u64()?),
            },
        ));
    }
    let count = r.count(MAX_IDENTITIES, 54)?;
    let mut identities = Vec::with_capacity(count);
    for _ in 0..count {
        identities.push(r.identity()?);
    }
    let count = r.count(MAX_IDENTITIES, 4)?;
    let mut players = Vec::with_capacity(count);
    for _ in 0..count {
        players.push(SavedPlayer {
            state: r.player()?,
            simulation: r.bytes(4096)?,
            place_ready_at: Tick(r.u64()?),
            break_ready_at: Tick(r.u64()?),
            inventory: Vec::new(),
            window: Vec::new(),
        });
    }
    let count = r.count(MAX_IDENTITIES, 36)?;
    let mut dedup = Vec::with_capacity(count);
    for _ in 0..count {
        let id = PlayerId(r.array()?);
        let contiguous = r.u64()?;
        let highest_seen = r.u64()?;
        let n = r.count(MAX_RECENT_ACTIONS, 4)?;
        let mut replies = Vec::with_capacity(n);
        for _ in 0..n {
            replies.push(r.action()?);
        }
        dedup.push((
            id,
            DedupState {
                contiguous,
                highest_seen,
                replies,
            },
        ));
    }
    read_inventories(&mut r, &mut players)?;
    read_windows(&mut r, &mut players)?;
    let runtimes = read_runtimes(&mut r)?;
    r.finish()?;
    Ok(Snapshot {
        record_sequence,
        server_tick,
        public_seed,
        generator_version,
        rules_version,
        world: RestoreState {
            schema,
            tick,
            version,
            next_tx,
            pages,
            tombstones,
            components,
            spaces,
            regions,
        },
        identities,
        players,
        dedup,
        runtimes,
    })
}

// A tagged optional extension preserves the exact M6 record prefix. Existing
// accounts without this extension receive zero items; malformed/unknown tags do
// not become fresh inventories. Major incompatible schemas still require a
// format migration.
fn write_inventories(w: &mut Writer, players: &[SavedPlayer]) {
    let count = players.iter().filter(|p| !p.inventory.is_empty()).count();
    if count == 0 {
        return;
    }
    w.raw(b"INV1");
    w.count(count, MAX_IDENTITIES);
    for (index, player) in players.iter().enumerate() {
        if !player.inventory.is_empty() {
            w.u32(index as u32);
            w.bytes(&player.inventory, 1024);
        }
    }
}
fn read_inventories(r: &mut Reader<'_>, players: &mut [SavedPlayer]) -> Result<(), SaveError> {
    if !next_tag(r, b"INV1") {
        return Ok(());
    }
    if r.array::<4>()? != *b"INV1" {
        return Err(SaveError::Invalid("unknown inventory extension"));
    }
    let count = r.count(MAX_IDENTITIES, 8)?;
    let mut seen = vec![false; players.len()];
    for _ in 0..count {
        let index = r.u32()? as usize;
        let entry = seen
            .get_mut(index)
            .ok_or(SaveError::Invalid("inventory owner index"))?;
        if *entry {
            return Err(SaveError::Invalid("duplicate inventory extension"));
        }
        *entry = true;
        players[index].inventory = r.bytes(1024)?;
    }
    Ok(())
}

fn next_tag(r: &Reader<'_>, tag: &[u8; 4]) -> bool {
    r.bytes.get(r.offset..r.offset + 4) == Some(tag.as_slice())
}
fn write_windows(w: &mut Writer, players: &[SavedPlayer]) {
    let count = players
        .iter()
        .filter(|player| !player.window.is_empty())
        .count();
    if count == 0 {
        return;
    }
    w.raw(b"WND1");
    w.count(count, MAX_IDENTITIES);
    for (index, player) in players.iter().enumerate() {
        if !player.window.is_empty() {
            w.u32(index as u32);
            w.bytes(&player.window, 4096);
        }
    }
}
fn read_windows(r: &mut Reader<'_>, players: &mut [SavedPlayer]) -> Result<(), SaveError> {
    if !next_tag(r, b"WND1") {
        return Ok(());
    }
    r.take(4)?;
    let count = r.count(MAX_IDENTITIES, 8)?;
    let mut seen = vec![false; players.len()];
    for _ in 0..count {
        let index = r.u32()? as usize;
        let entry = seen
            .get_mut(index)
            .ok_or(SaveError::Invalid("window owner index"))?;
        if *entry {
            return Err(SaveError::Invalid("duplicate window extension"));
        }
        *entry = true;
        let window = r.bytes(4096)?;
        if window.is_empty() {
            return Err(SaveError::Invalid("empty window extension"));
        }
        players[index].window = window;
    }
    Ok(())
}
fn write_runtimes(w: &mut Writer, records: &[RuntimeRecord]) -> Result<(), SaveError> {
    if records.is_empty() {
        return Ok(());
    }
    if records.len() > MAX_RUNTIME_RECORDS {
        return Err(SaveError::Bounds("runtime records"));
    }
    let mut seen = 0u16;
    for record in records {
        if !(RUNTIME_SURVIVAL..=RUNTIME_LIGHT).contains(&record.kind) {
            return Err(SaveError::Invalid("runtime kind"));
        }
        let bit = 1u16 << record.kind;
        if seen & bit != 0 {
            return Err(SaveError::Invalid("duplicate runtime kind"));
        }
        seen |= bit;
        if record.payload.len() > MAX_RUNTIME_BYTES {
            return Err(SaveError::Bounds("runtime payload"));
        }
        if record.payload.is_empty() {
            return Err(SaveError::Invalid("empty runtime payload"));
        }
    }
    w.raw(b"RUN1");
    w.count(records.len(), MAX_RUNTIME_RECORDS);
    for kind in RUNTIME_SURVIVAL..=RUNTIME_LIGHT {
        if let Some(record) = records.iter().find(|record| record.kind == kind) {
            w.u16(kind);
            w.bytes(&record.payload, MAX_RUNTIME_BYTES);
        }
    }
    Ok(())
}
fn read_runtimes(r: &mut Reader<'_>) -> Result<Vec<RuntimeRecord>, SaveError> {
    if r.offset == r.bytes.len() {
        return Ok(Vec::new());
    }
    if r.array::<4>()? != *b"RUN1" {
        return Err(SaveError::Invalid("unknown runtime extension"));
    }
    let count = r.count(MAX_RUNTIME_RECORDS, 6)?;
    let mut records = Vec::with_capacity(count);
    let mut previous = 0u16;
    for _ in 0..count {
        let kind = r.u16()?;
        if !(RUNTIME_SURVIVAL..=RUNTIME_LIGHT).contains(&kind) || kind <= previous {
            return Err(SaveError::Invalid("runtime kind/order"));
        }
        previous = kind;
        let payload = r.bytes(MAX_RUNTIME_BYTES)?;
        if payload.is_empty() {
            return Err(SaveError::Invalid("empty runtime payload"));
        }
        records.push(RuntimeRecord { kind, payload });
    }
    Ok(records)
}
