//! Private persisted overrides for public height-tile authority. Every committed
//! cell edit, including AIR, survives page eviction. No raw cell crosses to wire.
//! Morton-ordered columns make a dyadic tile capture a bounded contiguous range.
use core_types::{BlockPos, Cell, PageId, SpaceId, TxId};
use std::collections::{BTreeMap, BTreeSet};
pub const SCHEMA: u16 = 2;
pub const MAX_LEVEL: u8 = 10;
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TileKey {
    pub space: SpaceId,
    pub level: u8,
    pub x: i32,
    pub z: i32,
}
impl TileKey {
    pub fn bounds(self) -> Option<[i64; 4]> {
        if self.level > MAX_LEVEL {
            return None;
        }
        let span = 16_i64 << self.level;
        let x = i64::from(self.x) * span;
        let z = i64::from(self.z) * span;
        let b = [x, z, x + span, z + span];
        b.iter()
            .all(|v| (i64::from(i32::MIN)..=i64::from(i32::MAX) + 1).contains(v))
            .then_some(b)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellEdit {
    pub position: BlockPos,
    pub cell: Cell,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IndexedCell {
    pub y: i32,
    pub cell: Cell,
    pub source_tx: TxId,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditedColumn {
    pub space: SpaceId,
    pub x: i32,
    pub z: i32,
    pub cells: Vec<IndexedCell>,
}
#[derive(Clone, Debug)]
pub struct TileEdits {
    pub key: TileKey,
    pub source: u64,
    pub columns: Vec<EditedColumn>,
    pub visited_columns: usize,
    pub visited_cells: usize,
}
impl TileEdits {
    pub fn column(&self, x: i32, z: i32) -> Option<&EditedColumn> {
        self.columns
            .binary_search_by_key(&morton(x, z), |c| morton(c.x, c.z))
            .ok()
            .map(|i| &self.columns[i])
    }
}
#[derive(Clone, Copy, Debug)]
pub struct IndexLimits {
    pub max_columns: usize,
    pub max_cells: usize,
    pub max_stamps: usize,
    pub max_snapshot_bytes: usize,
}
impl Default for IndexLimits {
    fn default() -> Self {
        Self {
            max_columns: 262144,
            max_cells: 1048576,
            max_stamps: 1048576,
            max_snapshot_bytes: 64 * 1024 * 1024,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexError {
    InvalidLimits,
    InvalidTile,
    InvalidTransaction,
    DuplicateEdit,
    Capacity,
    SnapshotTooLarge,
    MissingIndex,
    WrongLegacyPage,
    Malformed,
}
impl std::fmt::Display for IndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for IndexError {}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IndexWork {
    pub cells_written: usize,
    pub columns_added: usize,
    pub ancestors_touched: usize,
    pub world_reads: usize,
}
struct Column {
    x: i32,
    z: i32,
    cells: BTreeMap<i32, (Cell, TxId)>,
}
pub struct HeightEditIndex {
    limits: IndexLimits,
    columns: BTreeMap<(SpaceId, u64), Column>,
    stamps: BTreeMap<TileKey, u64>,
    cells: usize,
    // Equal Cell+TxId runs along Y. Maintained from changed adjacent edges only;
    // the WAL preflight must never serialize or scan the cold index.
    runs: usize,
    last_tx: TxId,
    ready: bool,
    rebuild_tx: TxId,
    pending: BTreeSet<PageId>,
}
impl HeightEditIndex {
    pub fn new(limits: IndexLimits) -> Result<Self, IndexError> {
        if limits.max_columns == 0
            || limits.max_cells == 0
            || limits.max_stamps == 0
            || limits.max_snapshot_bytes < 36
            || limits.max_columns > u32::MAX as usize
            || limits.max_cells > u32::MAX as usize
            || limits.max_stamps > u32::MAX as usize
        {
            return Err(IndexError::InvalidLimits);
        }
        Ok(Self {
            limits,
            columns: BTreeMap::new(),
            stamps: BTreeMap::new(),
            cells: 0,
            runs: 0,
            last_tx: TxId(0),
            ready: true,
            rebuild_tx: TxId(0),
            pending: BTreeSet::new(),
        })
    }
    pub fn is_ready(&self) -> bool {
        self.ready
    }
    pub fn last_tx(&self) -> TxId {
        self.last_tx
    }
    /// Startup/checkpoint-worker traversal; never call this from a frame/tick.
    pub fn iter_cells(&self) -> impl Iterator<Item = (CellEdit, TxId)> + '_ {
        self.columns.iter().flat_map(|((space, _), column)| {
            column.cells.iter().map(move |(y, (cell, tx))| {
                (
                    CellEdit {
                        position: BlockPos {
                            space: *space,
                            x: column.x,
                            y: *y,
                            z: column.z,
                        },
                        cell: *cell,
                    },
                    *tx,
                )
            })
        })
    }
    pub fn cell_count(&self) -> usize {
        self.cells
    }
    pub fn column_count(&self) -> usize {
        self.columns.len()
    }
    pub fn stamp_count(&self) -> usize {
        self.stamps.len()
    }
    pub fn pending_legacy_pages(&self) -> usize {
        self.pending.len()
    }
    pub fn pending_legacy_keys(&self) -> impl ExactSizeIterator<Item = PageId> + '_ {
        self.pending.iter().copied()
    }
    pub fn source(&self, key: TileKey) -> Result<u64, IndexError> {
        key.bounds().ok_or(IndexError::InvalidTile)?;
        if !self.ready {
            return Err(IndexError::MissingIndex);
        }
        Ok(self.stamps.get(&key).copied().unwrap_or(1))
    }
    pub fn get(&self, p: BlockPos) -> Option<IndexedCell> {
        let (cell, source_tx) = *self
            .columns
            .get(&(p.space, morton(p.x, p.z)))?
            .cells
            .get(&p.y)?;
        Some(IndexedCell {
            y: p.y,
            cell,
            source_tx,
        })
    }
    /// Invoke before the durable transaction is accepted. It examines only this
    /// immutable edit batch, never loaded pages or the cold world.
    pub fn preflight(&self, tx: TxId, edits: &[CellEdit]) -> Result<(), IndexError> {
        if tx.0 == 0 || tx.0 == u64::MAX || tx <= self.last_tx {
            return Err(IndexError::InvalidTransaction);
        }
        self.preflight_edits(tx, edits, false, self.pending.len())
    }
    fn preflight_edits(
        &self,
        tx: TxId,
        edits: &[CellEdit],
        legacy: bool,
        pending: usize,
    ) -> Result<(), IndexError> {
        let mut positions = BTreeSet::new();
        let mut overlay = BTreeMap::new();
        let mut edges = BTreeSet::new();
        let mut columns = BTreeSet::new();
        let mut stamps = BTreeSet::new();
        let mut cells = 0;
        for e in edits {
            if !positions.insert(e.position) {
                return Err(IndexError::DuplicateEdit);
            }
            if legacy && self.get(e.position).is_some_and(|old| old.source_tx > tx) {
                continue;
            }
            overlay.insert(e.position, (e.cell, tx));
            edges.insert(e.position);
            if let Some(y) = e.position.y.checked_sub(1) {
                edges.insert(BlockPos { y, ..e.position });
            }
            let c = (e.position.space, morton(e.position.x, e.position.z));
            if !self.columns.contains_key(&c) {
                columns.insert(c);
            }
            if self.get(e.position).is_none() {
                cells += 1;
            }
            for key in affected(e.position) {
                if !self.stamps.contains_key(&key) {
                    stamps.insert(key);
                }
            }
        }
        if self.columns.len() + columns.len() > self.limits.max_columns
            || self.cells + cells > self.limits.max_cells
            || self.stamps.len() + stamps.len() > self.limits.max_stamps
        {
            return Err(IndexError::Capacity);
        }
        let old_value = |p| self.get(p).map(|v| (v.cell, v.source_tx));
        let new_value = |p| overlay.get(&p).copied().or_else(|| old_value(p));
        let mut runs = (self.runs + cells) as i128;
        for left in edges {
            let Some(y) = left.y.checked_add(1) else {
                continue;
            };
            let right = BlockPos { y, ..left };
            let joined = |a: Option<(Cell, TxId)>, b: Option<(Cell, TxId)>| a.is_some() && a == b;
            runs += i128::from(joined(old_value(left), old_value(right)));
            runs -= i128::from(joined(new_value(left), new_value(right)));
        }
        let runs = u128::try_from(runs).map_err(|_| IndexError::Malformed)?;
        let bytes = 36u128
            + (self.columns.len() + columns.len()) as u128 * 16
            + runs * 20
            + (self.stamps.len() + stamps.len()) as u128 * 21
            + pending as u128 * 16;
        if bytes > self.limits.max_snapshot_bytes as u128 {
            return Err(IndexError::SnapshotTooLarge);
        }
        Ok(())
    }
    /// Apply exactly the cell edits already accepted by the WAL boundary. Empty
    /// component-only batches advance the frontier without changing any tile stamp.
    pub fn apply_committed(
        &mut self,
        tx: TxId,
        edits: &[CellEdit],
    ) -> Result<IndexWork, IndexError> {
        self.preflight(tx, edits)?;
        let work = self.insert(tx, edits, false);
        self.last_tx = tx;
        Ok(work)
    }
    fn insert(&mut self, tx: TxId, edits: &[CellEdit], legacy: bool) -> IndexWork {
        let mut work = IndexWork::default();
        let mut touched = BTreeSet::new();
        for e in edits {
            if legacy && self.get(e.position).is_some_and(|old| old.source_tx > tx) {
                continue;
            }
            let key = (e.position.space, morton(e.position.x, e.position.z));
            if !self.columns.contains_key(&key) {
                work.columns_added += 1;
            }
            self.insert_cell(*e, tx);
            work.cells_written += 1;
            touched.extend(affected(e.position));
        }
        for key in touched {
            let stamp = self.stamps.entry(key).or_insert(1);
            *stamp = (*stamp).max(tx.0 + 1);
            work.ancestors_touched += 1;
        }
        work
    }
    fn insert_cell(&mut self, edit: CellEdit, tx: TxId) {
        let p = edit.position;
        let column = self
            .columns
            .entry((p.space, morton(p.x, p.z)))
            .or_insert_with(|| Column {
                x: p.x,
                z: p.z,
                cells: BTreeMap::new(),
            });
        let old = column.cells.get(&p.y).copied();
        let value = (edit.cell, tx);
        let mut old_edges = 0;
        let mut new_edges = 0;
        for y in [p.y.checked_sub(1), p.y.checked_add(1)]
            .into_iter()
            .flatten()
        {
            if let Some(neighbor) = column.cells.get(&y).copied() {
                old_edges += usize::from(old == Some(neighbor));
                new_edges += usize::from(value == neighbor);
            }
        }
        let added = usize::from(old.is_none());
        self.runs = self.runs + added + old_edges - new_edges;
        self.cells += added;
        column.cells.insert(p.y, value);
    }
    /// Missing-index legacy worlds remain explicitly unavailable until every saved
    /// page was compared to canonical server baseline in a background worker.
    pub fn begin_legacy(
        &mut self,
        snapshot_tx: TxId,
        pages: impl IntoIterator<Item = PageId>,
    ) -> Result<(), IndexError> {
        if snapshot_tx.0 == u64::MAX || !self.columns.is_empty() || self.last_tx.0 != 0 {
            return Err(IndexError::InvalidTransaction);
        }
        let mut pending = BTreeSet::new();
        for page in pages {
            pending.insert(page);
            if pending.len() > self.limits.max_columns {
                return Err(IndexError::Capacity);
            }
            if 36u128 + pending.len() as u128 * 16 > self.limits.max_snapshot_bytes as u128 {
                return Err(IndexError::SnapshotTooLarge);
            }
        }
        self.ready = false;
        self.rebuild_tx = snapshot_tx;
        self.last_tx = snapshot_tx;
        self.pending = pending;
        Ok(())
    }
    pub fn ingest_legacy_page(
        &mut self,
        page: PageId,
        differences: &[CellEdit],
    ) -> Result<IndexWork, IndexError> {
        if self.ready
            || !self.pending.contains(&page)
            || differences.iter().any(|e| e.position.page() != page)
        {
            return Err(IndexError::WrongLegacyPage);
        }
        self.preflight_edits(self.rebuild_tx, differences, true, self.pending.len() - 1)?;
        let work = self.insert(self.rebuild_tx, differences, true);
        self.pending.remove(&page);
        Ok(work)
    }
    pub fn finish_legacy(&mut self) -> Result<(), IndexError> {
        if self.ready || !self.pending.is_empty() {
            return Err(IndexError::MissingIndex);
        }
        self.ready = true;
        Ok(())
    }
    /// At most max_columns/max_cells are copied. Oversized edited regions return
    /// backpressure, never an incomplete tile labelled as actual authoritative data.
    pub fn capture(
        &self,
        key: TileKey,
        max_columns: usize,
        max_cells: usize,
    ) -> Result<TileEdits, IndexError> {
        let source = self.source(key)?;
        let b = key.bounds().ok_or(IndexError::InvalidTile)?;
        let bits = 4 + key.level;
        let first = morton(b[0] as i32, b[1] as i32);
        let last = first | ((1u64 << (2 * u32::from(bits))) - 1);
        let mut selected = BTreeSet::new();
        for (id, _) in self.columns.range((key.space, first)..=(key.space, last)) {
            if selected.len() >= max_columns {
                return Err(IndexError::Capacity);
            }
            selected.insert(*id);
        }
        // 17+16 shared boundary sample columns are outside the half-open tile body.
        let step = 1_i64 << key.level;
        for i in 0..=16 {
            for (x, z) in [(b[2], b[1] + i * step), (b[0] + i * step, b[3])] {
                if let (Ok(x), Ok(z)) = (i32::try_from(x), i32::try_from(z)) {
                    let id = (key.space, morton(x, z));
                    if self.columns.contains_key(&id) {
                        selected.insert(id);
                        if selected.len() > max_columns {
                            return Err(IndexError::Capacity);
                        }
                    }
                }
            }
        }
        let mut out = TileEdits {
            key,
            source,
            columns: Vec::with_capacity(selected.len()),
            visited_columns: 0,
            visited_cells: 0,
        };
        for id in selected {
            let c = &self.columns[&id];
            if out.visited_cells + c.cells.len() > max_cells {
                return Err(IndexError::Capacity);
            }
            out.visited_columns += 1;
            out.visited_cells += c.cells.len();
            out.columns.push(EditedColumn {
                space: id.0,
                x: c.x,
                z: c.z,
                cells: c
                    .cells
                    .iter()
                    .map(|(y, (cell, source_tx))| IndexedCell {
                        y: *y,
                        cell: *cell,
                        source_tx: *source_tx,
                    })
                    .collect(),
            });
        }
        Ok(out)
    }
    /// Snapshot serialization belongs to a checkpoint worker. WAL records use the
    /// bounded delta codec below; serializing the whole index per edit is forbidden.
    pub fn encode_snapshot(&self) -> Result<Vec<u8>, IndexError> {
        let size = 36
            + self.columns.len() * 16
            + self.runs * 20
            + self.stamps.len() * 21
            + self.pending.len() * 16;
        if size > self.limits.max_snapshot_bytes {
            return Err(IndexError::SnapshotTooLarge);
        }
        let mut b = Vec::with_capacity(size);
        b.extend_from_slice(b"HIX2");
        u16w(&mut b, SCHEMA);
        u16w(&mut b, u16::from(self.ready));
        u64w(&mut b, self.last_tx.0);
        u64w(&mut b, self.rebuild_tx.0);
        u32w(&mut b, self.columns.len() as u32);
        u32w(&mut b, self.stamps.len() as u32);
        u32w(&mut b, self.pending.len() as u32);
        for ((space, _), c) in &self.columns {
            u32w(&mut b, space.0);
            i32w(&mut b, c.x);
            i32w(&mut b, c.z);
            let count_at = b.len();
            u32w(&mut b, 0);
            let mut count = 0u32;
            let mut cells = c.cells.iter().peekable();
            while let Some((&y, &(cell, tx))) = cells.next() {
                let mut end = y;
                let mut len = 1u32;
                while let Some(&(&next, &value)) = cells.peek() {
                    if end.checked_add(1) != Some(next) || value != (cell, tx) {
                        break;
                    }
                    cells.next();
                    end = next;
                    len += 1;
                }
                i32w(&mut b, y);
                u32w(&mut b, len);
                u32w(&mut b, cell.raw());
                u64w(&mut b, tx.0);
                count += 1;
            }
            b[count_at..count_at + 4].copy_from_slice(&count.to_le_bytes());
        }
        for (key, stamp) in &self.stamps {
            u32w(&mut b, key.space.0);
            b.push(key.level);
            i32w(&mut b, key.x);
            i32w(&mut b, key.z);
            u64w(&mut b, *stamp);
        }
        for p in &self.pending {
            u32w(&mut b, p.space.0);
            i32w(&mut b, p.x);
            i32w(&mut b, p.y);
            i32w(&mut b, p.z);
        }
        debug_assert_eq!(b.len(), size);
        Ok(b)
    }
    pub fn decode_snapshot(bytes: &[u8], limits: IndexLimits) -> Result<Self, IndexError> {
        if bytes.len() > limits.max_snapshot_bytes {
            return Err(IndexError::SnapshotTooLarge);
        }
        let mut r = Reader { bytes, at: 0 };
        let magic = r.take::<4>()?;
        let schema = r.u16()?;
        let legacy = magic == *b"HIX1" && schema == 1;
        if !(legacy || magic == *b"HIX2" && schema == SCHEMA) {
            return Err(IndexError::Malformed);
        }
        let ready = match r.u16()? {
            0 => false,
            1 => true,
            _ => return Err(IndexError::Malformed),
        };
        let last_tx = TxId(r.u64()?);
        let rebuild_tx = TxId(r.u64()?);
        let count = r.u32()? as usize;
        let stamps = r.u32()? as usize;
        let pending = r.u32()? as usize;
        if count
            > if legacy {
                limits.max_cells
            } else {
                limits.max_columns
            }
            || stamps > limits.max_stamps
            || pending > limits.max_columns
            || last_tx.0 == u64::MAX
            || rebuild_tx > last_tx
            || (legacy
                && 36u128 + count as u128 * 28 + stamps as u128 * 21 + pending as u128 * 16
                    != bytes.len() as u128)
            || (!legacy
                && 36u128 + count as u128 * 36 + stamps as u128 * 21 + pending as u128 * 16
                    > bytes.len() as u128)
        {
            return Err(IndexError::Malformed);
        }
        let mut index = Self::new(limits)?;
        index.ready = ready;
        index.last_tx = last_tx;
        index.rebuild_tx = rebuild_tx;
        if legacy {
            for _ in 0..count {
                let e = r.edit()?;
                let tx = TxId(r.u64()?);
                if tx > last_tx || index.get(e.position).is_some() {
                    return Err(IndexError::Malformed);
                }
                index.insert_cell(e, tx);
            }
        } else {
            let mut previous_column = None;
            for _ in 0..count {
                let space = SpaceId(r.u32()?);
                let x = r.i32()?;
                let z = r.i32()?;
                let key = (space, morton(x, z));
                let runs = r.u32()? as usize;
                if previous_column.is_some_and(|old| old >= key)
                    || runs == 0
                    || runs > limits.max_cells - index.cells
                {
                    return Err(IndexError::Malformed);
                }
                previous_column = Some(key);
                let mut previous_run = None;
                for _ in 0..runs {
                    let y = r.i32()?;
                    let len = r.u32()? as usize;
                    let cell = Cell::from_raw(r.u32()?);
                    let tx = TxId(r.u64()?);
                    let end = i64::from(y) + len as i64 - 1;
                    if len == 0
                        || len > limits.max_cells - index.cells
                        || end > i64::from(i32::MAX)
                        || tx > last_tx
                        || previous_run.is_some_and(|(old_end, old_value)| {
                            i64::from(y) <= old_end
                                || (i64::from(y) == old_end + 1 && old_value == (cell, tx))
                        })
                    {
                        return Err(IndexError::Malformed);
                    }
                    for offset in 0..len {
                        index.insert_cell(
                            CellEdit {
                                position: BlockPos {
                                    space,
                                    x,
                                    y: (i64::from(y) + offset as i64) as i32,
                                    z,
                                },
                                cell,
                            },
                            tx,
                        );
                    }
                    previous_run = Some((end, (cell, tx)));
                }
            }
        }
        if index.columns.len() > limits.max_columns {
            return Err(IndexError::Capacity);
        }
        for _ in 0..stamps {
            let key = TileKey {
                space: SpaceId(r.u32()?),
                level: r.u8()?,
                x: r.i32()?,
                z: r.i32()?,
            };
            let stamp = r.u64()?;
            if key.bounds().is_none()
                || stamp == 0
                || stamp > last_tx.0 + 1
                || index.stamps.insert(key, stamp).is_some()
            {
                return Err(IndexError::Malformed);
            }
        }
        for _ in 0..pending {
            let p = PageId {
                space: SpaceId(r.u32()?),
                x: r.i32()?,
                y: r.i32()?,
                z: r.i32()?,
            };
            if !index.pending.insert(p) {
                return Err(IndexError::Malformed);
            }
        }
        if (ready && !index.pending.is_empty()) || r.at != bytes.len() {
            return Err(IndexError::Malformed);
        }
        let encoded_size = 36u128
            + index.columns.len() as u128 * 16
            + index.runs as u128 * 20
            + index.stamps.len() as u128 * 21
            + index.pending.len() as u128 * 16;
        if encoded_size > limits.max_snapshot_bytes as u128 {
            return Err(IndexError::SnapshotTooLarge);
        }
        Ok(index)
    }
    pub fn encode_delta(tx: TxId, edits: &[CellEdit]) -> Result<Vec<u8>, IndexError> {
        if tx.0 == 0 || tx.0 == u64::MAX || edits.len() > 1048576 {
            return Err(IndexError::InvalidTransaction);
        }
        let mut unique = BTreeSet::new();
        if edits.iter().any(|e| !unique.insert(e.position)) {
            return Err(IndexError::DuplicateEdit);
        }
        let mut b = Vec::with_capacity(16 + edits.len() * 20);
        b.extend_from_slice(b"HID1");
        u64w(&mut b, tx.0);
        u32w(&mut b, edits.len() as u32);
        for e in edits {
            edit_write(&mut b, *e)
        }
        Ok(b)
    }
    pub fn decode_delta(
        bytes: &[u8],
        max_edits: usize,
    ) -> Result<(TxId, Vec<CellEdit>), IndexError> {
        let mut r = Reader { bytes, at: 0 };
        if r.take::<4>()? != *b"HID1" {
            return Err(IndexError::Malformed);
        }
        let tx = TxId(r.u64()?);
        let count = r.u32()? as usize;
        if tx.0 == 0 || tx.0 == u64::MAX || count > max_edits || 16 + count * 20 != bytes.len() {
            return Err(IndexError::Malformed);
        }
        let mut out = Vec::with_capacity(count);
        let mut unique = BTreeSet::new();
        for _ in 0..count {
            let e = r.edit()?;
            if !unique.insert(e.position) {
                return Err(IndexError::DuplicateEdit);
            }
            out.push(e);
        }
        Ok((tx, out))
    }
}
fn affected(p: BlockPos) -> Vec<TileKey> {
    let mut out = Vec::with_capacity(44);
    for level in 0..=MAX_LEVEL {
        let span = 16_i64 << level;
        let x = i64::from(p.x).div_euclid(span);
        let z = i64::from(p.z).div_euclid(span);
        let xs = if i64::from(p.x).rem_euclid(span) == 0 {
            2
        } else {
            1
        };
        let zs = if i64::from(p.z).rem_euclid(span) == 0 {
            2
        } else {
            1
        };
        for dx in 0..xs {
            for dz in 0..zs {
                let key = TileKey {
                    space: p.space,
                    level,
                    x: (x - dx) as i32,
                    z: (z - dz) as i32,
                };
                if key.bounds().is_some() {
                    out.push(key)
                }
            }
        }
    }
    out
}
fn morton(x: i32, z: i32) -> u64 {
    fn spread(mut n: u64) -> u64 {
        n = (n | (n << 16)) & 0x0000ffff0000ffff;
        n = (n | (n << 8)) & 0x00ff00ff00ff00ff;
        n = (n | (n << 4)) & 0x0f0f0f0f0f0f0f0f;
        n = (n | (n << 2)) & 0x3333333333333333;
        (n | (n << 1)) & 0x5555555555555555
    }
    spread(u64::from((x as u32) ^ 0x80000000)) | (spread(u64::from((z as u32) ^ 0x80000000)) << 1)
}
fn u16w(b: &mut Vec<u8>, v: u16) {
    b.extend_from_slice(&v.to_le_bytes())
}
fn u32w(b: &mut Vec<u8>, v: u32) {
    b.extend_from_slice(&v.to_le_bytes())
}
fn i32w(b: &mut Vec<u8>, v: i32) {
    b.extend_from_slice(&v.to_le_bytes())
}
fn u64w(b: &mut Vec<u8>, v: u64) {
    b.extend_from_slice(&v.to_le_bytes())
}
fn edit_write(b: &mut Vec<u8>, e: CellEdit) {
    u32w(b, e.position.space.0);
    i32w(b, e.position.x);
    i32w(b, e.position.y);
    i32w(b, e.position.z);
    u32w(b, e.cell.raw())
}
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], IndexError> {
        let end = self.at.checked_add(N).ok_or(IndexError::Malformed)?;
        let s = self.bytes.get(self.at..end).ok_or(IndexError::Malformed)?;
        self.at = end;
        s.try_into().map_err(|_| IndexError::Malformed)
    }
    fn u8(&mut self) -> Result<u8, IndexError> {
        Ok(self.take::<1>()?[0])
    }
    fn u16(&mut self) -> Result<u16, IndexError> {
        Ok(u16::from_le_bytes(self.take()?))
    }
    fn u32(&mut self) -> Result<u32, IndexError> {
        Ok(u32::from_le_bytes(self.take()?))
    }
    fn i32(&mut self) -> Result<i32, IndexError> {
        Ok(i32::from_le_bytes(self.take()?))
    }
    fn u64(&mut self) -> Result<u64, IndexError> {
        Ok(u64::from_le_bytes(self.take()?))
    }
    fn edit(&mut self) -> Result<CellEdit, IndexError> {
        Ok(CellEdit {
            position: BlockPos {
                space: SpaceId(self.u32()?),
                x: self.i32()?,
                y: self.i32()?,
                z: self.i32()?,
            },
            cell: Cell::from_raw(self.u32()?),
        })
    }
}
