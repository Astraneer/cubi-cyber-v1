//! Durable owner of the sparse actual-world edit index and sky certificates.
//! Hot paths inspect changed cells/columns only. Full traversal is restricted to
//! startup restoration; missing legacy data never certifies an unknown sky.
use crate::height_index::{CellEdit, HeightEditIndex, IndexError, IndexLimits, IndexWork};
use core_types::{BlockPos, PageId, SpaceId, TxId};
use std::collections::{BTreeMap, BTreeSet};
use world::{WorldChange, lighting::SkyAccess};
type Column = (SpaceId, i32, i32);
pub const MAX_RUNTIME_BYTES: usize = 8 * 1024 * 1024;
#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub index: IndexLimits,
    pub max_batch_cells: usize,
    pub profile_top: i32,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            index: IndexLimits {
                max_columns: 65536,
                max_cells: 1048576,
                max_stamps: 262144,
                max_snapshot_bytes: MAX_RUNTIME_BYTES,
            },
            max_batch_cells: 65536,
            profile_top: 127,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidConfig,
    InvalidBatch,
    Capacity,
    StalePlan,
    CoordinateOverflow,
    Index(IndexError),
}
impl From<IndexError> for Error {
    fn from(value: IndexError) -> Self {
        Self::Index(value)
    }
}
#[derive(Clone, Debug)]
pub struct PlannedBatch {
    before: TxId,
    tx: TxId,
    edits: Vec<CellEdit>,
}
impl PlannedBatch {
    pub fn tx(&self) -> TxId {
        self.tx
    }
    pub fn edits(&self) -> &[CellEdit] {
        &self.edits
    }
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        HeightEditIndex::encode_delta(self.tx, &self.edits).map_err(Error::Index)
    }
}
pub struct HeightAuthority {
    config: Config,
    index: HeightEditIndex,
    ceilings: BTreeMap<Column, BTreeSet<i32>>,
}
impl HeightAuthority {
    pub fn new(config: Config) -> Result<Self, Error> {
        if config.max_batch_cells == 0
            || config.max_batch_cells > 1048576
            || config.index.max_snapshot_bytes > MAX_RUNTIME_BYTES
        {
            return Err(Error::InvalidConfig);
        }
        Ok(Self {
            config,
            index: HeightEditIndex::new(config.index)?,
            ceilings: BTreeMap::new(),
        })
    }
    pub fn restore(bytes: &[u8], config: Config) -> Result<Self, Error> {
        let mut this = Self::new(config)?;
        this.index = HeightEditIndex::decode_snapshot(bytes, config.index)?;
        // Restoring a checkpoint is an explicit loading operation.
        for (edit, _) in this.index.iter_cells() {
            update_ceiling(&mut this.ceilings, config.profile_top, edit);
        }
        Ok(this)
    }
    pub fn snapshot(&self) -> Result<Vec<u8>, Error> {
        self.index.encode_snapshot().map_err(Error::Index)
    }
    pub fn index(&self) -> &HeightEditIndex {
        &self.index
    }
    pub fn is_ready(&self) -> bool {
        self.index.is_ready()
    }
    pub fn last_tx(&self) -> TxId {
        self.index.last_tx()
    }
    pub fn blocker_columns(&self) -> usize {
        self.ceilings.len()
    }
    /// World changes are the ordered, immutable records produced by committed
    /// authority transactions. Canonical page births carry no explicit edits;
    /// arbitrary imported page images require the legacy baseline-diff worker.
    pub fn plan_batch(&self, changes: &[WorldChange]) -> Result<Option<PlannedBatch>, Error> {
        let mut tx = self.index.last_tx();
        let mut final_cells = BTreeMap::new();
        for change in changes {
            if change.tx <= tx || change.tx.0 == u64::MAX {
                return Err(Error::InvalidBatch);
            }
            tx = change.tx;
            for edit in &change.cells {
                let (x, y, z) = edit.index.xyz();
                let coordinate = |page: i32, local: u8| {
                    page.checked_mul(16)
                        .and_then(|v| v.checked_add(i32::from(local)))
                        .ok_or(Error::CoordinateOverflow)
                };
                let position = BlockPos {
                    space: edit.page.space,
                    x: coordinate(edit.page.x, x)?,
                    y: coordinate(edit.page.y, y)?,
                    z: coordinate(edit.page.z, z)?,
                };
                if position.space != change.space {
                    return Err(Error::InvalidBatch);
                }
                if !final_cells.contains_key(&position)
                    && final_cells.len() == self.config.max_batch_cells
                {
                    return Err(Error::Capacity);
                }
                final_cells.insert(position, edit.new);
            }
        }
        if final_cells.is_empty() {
            return Ok(None);
        }
        let edits: Vec<_> = final_cells
            .into_iter()
            .map(|(position, cell)| CellEdit { position, cell })
            .collect();
        self.index.preflight(tx, &edits)?;
        Ok(Some(PlannedBatch {
            before: self.index.last_tx(),
            tx,
            edits,
        }))
    }
    pub fn validate(&self, plan: &PlannedBatch) -> Result<(), Error> {
        if self.index.last_tx() != plan.before {
            return Err(Error::StalePlan);
        }
        self.index.preflight(plan.tx, &plan.edits)?;
        Ok(())
    }
    pub fn apply_after_durable(&mut self, plan: PlannedBatch) -> Result<IndexWork, Error> {
        self.validate(&plan)?;
        let work = self.index.apply_committed(plan.tx, &plan.edits)?;
        for edit in plan.edits {
            update_ceiling(&mut self.ceilings, self.config.profile_top, edit);
        }
        Ok(work)
    }
    /// WAL recovery applies every HID1 record in durable order. The index rejects
    /// duplicates/reordering instead of applying a state record twice.
    pub fn apply_encoded_delta(&mut self, bytes: &[u8]) -> Result<IndexWork, Error> {
        let (tx, edits) = HeightEditIndex::decode_delta(bytes, self.config.max_batch_cells)?;
        self.apply_after_durable(PlannedBatch {
            before: self.index.last_tx(),
            tx,
            edits,
        })
    }
    pub fn begin_legacy(
        &mut self,
        snapshot_tx: TxId,
        pages: impl IntoIterator<Item = PageId>,
    ) -> Result<(), Error> {
        self.index.begin_legacy(snapshot_tx, pages)?;
        Ok(())
    }
    pub fn ingest_legacy_page(
        &mut self,
        page: PageId,
        differences: &[CellEdit],
    ) -> Result<IndexWork, Error> {
        let work = self.index.ingest_legacy_page(page, differences)?;
        // A newer live edit wins a late migration result in both indices.
        for edit in differences {
            let current = self.index.get(edit.position).ok_or(Error::InvalidBatch)?;
            update_ceiling(
                &mut self.ceilings,
                self.config.profile_top,
                CellEdit {
                    position: edit.position,
                    cell: current.cell,
                },
            );
        }
        Ok(work)
    }
    pub fn finish_legacy(&mut self) -> Result<(), Error> {
        self.index.finish_legacy()?;
        Ok(())
    }
    /// The returned immutable view accounts for the current not-yet-durable
    /// batch, so a newly placed roof darkens its SAME-WAL derived light records.
    /// No complete map clone or loaded-page/world scan is performed.
    pub fn sky_proof(
        &self,
        space: SpaceId,
        pending: Option<&PlannedBatch>,
    ) -> Result<HeightSky<'_>, Error> {
        let mut changed: BTreeMap<Column, BTreeMap<i32, core_types::Cell>> = BTreeMap::new();
        if let Some(plan) = pending {
            self.validate(plan)?;
            for edit in &plan.edits {
                if edit.position.space == space && edit.position.y > self.config.profile_top {
                    changed
                        .entry(column(edit.position))
                        .or_default()
                        .insert(edit.position.y, edit.cell);
                }
            }
        }
        let mut overrides = BTreeMap::new();
        for (key, edits) in changed {
            let old = self.ceilings.get(&key).and_then(|ys| {
                ys.iter()
                    .rev()
                    .find(|y| edits.get(y).is_none_or(|cell| blocks_sky(*cell)))
                    .copied()
            });
            let new = edits
                .iter()
                .rev()
                .find(|(_, cell)| blocks_sky(**cell))
                .map(|(y, _)| *y);
            overrides.insert(key, old.into_iter().chain(new).max());
        }
        Ok(HeightSky {
            authority: self,
            space,
            source: pending.map_or(self.index.last_tx().0, |p| p.tx.0) + 1,
            overrides,
        })
    }
}
/// A short-lived read-only certificate for one lighting phase. Only above-profile
/// changed columns allocate an overlay; stationary phases borrow existing maps.
pub struct HeightSky<'a> {
    authority: &'a HeightAuthority,
    space: SpaceId,
    source: u64,
    overrides: BTreeMap<Column, Option<i32>>,
}
impl HeightSky<'_> {
    pub fn is_ready(&self) -> bool {
        self.authority.is_ready()
    }
    pub fn source(&self) -> u64 {
        self.source
    }
    pub fn changed_columns(&self) -> usize {
        self.overrides.len()
    }
}
impl SkyAccess for HeightSky<'_> {
    fn incoming_from_unknown_above(&self, known: BlockPos) -> Option<u8> {
        if !self.is_ready() || known.space != self.space {
            return None;
        }
        let key = column(known);
        let ceiling = if let Some(value) = self.overrides.get(&key) {
            *value
        } else {
            self.authority
                .ceilings
                .get(&key)
                .and_then(|ys| ys.last().copied())
        };
        (known.y >= ceiling.unwrap_or(self.authority.config.profile_top)).then_some(15)
    }
}
fn column(p: BlockPos) -> Column {
    (p.space, p.x, p.z)
}
fn blocks_sky(cell: core_types::Cell) -> bool {
    rules::properties(cell).light_opacity > 0
}
fn update_ceiling(
    ceilings: &mut BTreeMap<Column, BTreeSet<i32>>,
    profile_top: i32,
    edit: CellEdit,
) {
    if edit.position.y <= profile_top {
        return;
    }
    let key = column(edit.position);
    if blocks_sky(edit.cell) {
        ceilings.entry(key).or_default().insert(edit.position.y);
    } else if let Some(ys) = ceilings.get_mut(&key) {
        ys.remove(&edit.position.y);
        if ys.is_empty() {
            ceilings.remove(&key);
        }
    }
}
