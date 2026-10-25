//! Certificates for a coherent old base plus a current sparse overlay. Only
//! dispatched snapshots are tracked; fixed masks bound storage to 512 B/job.
//! Structural changes and journal gaps invalidate the certificate completely.
use crate::{INPUT_PAGES, MESH_CHANNELS, edit_shell::OwnerMask};
use core_types::{BlockPos, PageId};
use jobs::JobId;
use std::collections::{BTreeMap, BTreeSet};
use world::{SampledPageInput, WorldChange};

struct Entry {
    job: JobId,
    inputs: [SampledPageInput; INPUT_PAGES],
    owners: OwnerMask,
    valid: bool,
}
#[derive(Default)]
pub(crate) struct RebaseTracker {
    entries: BTreeMap<PageId, Entry>,
}
impl RebaseTracker {
    pub fn track(&mut self, page: PageId, job: JobId, inputs: [SampledPageInput; INPUT_PAGES]) {
        assert!(
            !self.entries.contains_key(&page),
            "one tracked base per page"
        );
        self.entries.insert(
            page,
            Entry {
                job,
                inputs,
                owners: OwnerMask::default(),
                valid: true,
            },
        );
    }
    pub fn preserves(&self, page: PageId) -> bool {
        self.entries.get(&page).is_some_and(|entry| entry.valid)
    }
    pub fn accepts(
        &self,
        page: PageId,
        job: JobId,
        inputs: &[SampledPageInput; INPUT_PAGES],
    ) -> bool {
        self.entries
            .get(&page)
            .is_some_and(|entry| entry.valid && entry.job == job && entry.inputs == *inputs)
    }
    pub fn certificate(
        &self,
        page: PageId,
        inputs: &[SampledPageInput; INPUT_PAGES],
    ) -> Option<(JobId, &OwnerMask)> {
        self.entries
            .get(&page)
            .filter(|entry| entry.valid && entry.inputs == *inputs)
            .map(|entry| (entry.job, &entry.owners))
    }
    pub fn forget(&mut self, page: PageId) {
        self.entries.remove(&page);
    }
    pub fn forget_job(&mut self, page: PageId, job: JobId) {
        if self
            .entries
            .get(&page)
            .is_some_and(|entry| entry.job == job)
        {
            self.forget(page);
        }
    }
    pub fn invalidate_all(&mut self) {
        for entry in self.entries.values_mut() {
            entry.valid = false;
        }
    }
    pub fn retain_visible(&mut self, visible: &BTreeSet<PageId>) {
        self.entries.retain(|page, _| visible.contains(page));
    }
    pub fn observe(&mut self, change: &WorldChange) {
        if self.entries.is_empty() || !change.channels.intersects(MESH_CHANNELS) {
            return;
        }
        for page in &change.pages {
            if page.channels.intersects(MESH_CHANNELS)
                && (page.created
                    || page.removed
                    || !change.cells.iter().any(|cell| cell.page == page.page))
            {
                // A replacement/unknown page has no sparse cell proof. Only the
                // at most 27 tasks sampling it lose their certificates.
                for entry in self.entries.values_mut() {
                    if entry.inputs.iter().any(|input| input.page == page.page) {
                        entry.valid = false;
                    }
                }
            }
        }
        for cell in &change.cells {
            if !rules::properties(cell.old)
                .changed_channels(rules::properties(cell.new))
                .intersects(MESH_CHANNELS)
            {
                continue;
            }
            let (x, y, z) = cell.index.xyz();
            let absolute =
                |page: i32, local: u8| page.checked_mul(16)?.checked_add(i32::from(local));
            let (Some(x), Some(y), Some(z)) = (
                absolute(cell.page.x, x),
                absolute(cell.page.y, y),
                absolute(cell.page.z, z),
            ) else {
                self.invalidate_all();
                return;
            };
            for dy in -1..=1 {
                for dz in -1..=1 {
                    for dx in -1..=1 {
                        let (Some(x), Some(y), Some(z)) =
                            (x.checked_add(dx), y.checked_add(dy), z.checked_add(dz))
                        else {
                            self.invalidate_all();
                            return;
                        };
                        let owner = BlockPos {
                            space: cell.page.space,
                            x,
                            y,
                            z,
                        };
                        if let Some(entry) = self.entries.get_mut(&owner.page()) {
                            entry.owners.insert(owner.local_index());
                        }
                    }
                }
            }
        }
    }
}
