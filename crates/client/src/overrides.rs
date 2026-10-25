//! Bounded persistent-in-session edits, distinct from regenerable public terrain.
//! Only touched override pages are staged before an atomic world transaction.
use crate::SceneError;
use core_types::{Cell, ComponentId, LocalIdx, PAGE_VOLUME, PageId};
use std::collections::BTreeMap;
use world::{CellRead, ComponentData, Transaction, World};

#[derive(Clone, Copy, Debug)]
pub struct OverrideLimits {
    pub max_pages: usize,
    pub max_cells: usize,
    pub max_bytes: usize,
}
impl Default for OverrideLimits {
    fn default() -> Self {
        Self {
            max_pages: 512,
            max_cells: 1024 * 1024,
            max_bytes: 8 * 1024 * 1024,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OverrideStats {
    pub pages: usize,
    pub cells: usize,
    pub bytes: usize,
}
#[derive(Clone, Copy)]
struct Edit {
    original: Cell,
    value: Cell,
}
#[derive(Clone, Default)]
struct PageOverrides {
    full: Option<Box<[Cell; PAGE_VOLUME]>>,
    cells: BTreeMap<LocalIdx, Edit>,
    components: BTreeMap<ComponentId, ComponentData>,
}
impl PageOverrides {
    fn is_empty(&self) -> bool {
        self.full.is_none() && self.cells.is_empty() && self.components.is_empty()
    }
    fn footprint(&self) -> OverrideStats {
        // Conservative logical reservation, including fixed per-entry metadata.
        OverrideStats {
            pages: 1,
            cells: if self.full.is_some() {
                PAGE_VOLUME
            } else {
                self.cells.len()
            },
            bytes: 128
                + if self.full.is_some() {
                    PAGE_VOLUME * 4
                } else {
                    self.cells.len() * 64
                }
                + self
                    .components
                    .values()
                    .map(|value| 128 + value.payload.len())
                    .sum::<usize>(),
        }
    }
}
pub(crate) struct Prepared {
    pages: BTreeMap<PageId, PageOverrides>,
    stats: OverrideStats,
}
pub(crate) struct Overrides {
    pages: BTreeMap<PageId, PageOverrides>,
    limits: OverrideLimits,
    stats: OverrideStats,
}
impl Overrides {
    pub fn new(limits: OverrideLimits) -> Result<Self, SceneError> {
        if limits.max_pages == 0 || limits.max_cells == 0 || limits.max_bytes == 0 {
            return Err(SceneError::InvalidConfig(
                "override budgets must be positive",
            ));
        }
        Ok(Self {
            pages: BTreeMap::new(),
            limits,
            stats: OverrideStats::default(),
        })
    }
    pub fn stats(&self) -> OverrideStats {
        self.stats
    }
    fn touch<'a>(
        &self,
        patch: &'a mut BTreeMap<PageId, PageOverrides>,
        page: PageId,
    ) -> &'a mut PageOverrides {
        patch
            .entry(page)
            .or_insert_with(|| self.pages.get(&page).cloned().unwrap_or_default())
    }
    pub fn prepare(&self, world: &World, tx: &Transaction) -> Result<Prepared, SceneError> {
        let mut pages = BTreeMap::new();
        for (page, cells) in tx.created_page_data() {
            let values: Box<[Cell; PAGE_VOLUME]> = cells
                .to_vec()
                .into_boxed_slice()
                .try_into()
                .map_err(|_| SceneError::World(world::CommitError::WrongPageLength))?;
            let target = self.touch(&mut pages, page);
            target.full = Some(values);
            target.cells.clear();
        }
        // Condense repeated writes within the atomic transaction before changing
        // the original-value markers. A→B→A→C cannot lose the true public base A.
        let cells: BTreeMap<_, _> = tx.cell_updates().collect();
        for (position, value) in cells {
            let page = position.page();
            let index = position.local_index();
            let target = self.touch(&mut pages, page);
            if let Some(full) = target.full.as_mut() {
                full[usize::from(index.get())] = value;
                continue;
            }
            let original = if let Some(edit) = target.cells.get(&index) {
                edit.original
            } else {
                match world.read(position) {
                    CellRead::Known(cell) => cell,
                    CellRead::Unknown => {
                        return Err(SceneError::World(world::CommitError::UnknownPage(page)));
                    }
                }
            };
            if value == original {
                target.cells.remove(&index);
            } else {
                target.cells.insert(index, Edit { original, value });
            }
        }
        for (id, value, _) in tx.component_updates() {
            if let Some(old) = world.component(id) {
                self.touch(&mut pages, old.data.anchor.page())
                    .components
                    .remove(&id);
            }
            if let Some(value) = value {
                self.touch(&mut pages, value.anchor.page())
                    .components
                    .insert(id, value.clone());
            }
        }
        let mut stats = self.stats;
        for (page, values) in &pages {
            if let Some(old) = self.pages.get(page) {
                let old = old.footprint();
                stats.pages -= old.pages;
                stats.cells -= old.cells;
                stats.bytes -= old.bytes;
            }
            if !values.is_empty() {
                let new = values.footprint();
                stats.pages = stats
                    .pages
                    .checked_add(new.pages)
                    .ok_or(SceneError::OverrideLimit)?;
                stats.cells = stats
                    .cells
                    .checked_add(new.cells)
                    .ok_or(SceneError::OverrideLimit)?;
                stats.bytes = stats
                    .bytes
                    .checked_add(new.bytes)
                    .ok_or(SceneError::OverrideLimit)?;
            }
        }
        if stats.pages > self.limits.max_pages
            || stats.cells > self.limits.max_cells
            || stats.bytes > self.limits.max_bytes
        {
            return Err(SceneError::OverrideLimit);
        }
        Ok(Prepared { pages, stats })
    }
    pub fn commit(&mut self, prepared: Prepared) {
        for (page, values) in prepared.pages {
            if values.is_empty() {
                self.pages.remove(&page);
            } else {
                self.pages.insert(page, values);
            }
        }
        self.stats = prepared.stats;
    }
    pub fn restore(&self, page: PageId, cells: &mut [Cell; PAGE_VOLUME], tx: &mut Transaction) {
        let Some(values) = self.pages.get(&page) else {
            return;
        };
        if let Some(full) = &values.full {
            cells.copy_from_slice(full.as_ref());
        }
        for (index, edit) in &values.cells {
            cells[usize::from(index.get())] = edit.value;
        }
        for (id, component) in &values.components {
            tx.set_component(*id, component.clone(), core_types::ChannelMask::CONTENTS);
        }
    }
}
