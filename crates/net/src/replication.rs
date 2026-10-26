use crate::*;
use std::collections::{BTreeMap, VecDeque};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReplicaError {
    Capacity,
    InvalidDelta,
    InvalidVersion,
    MissingBase {
        page: PageId,
        have: Option<u64>,
        expected: u64,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyResult {
    Applied,
    Stale,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplicaRead {
    Unknown,
    Known(Cell),
}

pub struct Replica {
    pages: BTreeMap<PageId, PageSnapshot>,
    maximum_pages: usize,
}
impl Replica {
    pub fn new(maximum_pages: usize) -> Self {
        Self {
            pages: BTreeMap::new(),
            maximum_pages,
        }
    }
    pub fn page(&self, page: PageId) -> Option<&PageSnapshot> {
        self.pages.get(&page)
    }
    pub fn remove(&mut self, page: PageId) -> bool {
        self.pages.remove(&page).is_some()
    }
    pub fn len(&self) -> usize {
        self.pages.len()
    }
    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }
    pub fn read(&self, position: BlockPos) -> ReplicaRead {
        self.pages
            .get(&position.page())
            .map_or(ReplicaRead::Unknown, |page| {
                ReplicaRead::Known(page.cells[usize::from(position.local_index().get())])
            })
    }
    pub fn apply(&mut self, update: PageUpdate) -> Result<ApplyResult, ReplicaError> {
        match update {
            PageUpdate::Snapshot(snapshot) => {
                if snapshot.version == 0 {
                    return Err(ReplicaError::InvalidVersion);
                }
                if self
                    .pages
                    .get(&snapshot.page)
                    .is_some_and(|old| old.version >= snapshot.version)
                {
                    return Ok(ApplyResult::Stale);
                }
                if !self.pages.contains_key(&snapshot.page)
                    && self.pages.len() >= self.maximum_pages
                {
                    return Err(ReplicaError::Capacity);
                }
                self.pages.insert(snapshot.page, snapshot);
                Ok(ApplyResult::Applied)
            }
            PageUpdate::Delta(delta) => {
                if delta.version <= delta.base {
                    return Err(ReplicaError::InvalidVersion);
                }
                let have = self.pages.get(&delta.page).map(|page| page.version);
                if have.is_some_and(|version| version >= delta.version) {
                    return Ok(ApplyResult::Stale);
                }
                if have != Some(delta.base) {
                    return Err(ReplicaError::MissingBase {
                        page: delta.page,
                        have,
                        expected: delta.base,
                    });
                }
                let mut seen = [false; PAGE_VOLUME];
                for &(index, _) in &delta.changes {
                    let slot = seen
                        .get_mut(usize::from(index))
                        .ok_or(ReplicaError::InvalidDelta)?;
                    if *slot {
                        return Err(ReplicaError::InvalidDelta);
                    }
                    *slot = true;
                }
                let page = self.pages.get_mut(&delta.page).expect("base checked");
                for (index, cell) in delta.changes {
                    page.cells[usize::from(index)] = cell;
                }
                page.version = delta.version;
                page.source_version = delta.source_version;
                Ok(ApplyResult::Applied)
            }
        }
    }
}

/// Bounded per-peer sanitized history. Deltas are produced exclusively against
/// the peer's acknowledged retained image, never its most recently sent image.
pub struct ViewHistory {
    pages: BTreeMap<PageId, VecDeque<PageSnapshot>>,
    confirmed: BTreeMap<PageId, u64>,
    order: VecDeque<PageId>,
    maximum_pages: usize,
    versions_per_page: usize,
    next_version: u64,
}
impl ViewHistory {
    pub fn new(maximum_pages: usize, versions_per_page: usize) -> Self {
        assert!(maximum_pages > 0 && versions_per_page > 0);
        Self {
            pages: BTreeMap::new(),
            confirmed: BTreeMap::new(),
            order: VecDeque::new(),
            maximum_pages,
            versions_per_page,
            next_version: 1,
        }
    }
    pub fn confirmed_page(&self, page: PageId) -> Option<&PageSnapshot> {
        let version = self.confirmed_version(page)?;
        self.pages
            .get(&page)?
            .iter()
            .find(|image| image.version == version)
    }
    pub fn confirmed_version(&self, page: PageId) -> Option<u64> {
        self.confirmed.get(&page).copied()
    }
    pub fn has_version(&self, page: PageId, version: u64) -> bool {
        self.pages
            .get(&page)
            .is_some_and(|p| p.iter().any(|h| h.version == version))
    }
    pub fn acknowledge(&mut self, page: PageId, version: u64) -> bool {
        if self
            .pages
            .get(&page)
            .is_some_and(|history| history.iter().any(|image| image.version == version))
            && self.confirmed.get(&page).is_none_or(|old| *old <= version)
        {
            self.confirmed.insert(page, version);
            true
        } else {
            false
        }
    }
    pub fn remove(&mut self, page: PageId) {
        self.pages.remove(&page);
        self.confirmed.remove(&page);
        self.order.retain(|value| *value != page);
    }
    pub fn build(
        &mut self,
        page: PageId,
        source_version: Version,
        cells: Box<[Cell; PAGE_VOLUME]>,
        force_snapshot: bool,
    ) -> Result<Option<PageUpdate>, ReplicaError> {
        if !self.pages.contains_key(&page) {
            if self.pages.len() >= self.maximum_pages {
                let old = self.order.pop_front().expect("nonempty bounded history");
                self.pages.remove(&old);
                self.confirmed.remove(&old);
            }
            self.pages.insert(page, VecDeque::new());
        }
        self.order.retain(|value| *value != page);
        self.order.push_back(page);
        let history = self.pages.get_mut(&page).expect("inserted");
        if history
            .back()
            .is_none_or(|old| old.cells != cells || old.source_version != source_version)
        {
            let version = self.next_version;
            self.next_version = self
                .next_version
                .checked_add(1)
                .ok_or(ReplicaError::InvalidVersion)?;
            history.push_back(PageSnapshot {
                page,
                version,
                source_version,
                cells,
            });
            while history.len() > self.versions_per_page {
                history.pop_front();
            }
        }
        let latest = history.back().expect("created");
        let base = self
            .confirmed
            .get(&page)
            .and_then(|confirmed| history.iter().find(|image| image.version == *confirmed));
        if !force_snapshot && let Some(base) = base {
            if base.version == latest.version {
                return Ok(None);
            }
            let changes: Vec<_> = base
                .cells
                .iter()
                .zip(latest.cells.iter())
                .enumerate()
                .filter_map(|(index, (old, new))| (old != new).then_some((index as u16, *new)))
                .collect();
            let delta = PageDelta {
                page,
                base: base.version,
                version: latest.version,
                source_version,
                changes,
            };
            let delta_bytes = encode_server(&ServerMessage::Page(PageUpdate::Delta(delta.clone())))
                .map_err(|_| ReplicaError::InvalidDelta)?
                .len();
            let snapshot_bytes =
                encode_server(&ServerMessage::Page(PageUpdate::Snapshot(latest.clone())))
                    .map_err(|_| ReplicaError::InvalidDelta)?
                    .len();
            if delta_bytes < snapshot_bytes {
                return Ok(Some(PageUpdate::Delta(delta)));
            }
        }
        Ok(Some(PageUpdate::Snapshot(latest.clone())))
    }
}

#[derive(Clone, Copy, Debug)]
struct Predicted {
    position: BlockPos,
    value: Cell,
    accepted: Option<Version>,
}
pub struct PredictionOverlay {
    edits: BTreeMap<ActionId, Predicted>,
    maximum: usize,
}
impl PredictionOverlay {
    pub fn new(maximum: usize) -> Self {
        Self {
            edits: BTreeMap::new(),
            maximum,
        }
    }
    pub fn predict(
        &mut self,
        id: ActionId,
        position: BlockPos,
        value: Cell,
    ) -> Result<(), ReplicaError> {
        if !self.edits.contains_key(&id) && self.edits.len() >= self.maximum {
            return Err(ReplicaError::Capacity);
        }
        self.edits.insert(
            id,
            Predicted {
                position,
                value,
                accepted: None,
            },
        );
        Ok(())
    }
    pub fn read(&self, position: BlockPos, replica: &Replica) -> ReplicaRead {
        self.edits
            .iter()
            .rev()
            .find_map(|(_, edit)| {
                (edit.position == position).then_some(ReplicaRead::Known(edit.value))
            })
            .unwrap_or_else(|| replica.read(position))
    }
    pub fn reply(&mut self, reply: ActionReply) {
        match reply.status {
            ActionStatus::Pending => {}
            ActionStatus::Rejected(_) => {
                self.edits.remove(&reply.id);
            }
            ActionStatus::Accepted => {
                if let Some(edit) = self.edits.get_mut(&reply.id) {
                    edit.accepted = Some(reply.source_version);
                    if let Some(cell) = reply.cell {
                        edit.value = cell;
                    }
                }
            }
        }
    }
    pub fn retire_applied(&mut self, replica: &Replica) {
        self.edits.retain(|_, edit| {
            !edit.accepted.is_some_and(|version| {
                replica.page(edit.position.page()).is_some_and(|page| {
                    page.source_version >= version
                        && page.cells[usize::from(edit.position.local_index().get())] == edit.value
                })
            })
        });
    }
    pub fn positions(&self) -> impl Iterator<Item = BlockPos> + '_ {
        self.edits.values().map(|edit| edit.position)
    }
    pub fn len(&self) -> usize {
        self.edits.len()
    }
    pub fn is_empty(&self) -> bool {
        self.edits.is_empty()
    }
}

pub struct InputHistory {
    frames: VecDeque<InputFrame>,
    maximum: usize,
    acknowledged: u64,
}
impl InputHistory {
    pub fn new(maximum: usize) -> Self {
        Self {
            frames: VecDeque::with_capacity(maximum),
            maximum,
            acknowledged: 0,
        }
    }
    pub fn push(&mut self, frame: InputFrame) -> Result<(), ReplicaError> {
        if frame.sequence <= self.acknowledged
            || self
                .frames
                .back()
                .is_some_and(|old| old.sequence >= frame.sequence)
        {
            return Err(ReplicaError::InvalidVersion);
        }
        if self.frames.len() >= self.maximum {
            return Err(ReplicaError::Capacity);
        }
        self.frames.push_back(frame);
        Ok(())
    }
    pub fn acknowledge(&mut self, sequence: u64) {
        self.acknowledged = self.acknowledged.max(sequence);
        while self
            .frames
            .front()
            .is_some_and(|frame| frame.sequence <= self.acknowledged)
        {
            self.frames.pop_front();
        }
    }
    pub fn pending(&self) -> impl Iterator<Item = &InputFrame> {
        self.frames.iter()
    }
    pub fn recent(&self) -> impl Iterator<Item = &InputFrame> {
        self.frames
            .iter()
            .skip(self.frames.len().saturating_sub(MAX_INPUT_HISTORY))
    }
}

/// Page-space hysteresis: enter within inner radius, retain until outer radius.
pub fn aoi_contains(
    center: PageId,
    page: PageId,
    inner: u16,
    outer: u16,
    already_present: bool,
) -> bool {
    if center.space != page.space || outer < inner {
        return false;
    }
    let radius = i64::from(if already_present { outer } else { inner });
    [
        i64::from(center.x) - i64::from(page.x),
        i64::from(center.y) - i64::from(page.y),
        i64::from(center.z) - i64::from(page.z),
    ]
    .iter()
    .all(|distance| distance.abs() <= radius)
}
