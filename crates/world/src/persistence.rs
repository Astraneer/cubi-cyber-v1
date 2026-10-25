//! Snapshot/recovery bridge. Capture shares page Arcs; flattening and mask rebuild
//! happen in the save/recovery worker, never by scanning cells during capture.
//! Journals/cursors are instance-local. WAL replay requires the same rules version;
//! schema/rules/generator migrations belong to the enclosing save format.

use crate::{
    CommitError, ComponentRecord, JournalLimits, LightField, LightSample, Page, RegionSummary,
    Registry, SpaceRecord, Transaction, World, WorldChange, journal::Journal,
};
use core_types::{
    Cell, ComponentId, LocalIdx, PAGE_VOLUME, PageId, RegionId, SpaceId, SpaceTransform, Tick,
    Version,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

pub const PERSISTENCE_SCHEMA: u32 = 1;

#[derive(Clone, Debug)]
pub struct PersistedPage {
    pub id: PageId,
    pub version: Version,
    pub channel_versions: [Version; 10],
    pub cells: Box<[Cell; PAGE_VOLUME]>,
    pub lights: Option<Box<[LightSample; PAGE_VOLUME]>>,
    pub components: Vec<(LocalIdx, ComponentId)>,
}

#[derive(Clone, Debug)]
pub struct RestoreState {
    pub schema: u32,
    pub tick: Tick,
    pub version: Version,
    pub next_tx: u64,
    pub pages: Vec<PersistedPage>,
    pub tombstones: Vec<(PageId, Version)>,
    pub components: Vec<(ComponentId, ComponentRecord)>,
    pub spaces: Vec<(SpaceId, SpaceRecord)>,
    pub regions: Vec<(RegionId, RegionSummary)>,
}

/// Codec limits must be checked before allocating untrusted vectors. These
/// additional bounds defend programmatic RestoreState construction as well.
#[derive(Clone, Copy, Debug)]
pub struct RestoreBounds {
    pub max_pages: usize,
    pub max_tombstones: usize,
    pub max_components: usize,
    pub max_component_bytes: usize,
    pub max_spaces: usize,
    pub max_regions: usize,
}
impl Default for RestoreBounds {
    fn default() -> Self {
        Self {
            max_pages: 65_536,
            max_tombstones: 1_000_000,
            max_components: 1_000_000,
            max_component_bytes: 256 * 1024 * 1024,
            max_spaces: 65_536,
            max_regions: 65_536,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RestoreError {
    Schema(u32),
    InvalidHeader,
    InvalidJournalLimits,
    Bounds(&'static str),
    DuplicatePage(PageId),
    DuplicateTombstone(PageId),
    LoadedTombstone(PageId),
    DuplicateComponent(ComponentId),
    DuplicateSpace(SpaceId),
    DuplicateRegion(RegionId),
    MissingGroundSpace,
    MissingSpace(SpaceId),
    InvalidSpace(SpaceId),
    InvalidPageVersion(PageId),
    InvalidTombstoneVersion(PageId),
    InvalidChannelVersion(PageId),
    InvalidLight {
        page: PageId,
        index: LocalIdx,
    },
    InvalidComponent(ComponentId),
    DuplicateAnchor(ComponentId),
    InvalidComponentLink {
        page: PageId,
        component: ComponentId,
    },
    InvalidRegion(RegionId),
    MissingRegion(RegionId),
}
impl std::fmt::Display for RestoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "world restore rejected: {self:?}")
    }
}
impl std::error::Error for RestoreError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReplayError {
    TransactionOrder { expected: u64, actual: u64 },
    Malformed(&'static str),
    MetadataMismatch,
    Commit(CommitError),
}
impl std::fmt::Display for ReplayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "world WAL replay rejected: {self:?}")
    }
}
impl std::error::Error for ReplayError {}
impl From<CommitError> for ReplayError {
    fn from(value: CommitError) -> Self {
        Self::Commit(value)
    }
}

/// Immutable checkpoint: cells/light arrays remain shared until a worker flattens
/// them. Component records currently contain Vec payloads and are copied at
/// capture; page data is never copied or classified on the capture path.
#[derive(Clone, Debug)]
pub struct Checkpoint {
    tick: Tick,
    version: Version,
    next_tx: u64,
    boundary: u64,
    pages: BTreeMap<PageId, Arc<Page>>,
    versions: BTreeMap<PageId, Version>,
    components: BTreeMap<ComponentId, ComponentRecord>,
    spaces: BTreeMap<SpaceId, SpaceRecord>,
    regions: BTreeMap<RegionId, RegionSummary>,
}
impl Checkpoint {
    pub fn journal_boundary(&self) -> u64 {
        self.boundary
    }
    pub fn tick(&self) -> Tick {
        self.tick
    }
    pub fn version(&self) -> Version {
        self.version
    }
    pub fn next_tx(&self) -> u64 {
        self.next_tx
    }
    pub fn loaded_pages(&self) -> usize {
        self.pages.len()
    }
    pub fn page_snapshot(&self, id: PageId) -> Option<&Arc<Page>> {
        self.pages.get(&id)
    }
    /// Call this outside the foreground owner phase. It visits every saved cell.
    pub fn to_restore_state(&self) -> RestoreState {
        RestoreState {
            schema: PERSISTENCE_SCHEMA,
            tick: self.tick,
            version: self.version,
            next_tx: self.next_tx,
            pages: self
                .pages
                .iter()
                .map(|(id, page)| PersistedPage {
                    id: *id,
                    version: page.version,
                    channel_versions: page.channel_versions,
                    cells: page.cells(),
                    lights: page.lights.as_ref().map(|field| field.0.clone()),
                    components: page
                        .components
                        .iter()
                        .map(|(index, id)| (*index, *id))
                        .collect(),
                })
                .collect(),
            tombstones: self
                .versions
                .iter()
                .filter(|(id, _)| !self.pages.contains_key(id))
                .map(|(id, version)| (*id, *version))
                .collect(),
            components: self
                .components
                .iter()
                .map(|(id, record)| (*id, record.clone()))
                .collect(),
            spaces: self
                .spaces
                .iter()
                .map(|(id, record)| (*id, *record))
                .collect(),
            regions: self
                .regions
                .iter()
                .map(|(id, record)| (*id, *record))
                .collect(),
        }
    }
}

impl World {
    pub fn checkpoint(&self) -> Checkpoint {
        Checkpoint {
            tick: self.tick,
            version: self.version,
            next_tx: self.next_tx,
            boundary: self.journal.next(),
            pages: self.pages.clone(),
            versions: self.versions.clone(),
            components: self.components.clone(),
            spaces: self.spaces.clone(),
            regions: self.regions.clone(),
        }
    }

    pub fn restore(
        registry: impl Registry + 'static,
        limits: JournalLimits,
        state: RestoreState,
    ) -> Result<Self, RestoreError> {
        Self::restore_with_bounds(registry, limits, RestoreBounds::default(), state)
    }

    pub fn restore_with_bounds(
        registry: impl Registry + 'static,
        limits: JournalLimits,
        bounds: RestoreBounds,
        state: RestoreState,
    ) -> Result<Self, RestoreError> {
        validate_state(&state, limits, bounds)?;
        // All structural checks finish before constructing the replacement world.
        let mut world = Self::with_limits(registry, limits);
        world.pages.clear();
        world.versions.clear();
        world.components.clear();
        world.spaces.clear();
        world.regions.clear();
        for saved in state.pages {
            let mut page = Page::new(saved.cells, world.registry.as_ref());
            page.version = saved.version;
            page.channel_versions = saved.channel_versions;
            page.reset_rim_versions();
            page.lights = saved.lights.map(LightField);
            page.components = saved.components.into_iter().collect();
            world.versions.insert(saved.id, saved.version);
            world.pages.insert(saved.id, Arc::new(page));
        }
        world.versions.extend(state.tombstones);
        world.components.extend(state.components);
        world.spaces.extend(state.spaces);
        world.regions.extend(state.regions);
        world.tick = state.tick;
        world.version = state.version;
        world.next_tx = state.next_tx;
        Ok(world)
    }

    /// Rebuild and preview only touched pages/components. A corrupt record is
    /// rejected before touching this world. This uses the real transaction engine
    /// twice (preview and publish), rather than cloning/scanning the entire world.
    /// Registry definitions must remain immutable, as required by `Registry`.
    pub fn apply_recovered_change(&mut self, change: &WorldChange) -> Result<(), ReplayError> {
        if change.tx.0 != self.next_tx {
            return Err(ReplayError::TransactionOrder {
                expected: self.next_tx,
                actual: change.tx.0,
            });
        }
        let transaction = recovered_transaction(change, self.spaces.contains_key(&change.space))?;
        let mut touched = BTreeSet::new();
        for page in &change.pages {
            touched.insert(page.page);
        }
        for cell in &change.cells {
            touched.insert(cell.page);
        }
        for light in &change.lights {
            touched.insert(light.page);
        }
        for component in &change.components {
            for record in component
                .before
                .iter()
                .chain(component.after.iter())
                .chain(self.components.get(&component.id))
            {
                touched.insert(record.data.anchor.page());
            }
        }
        let mut preview = World {
            registry: self.registry.clone(),
            pages: touched
                .iter()
                .filter_map(|id| self.pages.get(id).map(|page| (*id, page.clone())))
                .collect(),
            versions: touched
                .iter()
                .filter_map(|id| self.versions.get(id).map(|version| (*id, *version)))
                .collect(),
            components: change
                .components
                .iter()
                .filter_map(|component| {
                    self.components
                        .get(&component.id)
                        .map(|record| (component.id, record.clone()))
                })
                .collect(),
            spaces: self
                .spaces
                .get(&change.space)
                .map(|record| [(change.space, *record)].into_iter().collect())
                .unwrap_or_default(),
            regions: touched
                .iter()
                .map(|page| RegionId::from_page(*page))
                .filter_map(|id| self.regions.get(&id).map(|record| (id, *record)))
                .collect(),
            tick: self.tick,
            version: self.version,
            next_tx: self.next_tx,
            journal: Journal::new(JournalLimits {
                max_records: 1,
                max_bytes: usize::MAX,
                max_age_ticks: u64::MAX,
            }),
        };
        let previewed = preview.commit(transaction)?;
        if !same_change(&previewed, change) {
            return Err(ReplayError::MetadataMismatch);
        }
        // Full commit retains its existing journal capacity/age contract and is
        // atomic on every error path. Unchanged world pages are never cloned.
        self.commit(recovered_transaction(
            change,
            self.spaces.contains_key(&change.space),
        )?)?;
        Ok(())
    }
}

fn validate_state(
    state: &RestoreState,
    limits: JournalLimits,
    bounds: RestoreBounds,
) -> Result<(), RestoreError> {
    if state.schema != PERSISTENCE_SCHEMA {
        return Err(RestoreError::Schema(state.schema));
    }
    if state.version.0.checked_add(1) != Some(state.next_tx) || state.next_tx == 0 {
        return Err(RestoreError::InvalidHeader);
    }
    if limits.max_records == 0 || limits.max_bytes == 0 {
        return Err(RestoreError::InvalidJournalLimits);
    }
    for (count, max, name) in [
        (state.pages.len(), bounds.max_pages, "pages"),
        (state.tombstones.len(), bounds.max_tombstones, "tombstones"),
        (state.components.len(), bounds.max_components, "components"),
        (state.spaces.len(), bounds.max_spaces, "spaces"),
        (state.regions.len(), bounds.max_regions, "regions"),
    ] {
        if count > max {
            return Err(RestoreError::Bounds(name));
        }
    }
    let mut spaces = BTreeMap::new();
    for (id, record) in &state.spaces {
        if spaces.insert(*id, *record).is_some() {
            return Err(RestoreError::DuplicateSpace(*id));
        }
        if !record.transform.is_valid()
            || record.version > state.version
            || (record.version.0 == 0
                && (*id != SpaceId(0) || record.transform != SpaceTransform::IDENTITY))
        {
            return Err(RestoreError::InvalidSpace(*id));
        }
    }
    if !spaces.contains_key(&SpaceId(0)) {
        return Err(RestoreError::MissingGroundSpace);
    }
    let mut versions = BTreeMap::new();
    let mut loaded = BTreeSet::new();
    let mut expected_regions = BTreeMap::<RegionId, RegionSummary>::new();
    for page in &state.pages {
        if !loaded.insert(page.id) {
            return Err(RestoreError::DuplicatePage(page.id));
        }
        if !spaces.contains_key(&page.id.space) {
            return Err(RestoreError::MissingSpace(page.id.space));
        }
        if page.version.0 == 0 || page.version > state.version {
            return Err(RestoreError::InvalidPageVersion(page.id));
        }
        if page
            .channel_versions
            .iter()
            .any(|version| version.0 == 0 || *version > page.version)
        {
            return Err(RestoreError::InvalidChannelVersion(page.id));
        }
        if let Some(lights) = &page.lights
            && let Some(index) = lights
                .iter()
                .position(|light| light.sky > 15 || light.block > 15)
        {
            return Err(RestoreError::InvalidLight {
                page: page.id,
                index: LocalIdx::new(index as u16).expect("page range"),
            });
        }
        versions.insert(page.id, page.version);
        let region = expected_regions
            .entry(RegionId::from_page(page.id))
            .or_insert(RegionSummary {
                loaded_pages: 0,
                version: page.version,
            });
        region.loaded_pages += 1;
        region.version = region.version.max(page.version);
    }
    let mut deleted = BTreeSet::new();
    for (id, version) in &state.tombstones {
        if !deleted.insert(*id) {
            return Err(RestoreError::DuplicateTombstone(*id));
        }
        if loaded.contains(id) {
            return Err(RestoreError::LoadedTombstone(*id));
        }
        if !spaces.contains_key(&id.space) {
            return Err(RestoreError::MissingSpace(id.space));
        }
        if version.0 == 0 || *version > state.version {
            return Err(RestoreError::InvalidTombstoneVersion(*id));
        }
        versions.insert(*id, *version);
        let region = expected_regions
            .entry(RegionId::from_page(*id))
            .or_insert(RegionSummary {
                loaded_pages: 0,
                version: *version,
            });
        region.version = region.version.max(*version);
    }
    let mut components = BTreeMap::new();
    let mut anchors = BTreeSet::new();
    let mut payload_bytes = 0usize;
    for (id, record) in &state.components {
        if components.insert(*id, record).is_some() {
            return Err(RestoreError::DuplicateComponent(*id));
        }
        if record.version.0 == 0
            || record.version > state.version
            || !loaded.contains(&record.data.anchor.page())
            || versions
                .get(&record.data.anchor.page())
                .is_none_or(|version| record.version > *version)
        {
            return Err(RestoreError::InvalidComponent(*id));
        }
        if !anchors.insert(record.data.anchor) {
            return Err(RestoreError::DuplicateAnchor(*id));
        }
        payload_bytes = payload_bytes
            .checked_add(record.data.payload.len())
            .ok_or(RestoreError::Bounds("component bytes"))?;
        if payload_bytes > bounds.max_component_bytes {
            return Err(RestoreError::Bounds("component bytes"));
        }
    }
    let mut linked = BTreeSet::new();
    for page in &state.pages {
        let mut indices = BTreeSet::new();
        for (index, id) in &page.components {
            let valid = components.get(id).is_some_and(|record| {
                record.data.anchor.page() == page.id && record.data.anchor.local_index() == *index
            });
            if !valid || !indices.insert(*index) || !linked.insert(*id) {
                return Err(RestoreError::InvalidComponentLink {
                    page: page.id,
                    component: *id,
                });
            }
        }
    }
    if let Some((id, _)) = components.iter().find(|(id, _)| !linked.contains(id)) {
        return Err(RestoreError::InvalidComponent(*id));
    }
    let mut regions = BTreeMap::new();
    for (id, region) in &state.regions {
        if regions.insert(*id, *region).is_some() {
            return Err(RestoreError::DuplicateRegion(*id));
        }
        if !expected_regions.get(id).is_some_and(|expected| {
            expected.loaded_pages == region.loaded_pages && expected.version == region.version
        }) {
            return Err(RestoreError::InvalidRegion(*id));
        }
    }
    if let Some((id, _)) = expected_regions
        .iter()
        .find(|(id, _)| !regions.contains_key(id))
    {
        return Err(RestoreError::MissingRegion(*id));
    }
    Ok(())
}

fn recovered_transaction(
    change: &WorldChange,
    space_exists: bool,
) -> Result<Transaction, ReplayError> {
    let mut tx = Transaction::new(change.tick, change.space);
    for page in &change.pages {
        if page.created && page.removed {
            return Err(ReplayError::Malformed("page both created and removed"));
        }
        if page.created != page.initial_cells.is_some()
            || page.removed != page.removed_cells.is_some()
        {
            return Err(ReplayError::Malformed("structural cell payload"));
        }
        tx.expect_page(page.page, page.before);
        if let Some(cells) = &page.initial_cells {
            tx.create_page(page.page, Vec::from(cells.as_slice()));
        }
        if page.removed {
            tx.remove_page(page.page);
        }
    }
    for cell in &change.cells {
        tx.set_cell(position(cell.page, cell.index)?, cell.new);
    }
    for light in &change.lights {
        tx.set_light(position(light.page, light.index)?, light.new);
    }
    for component in &change.components {
        if let Some(after) = &component.after {
            tx.set_component(component.id, after.data.clone(), component.channels);
        } else {
            tx.remove_component(component.id, component.channels);
        }
    }
    if let Some(transform) = &change.transform {
        if space_exists {
            tx.set_transform(transform.after);
        } else {
            tx.create_space(transform.after);
        }
    }
    for event in &change.events {
        tx.event(event.clone());
    }
    Ok(tx)
}

fn position(page: PageId, index: LocalIdx) -> Result<core_types::BlockPos, ReplayError> {
    let (x, y, z) = index.xyz();
    Ok(core_types::BlockPos {
        space: page.space,
        x: page
            .x
            .checked_mul(16)
            .and_then(|p| p.checked_add(i32::from(x)))
            .ok_or(ReplayError::Malformed("cell coordinate overflow"))?,
        y: page
            .y
            .checked_mul(16)
            .and_then(|p| p.checked_add(i32::from(y)))
            .ok_or(ReplayError::Malformed("cell coordinate overflow"))?,
        z: page
            .z
            .checked_mul(16)
            .and_then(|p| p.checked_add(i32::from(z)))
            .ok_or(ReplayError::Malformed("cell coordinate overflow"))?,
    })
}

fn same_change(a: &WorldChange, b: &WorldChange) -> bool {
    a.tx == b.tx
        && a.tick == b.tick
        && a.space == b.space
        && a.channels == b.channels
        && a.events == b.events
        && a.pages.len() == b.pages.len()
        && a.pages.iter().zip(&b.pages).all(|(a, b)| {
            a.page == b.page
                && a.before == b.before
                && a.after == b.after
                && a.channels == b.channels
                && a.created == b.created
                && a.removed == b.removed
                && a.initial_cells == b.initial_cells
                && a.removed_cells == b.removed_cells
        })
        && a.cells.len() == b.cells.len()
        && a.cells.iter().zip(&b.cells).all(|(a, b)| {
            a.page == b.page && a.index == b.index && a.old == b.old && a.new == b.new
        })
        && a.lights.len() == b.lights.len()
        && a.lights.iter().zip(&b.lights).all(|(a, b)| {
            a.page == b.page && a.index == b.index && a.old == b.old && a.new == b.new
        })
        && a.components.len() == b.components.len()
        && a.components.iter().zip(&b.components).all(|(a, b)| {
            a.id == b.id && a.before == b.before && a.after == b.after && a.channels == b.channels
        })
        && match (&a.transform, &b.transform) {
            (None, None) => true,
            (Some(a), Some(b)) => a.before == b.before && a.after == b.after,
            _ => false,
        }
}
