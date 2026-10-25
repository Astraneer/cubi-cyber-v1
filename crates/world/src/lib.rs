//! Transactional page world. Reads never generate data and every consumer owns
//! a journal cursor. Only touched pages/components are staged for atomic commit.
#![forbid(unsafe_code)]

pub mod lighting;
pub mod persistence;

mod journal;
mod storage;
use core_types::{
    BlockPos, Cell, CellProperties, ChannelMask, ComponentId, LocalIdx, PAGE_VOLUME, PageId,
    RegionId, SpaceId, SpaceTransform, Tick, TxId, Version,
};
use journal::Journal;
pub use journal::{
    CoalescerFull, CursorError, CursorId, DerivedChanges, JournalBatch, JournalLimits,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
pub use storage::{
    CellMask, DENSE_ENTER_UNIQUE, DENSE_LEAVE_UNIQUE, PageMasks, PageStorage, StorageMode,
};

/// Registry results must be deterministic and immutable for this world's rules
/// version. Mutable instrumentation is fine; mutable block definitions are not.
pub trait Registry: Send + Sync {
    fn properties(&self, cell: Cell) -> CellProperties;
}
impl<F: Fn(Cell) -> CellProperties + Send + Sync> Registry for F {
    fn properties(&self, cell: Cell) -> CellProperties {
        self(cell)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellRead {
    Unknown,
    Known(Cell),
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LightSample {
    pub sky: u8,
    pub block: u8,
    pub rgb: [u8; 3],
}
#[derive(Clone, Debug)]
struct LightField(Box<[LightSample; PAGE_VOLUME]>);

#[derive(Clone, Debug)]
pub struct Page {
    cells: PageStorage,
    masks: PageMasks,
    lights: Option<LightField>,
    components: BTreeMap<LocalIdx, ComponentId>,
    version: Version,
    channel_versions: [Version; 10],
    // Derived cache stamps, deliberately excluded from persistence and logical
    // hashing. Non-centre regions are exactly one boundary plane/edge/corner.
    rim_versions: [Version; 27],
    classified_cells: u64,
}
impl Page {
    fn new(cells: Box<[Cell; PAGE_VOLUME]>, registry: &dyn Registry) -> Self {
        let mut masks = PageMasks::default();
        for (index, cell) in cells.iter().enumerate() {
            masks.update(
                LocalIdx::new(index as u16).expect("page index"),
                registry.properties(*cell),
            );
        }
        Self {
            cells: PageStorage::from_cells(cells),
            masks,
            lights: None,
            components: BTreeMap::new(),
            version: Version(0),
            channel_versions: [Version(0); 10],
            rim_versions: [Version(0); 27],
            classified_cells: PAGE_VOLUME as u64,
        }
    }
    pub fn cell(&self, index: LocalIdx) -> Cell {
        self.cells.get(index)
    }
    pub fn cells(&self) -> Box<[Cell; PAGE_VOLUME]> {
        self.cells.to_cells()
    }
    pub fn storage(&self) -> &PageStorage {
        &self.cells
    }
    pub fn storage_mode(&self) -> StorageMode {
        self.cells.mode()
    }
    pub fn masks(&self) -> &PageMasks {
        &self.masks
    }
    pub fn version(&self) -> Version {
        self.version
    }
    pub fn channel_version(&self, channels: ChannelMask) -> Version {
        self.channel_versions
            .iter()
            .enumerate()
            .filter(|(bit, _)| channels.0 & (1 << bit) != 0)
            .map(|(_, version)| *version)
            .max()
            .unwrap_or(Version(0))
    }
    pub fn sampled_version(&self, channels: ChannelMask, region: PageRegion) -> Version {
        if region == PageRegion::WHOLE {
            return self.channel_version(channels);
        }
        let geometry = channels & RIM_CHANNELS;
        let other = ChannelMask(channels.0 & !RIM_CHANNELS.0);
        let rim = if geometry.is_empty() {
            Version(0)
        } else {
            self.rim_versions[region.index()]
        };
        rim.max(self.channel_version(other))
    }
    /// Restore starts a fresh derived-cache epoch; old fine-grained border
    /// history is neither persisted nor required to recover logical state.
    pub(crate) fn reset_rim_versions(&mut self) {
        let geometry = self.channel_version(RIM_CHANNELS);
        self.rim_versions.fill(geometry);
    }
    fn stamp_rim_cell(&mut self, index: LocalIdx, version: Version) -> usize {
        let mut bits = PageRegion::cell_boundary_mask(index);
        let mut updated = 0;
        while bits != 0 {
            let region = bits.trailing_zeros() as usize;
            if self.rim_versions[region] != version {
                self.rim_versions[region] = version;
                updated += 1;
            }
            bits &= bits - 1;
        }
        updated
    }
    pub fn light(&self, index: LocalIdx) -> LightSample {
        self.lights
            .as_ref()
            .map_or(LightSample::default(), |field| {
                field.0[usize::from(index.get())]
            })
    }
    pub fn component(&self, index: LocalIdx) -> Option<ComponentId> {
        self.components.get(&index).copied()
    }
    pub fn component_links(&self) -> impl Iterator<Item = (LocalIdx, ComponentId)> + '_ {
        self.components.iter().map(|(index, id)| (*index, *id))
    }
    pub fn classified_cells(&self) -> u64 {
        self.classified_cells
    }
    fn set_cell(&mut self, index: LocalIdx, value: Cell, registry: &dyn Registry) {
        if self.cells.set(index, value) != value {
            self.masks.update(index, registry.properties(value));
            self.classified_cells = self.classified_cells.saturating_add(1);
        }
    }
    fn set_light(&mut self, index: LocalIdx, value: LightSample) {
        let field = self
            .lights
            .get_or_insert_with(|| LightField(Box::new([LightSample::default(); PAGE_VOLUME])));
        field.0[usize::from(index.get())] = value;
    }
    fn stamp(&mut self, version: Version, channels: ChannelMask) {
        self.version = version;
        for (bit, stamp) in self.channel_versions.iter_mut().enumerate() {
            if channels.0 & (1 << bit) != 0 {
                *stamp = version;
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComponentData {
    pub anchor: BlockPos,
    pub kind: u16,
    pub payload: Vec<u8>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComponentRecord {
    pub data: ComponentData,
    pub version: Version,
}
#[derive(Clone, Debug)]
pub struct ComponentChange {
    pub id: ComponentId,
    pub before: Option<ComponentRecord>,
    pub after: Option<ComponentRecord>,
    pub channels: ChannelMask,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GameplayEvent {
    pub kind: u16,
    pub subject: Option<ComponentId>,
    pub position: Option<BlockPos>,
    pub payload: Vec<u8>,
}
#[derive(Clone, Debug)]
pub struct CellChange {
    pub page: PageId,
    pub index: LocalIdx,
    pub old: Cell,
    pub new: Cell,
}
#[derive(Clone, Debug)]
pub struct LightChange {
    pub page: PageId,
    pub index: LocalIdx,
    pub old: LightSample,
    pub new: LightSample,
}
#[derive(Clone, Debug)]
pub struct PageChange {
    pub page: PageId,
    pub before: Version,
    pub after: Version,
    pub channels: ChannelMask,
    pub created: bool,
    pub removed: bool,
    pub initial_cells: Option<Box<[Cell; PAGE_VOLUME]>>,
    pub removed_cells: Option<Box<[Cell; PAGE_VOLUME]>>,
}
#[derive(Clone, Debug)]
pub struct SpaceChange {
    pub before: Option<SpaceTransform>,
    pub after: SpaceTransform,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CommitWork {
    /// Only these pages were staged; the remaining world was not traversed.
    pub pages_staged: usize,
    pub component_records_staged: usize,
    /// Cells whose six page masks were classified, excluding channel comparisons.
    pub mask_cells_classified: u64,
    /// Registry calls including old/new channel comparisons.
    pub registry_queries: u64,
    /// Existing-page boundary stamps updated; new-page initialization excluded.
    pub rim_stamps_updated: usize,
}
#[derive(Clone, Debug)]
pub struct WorldChange {
    pub tx: TxId,
    pub tick: Tick,
    pub space: SpaceId,
    pub pages: Vec<PageChange>,
    pub cells: Vec<CellChange>,
    pub lights: Vec<LightChange>,
    pub components: Vec<ComponentChange>,
    pub events: Vec<GameplayEvent>,
    pub transform: Option<SpaceChange>,
    pub channels: ChannelMask,
    pub work: CommitWork,
}
impl WorldChange {
    pub fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.pages.len() * std::mem::size_of::<PageChange>()
            + self.cells.len() * std::mem::size_of::<CellChange>()
            + self.lights.len() * std::mem::size_of::<LightChange>()
            + self.components.len() * std::mem::size_of::<ComponentChange>()
            + self.events.len() * std::mem::size_of::<GameplayEvent>()
            + self
                .pages
                .iter()
                .map(|p| {
                    (usize::from(p.initial_cells.is_some())
                        + usize::from(p.removed_cells.is_some()))
                        * PAGE_VOLUME
                        * 4
                })
                .sum::<usize>()
            + self
                .components
                .iter()
                .flat_map(|c| [&c.before, &c.after])
                .filter_map(|c| c.as_ref())
                .map(|c| c.data.payload.len())
                .sum::<usize>()
            + self
                .events
                .iter()
                .map(|event| event.payload.len())
                .sum::<usize>()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageInput {
    pub page: PageId,
    pub channels: ChannelMask,
    pub version: Version,
}

const RIM_CHANNELS: ChannelMask = ChannelMask(ChannelMask::SHAPE.0 | ChannelMask::OPACITY.0);

/// A source-page region sampled by a consumer. A negative component selects
/// coordinate0, a positive one selects15; zero leaves that axis unconstrained.
/// Thus [-1,0,0] is the minimum-X plane and [-1,1,1] is one corner cell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct PageRegion([i8; 3]);
impl PageRegion {
    pub const WHOLE: Self = Self([0; 3]);
    pub const fn new(coordinates: [i8; 3]) -> Option<Self> {
        let [x, y, z] = coordinates;
        if x < -1 || x > 1 || y < -1 || y > 1 || z < -1 || z > 1 {
            None
        } else {
            Some(Self(coordinates))
        }
    }
    pub const fn coordinates(self) -> [i8; 3] {
        self.0
    }
    const fn index(self) -> usize {
        (self.0[0] + 1) as usize + 3 * (self.0[2] + 1) as usize + 9 * (self.0[1] + 1) as usize
    }
    fn cell_boundary_mask(index: LocalIdx) -> u32 {
        let (x, y, z) = index.xyz();
        let axes = |coordinate| match coordinate {
            0 => [Some(0), Some(-1)],
            15 => [Some(0), Some(1)],
            _ => [Some(0), None],
        };
        let mut mask = 0;
        for x in axes(x).into_iter().flatten() {
            for y in axes(y).into_iter().flatten() {
                for z in axes(z).into_iter().flatten() {
                    let region = Self([x, y, z]);
                    if region != Self::WHOLE {
                        mask |= 1 << region.index();
                    }
                }
            }
        }
        mask
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SampledPageInput {
    pub page: PageId,
    pub channels: ChannelMask,
    pub region: PageRegion,
    pub version: Version,
}

/// Both whole-page consumers and sparse/halo consumers can validate snapshots
/// through the same API without weakening the existing PageInput contract.
pub trait SnapshotInput {
    fn matches(&self, world: &World) -> bool;
}
impl SnapshotInput for PageInput {
    fn matches(&self, world: &World) -> bool {
        world.input_version(self.page, self.channels) == self.version
    }
}
impl SnapshotInput for SampledPageInput {
    fn matches(&self, world: &World) -> bool {
        world.sampled_input_version(self.page, self.channels, self.region) == self.version
    }
}
#[derive(Clone, Copy, Debug)]
pub struct RegionSummary {
    pub loaded_pages: usize,
    pub version: Version,
}
#[derive(Clone, Copy, Debug)]
pub struct SpaceRecord {
    pub transform: SpaceTransform,
    pub version: Version,
}

struct ComponentEdit {
    value: Option<ComponentData>,
    channels: ChannelMask,
}
/// A transaction is assembled off-world and validated completely before publish.
pub struct Transaction {
    tick: Tick,
    space: SpaceId,
    expected: BTreeMap<PageId, Version>,
    create: BTreeMap<PageId, Box<[Cell]>>,
    duplicate_create: bool,
    remove: BTreeSet<PageId>,
    cells: Vec<(BlockPos, Cell)>,
    lights: Vec<(BlockPos, LightSample)>,
    components: BTreeMap<ComponentId, ComponentEdit>,
    events: Vec<GameplayEvent>,
    create_space: Option<SpaceTransform>,
    transform: Option<SpaceTransform>,
}
impl Transaction {
    /// Read-only resource preflight for a bounded client cache.
    pub fn created_pages(&self) -> impl ExactSizeIterator<Item = PageId> + '_ {
        self.create.keys().copied()
    }
    pub fn removed_pages(&self) -> impl ExactSizeIterator<Item = PageId> + '_ {
        self.remove.iter().copied()
    }
    pub fn created_page_data(&self) -> impl Iterator<Item = (PageId, &[Cell])> {
        self.create
            .iter()
            .map(|(page, values)| (*page, values.as_ref()))
    }
    pub fn cell_updates(&self) -> impl Iterator<Item = (BlockPos, Cell)> + '_ {
        self.cells.iter().copied()
    }
    pub fn component_updates(
        &self,
    ) -> impl Iterator<Item = (ComponentId, Option<&ComponentData>, ChannelMask)> {
        self.components
            .iter()
            .map(|(id, edit)| (*id, edit.value.as_ref(), edit.channels))
    }
    pub fn new(tick: Tick, space: SpaceId) -> Self {
        Self {
            tick,
            space,
            expected: BTreeMap::new(),
            create: BTreeMap::new(),
            duplicate_create: false,
            remove: BTreeSet::new(),
            cells: Vec::new(),
            lights: Vec::new(),
            components: BTreeMap::new(),
            events: Vec::new(),
            create_space: None,
            transform: None,
        }
    }
    pub fn expect_page(&mut self, page: PageId, version: Version) -> &mut Self {
        self.expected.insert(page, version);
        self
    }
    pub fn create_page(&mut self, page: PageId, cells: impl Into<Box<[Cell]>>) -> &mut Self {
        if self.create.insert(page, cells.into()).is_some() {
            self.duplicate_create = true;
        }
        self
    }
    pub fn remove_page(&mut self, page: PageId) -> &mut Self {
        self.remove.insert(page);
        self
    }
    pub fn set_cell(&mut self, position: BlockPos, value: Cell) -> &mut Self {
        self.cells.push((position, value));
        self
    }
    pub fn set_light(&mut self, position: BlockPos, value: LightSample) -> &mut Self {
        self.lights.push((position, value));
        self
    }
    pub fn set_component(
        &mut self,
        id: ComponentId,
        value: ComponentData,
        channels: ChannelMask,
    ) -> &mut Self {
        let channels = self
            .components
            .get(&id)
            .map_or(channels, |edit| edit.channels | channels);
        self.components.insert(
            id,
            ComponentEdit {
                value: Some(value),
                channels,
            },
        );
        self
    }
    pub fn remove_component(&mut self, id: ComponentId, channels: ChannelMask) -> &mut Self {
        let channels = self
            .components
            .get(&id)
            .map_or(channels, |edit| edit.channels | channels);
        self.components.insert(
            id,
            ComponentEdit {
                value: None,
                channels,
            },
        );
        self
    }
    pub fn event(&mut self, event: GameplayEvent) -> &mut Self {
        self.events.push(event);
        self
    }
    pub fn create_space(&mut self, transform: SpaceTransform) -> &mut Self {
        self.create_space = Some(transform);
        self
    }
    pub fn set_transform(&mut self, transform: SpaceTransform) -> &mut Self {
        self.transform = Some(transform);
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommitError {
    WrongSpace,
    UnknownSpace,
    SpaceExists,
    InvalidTransform,
    TickWentBackwards,
    DuplicatePage,
    WrongPageLength,
    PageAlreadyLoaded(PageId),
    UnknownPage(PageId),
    ConflictingVersion {
        page: PageId,
        expected: Version,
        actual: Version,
    },
    PageCreatedAndRemoved(PageId),
    ComponentAnchorOccupied(BlockPos),
    UnknownComponent(ComponentId),
    DanglingComponent(ComponentId),
    InvalidLight,
    InvalidChannels,
    ChangeTooLarge,
    VersionExhausted,
}
impl std::fmt::Display for CommitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "world transaction rejected: {self:?}")
    }
}
impl std::error::Error for CommitError {}

pub struct World {
    registry: Arc<dyn Registry>,
    pages: BTreeMap<PageId, Arc<Page>>,
    versions: BTreeMap<PageId, Version>,
    components: BTreeMap<ComponentId, ComponentRecord>,
    spaces: BTreeMap<SpaceId, SpaceRecord>,
    regions: BTreeMap<RegionId, RegionSummary>,
    tick: Tick,
    version: Version,
    next_tx: u64,
    journal: Journal,
}
impl World {
    pub fn new(registry: impl Registry + 'static, journal_capacity: usize) -> Self {
        Self::with_limits(
            registry,
            JournalLimits {
                max_records: journal_capacity,
                ..JournalLimits::default()
            },
        )
    }
    pub fn with_limits(registry: impl Registry + 'static, limits: JournalLimits) -> Self {
        let mut spaces = BTreeMap::new();
        spaces.insert(
            SpaceId(0),
            SpaceRecord {
                transform: SpaceTransform::IDENTITY,
                version: Version(0),
            },
        );
        Self {
            registry: Arc::new(registry),
            pages: BTreeMap::new(),
            versions: BTreeMap::new(),
            components: BTreeMap::new(),
            spaces,
            regions: BTreeMap::new(),
            tick: Tick(0),
            version: Version(0),
            next_tx: 1,
            journal: Journal::new(limits),
        }
    }
    pub fn read(&self, position: BlockPos) -> CellRead {
        self.pages
            .get(&position.page())
            .map_or(CellRead::Unknown, |page| {
                CellRead::Known(page.cell(position.local_index()))
            })
    }
    pub fn page(&self, page: PageId) -> Option<&Page> {
        self.pages.get(&page).map(Arc::as_ref)
    }
    pub fn snapshot(&self, page: PageId) -> Option<Arc<Page>> {
        self.pages.get(&page).cloned()
    }
    pub fn page_version(&self, page: PageId) -> Version {
        self.versions.get(&page).copied().unwrap_or(Version(0))
    }
    pub fn input_version(&self, page: PageId, channels: ChannelMask) -> Version {
        self.page(page).map_or_else(
            || self.page_version(page),
            |page| page.channel_version(channels),
        )
    }
    pub fn capture_input(&self, page: PageId, channels: ChannelMask) -> PageInput {
        PageInput {
            page,
            channels,
            version: self.input_version(page, channels),
        }
    }
    pub fn sampled_input_version(
        &self,
        page: PageId,
        channels: ChannelMask,
        region: PageRegion,
    ) -> Version {
        self.page(page).map_or_else(
            || self.page_version(page),
            |page| page.sampled_version(channels, region),
        )
    }
    pub fn capture_sampled_input(
        &self,
        page: PageId,
        channels: ChannelMask,
        region: PageRegion,
    ) -> SampledPageInput {
        SampledPageInput {
            page,
            channels,
            region,
            version: self.sampled_input_version(page, channels, region),
        }
    }
    pub fn matches_inputs<I: SnapshotInput>(&self, inputs: &[I]) -> bool {
        inputs.iter().all(|input| input.matches(self))
    }
    pub fn matches_versions(&self, inputs: &[(PageId, Version)]) -> bool {
        inputs
            .iter()
            .all(|(page, version)| self.page_version(*page) == *version)
    }
    pub fn component(&self, id: ComponentId) -> Option<&ComponentRecord> {
        self.components.get(&id)
    }
    pub fn components(&self) -> impl Iterator<Item = (ComponentId, &ComponentRecord)> {
        self.components.iter().map(|(id, record)| (*id, record))
    }
    pub fn spaces(&self) -> impl Iterator<Item = (SpaceId, SpaceRecord)> + '_ {
        self.spaces.iter().map(|(id, record)| (*id, *record))
    }
    pub fn page_versions(&self) -> impl Iterator<Item = (PageId, Version)> + '_ {
        self.versions.iter().map(|(id, version)| (*id, *version))
    }
    pub fn space(&self, id: SpaceId) -> Option<SpaceRecord> {
        self.spaces.get(&id).copied()
    }
    pub fn region(&self, id: RegionId) -> Option<RegionSummary> {
        self.regions.get(&id).copied()
    }
    pub fn tick(&self) -> Tick {
        self.tick
    }
    pub fn version(&self) -> Version {
        self.version
    }
    pub fn loaded_pages(&self) -> usize {
        self.pages.len()
    }
    pub fn pages(&self) -> impl Iterator<Item = (PageId, &Page)> {
        self.pages.iter().map(|(id, page)| (*id, page.as_ref()))
    }
    pub fn journal_next(&self) -> u64 {
        self.journal.next()
    }
    pub fn journal_oldest(&self) -> u64 {
        self.journal.oldest()
    }
    pub fn journal_len(&self) -> usize {
        self.journal.len()
    }
    pub fn journal_bytes(&self) -> usize {
        self.journal.bytes()
    }
    pub fn register_cursor(&mut self) -> CursorId {
        self.journal
            .register(self.journal.next())
            .expect("current boundary")
    }
    pub fn register_cursor_from(&mut self, sequence: u64) -> Result<CursorId, CursorError> {
        self.journal.register(sequence)
    }
    pub fn remove_cursor(&mut self, cursor: CursorId) -> bool {
        self.journal.unregister(cursor)
    }
    pub fn read_cursor(&self, cursor: CursorId, limit: usize) -> Result<JournalBatch, CursorError> {
        self.journal.read(cursor, limit)
    }
    pub fn acknowledge_cursor(&mut self, cursor: CursorId, next: u64) -> Result<(), CursorError> {
        self.journal.acknowledge(cursor, next)
    }
    pub fn recover_cursor(&mut self, cursor: CursorId, next: u64) -> Result<(), CursorError> {
        self.journal.recover(cursor, next)
    }

    pub fn commit(&mut self, tx: Transaction) -> Result<WorldChange, CommitError> {
        if tx.tick < self.tick {
            return Err(CommitError::TickWentBackwards);
        }
        let revision = Version(
            self.version
                .0
                .checked_add(1)
                .ok_or(CommitError::VersionExhausted)?,
        );
        let next_tx = self
            .next_tx
            .checked_add(1)
            .ok_or(CommitError::VersionExhausted)?;
        if tx.duplicate_create {
            return Err(CommitError::DuplicatePage);
        }
        if tx.create_space.is_some() && self.spaces.contains_key(&tx.space) {
            return Err(CommitError::SpaceExists);
        }
        if tx.create_space.is_none() && !self.spaces.contains_key(&tx.space) {
            return Err(CommitError::UnknownSpace);
        }
        if tx
            .create_space
            .into_iter()
            .chain(tx.transform)
            .any(|transform| !transform.is_valid())
        {
            return Err(CommitError::InvalidTransform);
        }
        for (page, expected) in &tx.expected {
            if page.space != tx.space {
                return Err(CommitError::WrongSpace);
            }
            let actual = self.page_version(*page);
            if actual != *expected {
                return Err(CommitError::ConflictingVersion {
                    page: *page,
                    expected: *expected,
                    actual,
                });
            }
        }
        let mut touched = BTreeSet::new();
        for (page, values) in &tx.create {
            if page.space != tx.space {
                return Err(CommitError::WrongSpace);
            }
            if values.len() != PAGE_VOLUME {
                return Err(CommitError::WrongPageLength);
            }
            if self.pages.contains_key(page) {
                return Err(CommitError::PageAlreadyLoaded(*page));
            }
            if tx.remove.contains(page) {
                return Err(CommitError::PageCreatedAndRemoved(*page));
            }
            touched.insert(*page);
        }
        for page in &tx.remove {
            if page.space != tx.space {
                return Err(CommitError::WrongSpace);
            }
            if !self.pages.contains_key(page) {
                return Err(CommitError::UnknownPage(*page));
            }
            touched.insert(*page);
        }
        for (position, _) in &tx.cells {
            if position.space != tx.space {
                return Err(CommitError::WrongSpace);
            }
            touched.insert(position.page());
        }
        for (position, light) in &tx.lights {
            if position.space != tx.space {
                return Err(CommitError::WrongSpace);
            }
            if light.sky > 15 || light.block > 15 {
                return Err(CommitError::InvalidLight);
            }
            touched.insert(position.page());
        }
        for (id, edit) in &tx.components {
            if edit.channels.0 & !ChannelMask::ALL.0 != 0 {
                return Err(CommitError::InvalidChannels);
            }
            if let Some(previous) = self.components.get(id) {
                if previous.data.anchor.space != tx.space {
                    return Err(CommitError::WrongSpace);
                }
                touched.insert(previous.data.anchor.page());
            } else if edit.value.is_none() {
                return Err(CommitError::UnknownComponent(*id));
            }
            if let Some(value) = &edit.value {
                if value.anchor.space != tx.space {
                    return Err(CommitError::WrongSpace);
                }
                touched.insert(value.anchor.page());
            }
        }
        if tx
            .events
            .iter()
            .filter_map(|event| event.position)
            .any(|position| position.space != tx.space)
        {
            return Err(CommitError::WrongSpace);
        }
        // Staging is page-local: no clone or walk of the remaining world occurs.
        let mut staged: BTreeMap<PageId, Option<Page>> = touched
            .iter()
            .map(|id| (*id, self.pages.get(id).map(|page| page.as_ref().clone())))
            .collect();
        for (page, values) in tx.create {
            let values: Box<[Cell; PAGE_VOLUME]> = values
                .try_into()
                .map_err(|_| CommitError::WrongPageLength)?;
            staged.insert(page, Some(Page::new(values, self.registry.as_ref())));
        }
        let mut cell_keys = BTreeSet::new();
        for (position, value) in tx.cells {
            let page = staged
                .get_mut(&position.page())
                .and_then(Option::as_mut)
                .ok_or(CommitError::UnknownPage(position.page()))?;
            page.set_cell(position.local_index(), value, self.registry.as_ref());
            cell_keys.insert((position.page(), position.local_index()));
        }
        let mut light_keys = BTreeSet::new();
        for (position, light) in tx.lights {
            let page = staged
                .get_mut(&position.page())
                .and_then(Option::as_mut)
                .ok_or(CommitError::UnknownPage(position.page()))?;
            page.set_light(position.local_index(), light);
            light_keys.insert((position.page(), position.local_index()));
        }
        let component_records_staged = tx.components.len();
        let mut component_changes = Vec::new();
        let mut page_channels = BTreeMap::<PageId, ChannelMask>::new();
        let mut rim_stamps_updated = 0;
        // Remove old links first so moving/swapping components in one transaction
        // is independent of the numeric ordering of stable component IDs.
        for id in tx.components.keys() {
            if let Some(previous) = self.components.get(id) {
                let anchor = previous.data.anchor;
                let page = staged
                    .get_mut(&anchor.page())
                    .and_then(Option::as_mut)
                    .ok_or(CommitError::UnknownPage(anchor.page()))?;
                page.components.remove(&anchor.local_index());
            }
        }
        for (id, edit) in tx.components {
            let before = self.components.get(&id).cloned();
            let after = if let Some(value) = edit.value {
                let page = staged
                    .get_mut(&value.anchor.page())
                    .and_then(Option::as_mut)
                    .ok_or(CommitError::UnknownPage(value.anchor.page()))?;
                if page
                    .components
                    .insert(value.anchor.local_index(), id)
                    .is_some()
                {
                    return Err(CommitError::ComponentAnchorOccupied(value.anchor));
                }
                Some(ComponentRecord {
                    data: value,
                    version: revision,
                })
            } else {
                None
            };
            let differs = before.as_ref().map(|c| &c.data) != after.as_ref().map(|c| &c.data);
            if differs {
                let channels = if edit.channels.is_empty() {
                    ChannelMask::CONTENTS
                } else {
                    edit.channels & ChannelMask::ALL
                };
                for record in before.iter().chain(after.iter()) {
                    *page_channels.entry(record.data.anchor.page()).or_default() |= channels;
                    if channels.intersects(RIM_CHANNELS)
                        && self.pages.contains_key(&record.data.anchor.page())
                        && let Some(page) = staged
                            .get_mut(&record.data.anchor.page())
                            .and_then(Option::as_mut)
                    {
                        rim_stamps_updated +=
                            page.stamp_rim_cell(record.data.anchor.local_index(), revision);
                    }
                }
                component_changes.push(ComponentChange {
                    id,
                    before,
                    after,
                    channels,
                });
            }
        }
        let mask_cells_classified: u64 = staged
            .iter()
            .map(|(id, page)| {
                page.as_ref().map_or(0, |page| {
                    page.classified_cells
                        .saturating_sub(self.pages.get(id).map_or(0, |old| old.classified_cells))
                })
            })
            .sum();
        for id in &tx.remove {
            let page = staged
                .get(id)
                .and_then(Option::as_ref)
                .ok_or(CommitError::UnknownPage(*id))?;
            if let Some(component) = page.components.values().next() {
                return Err(CommitError::DanglingComponent(*component));
            }
            for index in 0..PAGE_VOLUME {
                let index = LocalIdx::new(index as u16).expect("page index");
                if page.light(index) != LightSample::default() {
                    light_keys.insert((*id, index));
                }
            }
            staged.insert(*id, None);
        }
        let mut change = WorldChange {
            tx: TxId(self.next_tx),
            tick: tx.tick,
            space: tx.space,
            pages: Vec::new(),
            cells: Vec::new(),
            lights: Vec::new(),
            components: component_changes,
            events: tx.events,
            transform: None,
            channels: ChannelMask::NONE,
            work: CommitWork {
                pages_staged: touched.len(),
                component_records_staged,
                mask_cells_classified,
                registry_queries: mask_cells_classified,
                rim_stamps_updated,
            },
        };
        for (id, index) in cell_keys {
            // Created/removed pages carry complete cell payloads instead.
            let Some(old_page) = self.pages.get(&id) else {
                continue;
            };
            let Some(new_page) = staged.get(&id).and_then(Option::as_ref) else {
                continue;
            };
            let old = old_page.cell(index);
            let new = new_page.cell(index);
            if old != new {
                let channels = self
                    .registry
                    .properties(old)
                    .changed_channels(self.registry.properties(new));
                *page_channels.entry(id).or_default() |= channels;
                if channels.intersects(RIM_CHANNELS) {
                    change.work.rim_stamps_updated += staged
                        .get_mut(&id)
                        .and_then(Option::as_mut)
                        .expect("changed page remains staged")
                        .stamp_rim_cell(index, revision);
                }
                change.cells.push(CellChange {
                    page: id,
                    index,
                    old,
                    new,
                });
            }
        }
        change.work.registry_queries += change.cells.len() as u64 * 2;
        let changed_cell_pages: BTreeSet<_> = change.cells.iter().map(|cell| cell.page).collect();
        for (id, index) in light_keys {
            let old = self
                .pages
                .get(&id)
                .map_or(LightSample::default(), |page| page.light(index));
            let new = staged
                .get(&id)
                .and_then(Option::as_ref)
                .map_or(LightSample::default(), |page| page.light(index));
            if old != new {
                *page_channels.entry(id).or_default() |= ChannelMask::LIGHTING;
                change.lights.push(LightChange {
                    page: id,
                    index,
                    old,
                    new,
                });
            }
        }
        for id in &touched {
            let before = self.pages.get(id);
            let after = staged.get(id).and_then(Option::as_ref);
            let created = before.is_none() && after.is_some();
            let removed = before.is_some() && after.is_none();
            let channels = if created || removed {
                ChannelMask::ALL
            } else {
                page_channels.get(id).copied().unwrap_or_default()
            };
            // Raw cell bits may change with no derived channel (e.g. a benign
            // gameplay state). Preserve the cell mutation and page base version.
            let cell_changed = changed_cell_pages.contains(id);
            if created || removed || !channels.is_empty() || cell_changed {
                if let Some(page) = staged.get_mut(id).and_then(Option::as_mut) {
                    page.stamp(revision, channels);
                    if created {
                        page.reset_rim_versions();
                    }
                }
                change.pages.push(PageChange {
                    page: *id,
                    before: self.page_version(*id),
                    after: revision,
                    channels,
                    created,
                    removed,
                    initial_cells: if created {
                        staged.get(id).and_then(Option::as_ref).map(Page::cells)
                    } else {
                        None
                    },
                    removed_cells: if removed {
                        before.map(|page| page.cells())
                    } else {
                        None
                    },
                });
                change.channels |= channels;
            }
        }
        let before_transform = self.spaces.get(&tx.space).map(|space| space.transform);
        let requested_transform = tx.transform.or(tx.create_space);
        if let Some(after) = requested_transform
            && before_transform != Some(after)
        {
            change.transform = Some(SpaceChange {
                before: before_transform,
                after,
            });
            change.channels |= ChannelMask::TRANSFORM;
        }
        if !change.events.is_empty() {
            change.channels |= ChannelMask::EVENTS;
        }
        if !self.journal.can_retain(change.retained_bytes()) {
            return Err(CommitError::ChangeTooLarge);
        }
        // Everything that can return an error has completed. Publication is one
        // deterministic owner-thread phase, touching only the staged records.
        for page_change in &change.pages {
            match staged.remove(&page_change.page).flatten() {
                Some(page) => {
                    self.pages.insert(page_change.page, Arc::new(page));
                }
                None => {
                    self.pages.remove(&page_change.page);
                }
            }
            self.versions.insert(page_change.page, revision);
            let region = self
                .regions
                .entry(RegionId::from_page(page_change.page))
                .or_insert(RegionSummary {
                    loaded_pages: 0,
                    version: revision,
                });
            if page_change.created {
                region.loaded_pages += 1;
            }
            if page_change.removed {
                region.loaded_pages -= 1;
            }
            region.version = revision;
        }
        for component in &change.components {
            if let Some(value) = &component.after {
                self.components.insert(component.id, value.clone());
            } else {
                self.components.remove(&component.id);
            }
        }
        if let Some(transform) = &change.transform {
            self.spaces.insert(
                tx.space,
                SpaceRecord {
                    transform: transform.after,
                    version: revision,
                },
            );
        }
        self.tick = tx.tick;
        self.version = revision;
        self.next_tx = next_tx;
        self.journal.prune(self.tick);
        self.journal.push(change.clone());
        Ok(change)
    }

    /// Diagnostic hash of logical state in fixed little-endian order. No hash
    /// map iteration order, native pointer, packed representation or cache work
    /// counter is included. This is not a cryptographic authenticity checksum.
    pub fn canonical_hash(&self) -> u64 {
        struct Hash(u64);
        impl Hash {
            fn bytes(&mut self, bytes: &[u8]) {
                for byte in bytes {
                    self.0 ^= u64::from(*byte);
                    self.0 = self.0.wrapping_mul(0x100000001b3);
                }
            }
            fn u64(&mut self, n: u64) {
                self.bytes(&n.to_le_bytes());
            }
            fn u32(&mut self, n: u32) {
                self.bytes(&n.to_le_bytes());
            }
            fn position(&mut self, p: BlockPos) {
                self.u32(p.space.0);
                self.u32(p.x as u32);
                self.u32(p.y as u32);
                self.u32(p.z as u32);
            }
            fn page(&mut self, p: PageId) {
                self.u32(p.space.0);
                self.u32(p.x as u32);
                self.u32(p.y as u32);
                self.u32(p.z as u32);
            }
        }
        let mut hash = Hash(0xcbf29ce484222325);
        hash.bytes(b"cubi-world-state-v1");
        hash.u64(self.tick.0);
        hash.u64(self.version.0);
        hash.u64(self.next_tx);
        hash.u64(self.spaces.len() as u64);
        for (id, record) in &self.spaces {
            hash.u32(id.0);
            hash.u64(record.version.0);
            for value in record
                .transform
                .translation
                .into_iter()
                .chain(record.transform.rotation)
            {
                hash.u64(if value == 0.0 { 0 } else { value.to_bits() });
            }
        }
        hash.u64(self.versions.len() as u64);
        for (id, version) in &self.versions {
            hash.page(*id);
            hash.u64(version.0);
            hash.bytes(&[u8::from(self.pages.contains_key(id))]);
        }
        for (id, page) in &self.pages {
            hash.page(*id);
            for index in 0..PAGE_VOLUME {
                let index = LocalIdx::new(index as u16).expect("page index");
                hash.u32(page.cell(index).raw());
                let light = page.light(index);
                hash.bytes(&[
                    light.sky,
                    light.block,
                    light.rgb[0],
                    light.rgb[1],
                    light.rgb[2],
                ]);
            }
            for version in &page.channel_versions {
                hash.u64(version.0);
            }
        }
        hash.u64(self.components.len() as u64);
        for (id, component) in &self.components {
            hash.u64(id.0);
            hash.u64(component.version.0);
            hash.position(component.data.anchor);
            hash.bytes(&component.data.kind.to_le_bytes());
            hash.u64(component.data.payload.len() as u64);
            hash.bytes(&component.data.payload);
        }
        hash.0
    }
}
