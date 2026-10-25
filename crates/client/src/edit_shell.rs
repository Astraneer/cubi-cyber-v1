//! Bounded H1 edit overlay. Masks address the owner cell of a face, so a greedy
//! base rectangle can survive everywhere except the cells whose faces/AO changed.
//! Reads retain loaded snapshots only; Unknown is an explicit error, never air or
//! a request to synchronously generate terrain.

use crate::{MESH_CHANNELS, material};
use core_types::{BlockPos, LocalIdx, PageId};
use render::terrain_mesh::{
    CellPatchError, MAX_CELL_PATCH_CELLS, MAX_CELL_PATCH_QUADS, MeshWork, OwnerPatch, PageMesh,
    mesh_cell_patch,
};
use std::collections::BTreeMap;
use world::{Page, PageRegion, SampledPageInput as PageInput, World, WorldChange};

/// GPU storage words, with x + 16*z + 256*y as the bit index.
pub const OWNER_MASK_WORDS: usize = 128;
pub const DEFAULT_SHELL_PAGES: usize = 64;
pub const DEFAULT_SHELL_OWNER_CELLS: usize = 8192;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnerMask {
    words: [u32; OWNER_MASK_WORDS],
    len: u16,
}

impl Default for OwnerMask {
    fn default() -> Self {
        Self {
            words: [0; OWNER_MASK_WORDS],
            len: 0,
        }
    }
}
impl OwnerMask {
    pub fn words(&self) -> &[u32; OWNER_MASK_WORDS] {
        &self.words
    }
    pub fn len(&self) -> usize {
        usize::from(self.len)
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn contains(&self, index: LocalIdx) -> bool {
        let index = usize::from(index.get());
        self.words[index / 32] & (1 << (index % 32)) != 0
    }
    pub fn contains_xyz(&self, [x, y, z]: [u8; 3]) -> bool {
        if x >= 16 || y >= 16 || z >= 16 {
            return false;
        }
        self.contains(LocalIdx::new(u16::from(x) + 16 * u16::from(z) + 256 * u16::from(y)).unwrap())
    }
    pub(crate) fn insert(&mut self, index: LocalIdx) -> bool {
        let index = usize::from(index.get());
        let bit = 1 << (index % 32);
        let word = &mut self.words[index / 32];
        if *word & bit != 0 {
            return false;
        }
        *word |= bit;
        self.len += 1;
        true
    }
    fn remove(&mut self, index: LocalIdx) {
        let index = usize::from(index.get());
        let bit = 1 << (index % 32);
        let word = &mut self.words[index / 32];
        if *word & bit != 0 {
            *word &= !bit;
            self.len -= 1;
        }
    }
    /// Iterates set bits only; there is no scan or copy of page cell storage.
    pub fn cells(&self) -> impl Iterator<Item = [u8; 3]> + '_ {
        self.words.iter().enumerate().flat_map(|(word, bits)| {
            let mut remaining = *bits;
            std::iter::from_fn(move || {
                if remaining == 0 {
                    return None;
                }
                let bit = remaining.trailing_zeros() as usize;
                remaining &= remaining - 1;
                let index = word * 32 + bit;
                Some([
                    (index % 16) as u8,
                    (index / 256) as u8,
                    ((index / 16) % 16) as u8,
                ])
            })
        })
    }
}

#[derive(Debug)]
pub enum ShellError {
    Capacity { pages: usize, owner_cells: usize },
    CoordinateOverflow,
    UnknownPage(PageId),
    Mesh(CellPatchError),
}
impl std::fmt::Display for ShellError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "edit shell rejected: {self:?}")
    }
}
impl std::error::Error for ShellError {}

#[derive(Debug)]
pub struct EditPatch {
    pub page: PageId,
    pub mask: OwnerMask,
    pub mesh: PageMesh,
    /// Exact loaded pages sampled, including diagonal AO dependencies.
    pub inputs: Vec<PageInput>,
    pub work: MeshWork,
}
impl EditPatch {
    pub fn is_current(&self, world: &World) -> bool {
        world.matches_inputs(&self.inputs)
    }
}

#[derive(Clone)]
struct ShellPage {
    mask: OwnerMask,
    dirty: bool,
    dirty_owners: OwnerMask,
    source_bits: u32,
}

#[derive(Debug)]
pub struct EditOwnerUpdates {
    pub page: PageId,
    pub mask: OwnerMask,
    pub updates: Vec<OwnerPatch>,
    pub inputs: Vec<PageInput>,
    pub work: MeshWork,
}
impl EditOwnerUpdates {
    pub fn is_current(&self, world: &World) -> bool {
        world.matches_inputs(&self.inputs)
    }
}

/// Session-local masking state. Preflight a proposed transaction with
/// `can_mark_cells`, commit it, then call `mark_changes` only on success.
pub struct EditShell {
    pages: BTreeMap<PageId, ShellPage>,
    owner_cells: usize,
    max_pages: usize,
    max_owner_cells: usize,
}
impl Default for EditShell {
    fn default() -> Self {
        Self::new(DEFAULT_SHELL_PAGES, DEFAULT_SHELL_OWNER_CELLS)
    }
}
impl EditShell {
    pub fn new(max_pages: usize, max_owner_cells: usize) -> Self {
        Self {
            pages: BTreeMap::new(),
            owner_cells: 0,
            max_pages,
            max_owner_cells,
        }
    }
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }
    pub fn owner_cells(&self) -> usize {
        self.owner_cells
    }
    pub fn mask(&self, page: PageId) -> Option<&OwnerMask> {
        self.pages.get(&page).map(|entry| &entry.mask)
    }
    /// Replace a page's accumulated overlay only after an atomic composite GPU
    /// publication. The replacement covers every change after that base snapshot.
    /// Capacity rejection leaves the old mask intact.
    pub fn replace_page_mask(&mut self, page: PageId, mask: OwnerMask) -> Result<(), ShellError> {
        self.can_replace_page_mask(page, &mask)?;
        let old = self.pages.get(&page).map_or(0, |entry| entry.mask.len());
        let owners = self.owner_cells - old + mask.len();
        if mask.is_empty() {
            self.clear_page(page);
        } else {
            self.owner_cells = owners;
            let source_bits = mask_sources(&mask);
            self.pages.insert(
                page,
                ShellPage {
                    mask,
                    dirty: false,
                    dirty_owners: OwnerMask::default(),
                    source_bits,
                },
            );
        }
        Ok(())
    }
    pub fn can_replace_page_mask(&self, page: PageId, mask: &OwnerMask) -> Result<(), ShellError> {
        let old = self.pages.get(&page).map_or(0, |entry| entry.mask.len());
        let owners = self.owner_cells - old + mask.len();
        let pages = self.pages.len() + usize::from(old == 0 && !mask.is_empty());
        if owners > self.max_owner_cells || pages > self.max_pages {
            return Err(ShellError::Capacity {
                pages,
                owner_cells: owners,
            });
        }
        Ok(())
    }
    pub fn dirty_pages(&self) -> impl Iterator<Item = PageId> + '_ {
        self.pages
            .iter()
            .filter(|(_, entry)| entry.dirty)
            .map(|(page, _)| *page)
    }

    fn stage(
        &self,
        cells: impl IntoIterator<Item = Result<BlockPos, ShellError>>,
    ) -> Result<BTreeMap<PageId, ShellPage>, ShellError> {
        let mut staged = BTreeMap::new();
        let mut pages = self.pages.len();
        let mut owners = self.owner_cells;
        for edited in cells {
            let edited = edited?;
            for coordinate in [edited.x, edited.y, edited.z] {
                coordinate
                    .checked_sub(1)
                    .and_then(|_| coordinate.checked_add(1))
                    .ok_or(ShellError::CoordinateOverflow)?;
            }
            // A face's visibility and vertex AO depend on this complete stencil.
            for dy in -1..=1 {
                for dz in -1..=1 {
                    for dx in -1..=1 {
                        let owner = BlockPos {
                            space: edited.space,
                            x: edited
                                .x
                                .checked_add(dx)
                                .ok_or(ShellError::CoordinateOverflow)?,
                            y: edited
                                .y
                                .checked_add(dy)
                                .ok_or(ShellError::CoordinateOverflow)?,
                            z: edited
                                .z
                                .checked_add(dz)
                                .ok_or(ShellError::CoordinateOverflow)?,
                        };
                        let page = owner.page();
                        let entry = staged.entry(page).or_insert_with(|| {
                            self.pages.get(&page).map_or_else(
                                || {
                                    pages += 1;
                                    ShellPage {
                                        mask: OwnerMask::default(),
                                        dirty: true,
                                        dirty_owners: OwnerMask::default(),
                                        source_bits: 0,
                                    }
                                },
                                |entry| entry.clone(),
                            )
                        });
                        owners += usize::from(entry.mask.insert(owner.local_index()));
                        entry.dirty = true;
                        entry.dirty_owners.insert(owner.local_index());
                        let (x, y, z) = owner.local_index().xyz();
                        entry.source_bits |= cell_sources([x, y, z]);
                        if pages > self.max_pages || owners > self.max_owner_cells {
                            return Err(ShellError::Capacity {
                                pages,
                                owner_cells: owners,
                            });
                        }
                    }
                }
            }
        }
        Ok(staged)
    }

    /// Does not mutate masks or the world, including on a capacity error.
    pub fn can_mark_cells(&self, cells: &[BlockPos]) -> Result<(), ShellError> {
        self.stage(cells.iter().copied().map(Ok)).map(|_| ())
    }
    /// Atomically unions all affected owner cells; repeated edits rebuild even
    /// when their mask bits were already present.
    pub fn mark_cells(&mut self, cells: &[BlockPos]) -> Result<(), ShellError> {
        let staged = self.stage(cells.iter().copied().map(Ok))?;
        self.apply_staged(staged);
        Ok(())
    }
    fn apply_staged(&mut self, staged: BTreeMap<PageId, ShellPage>) {
        for (page, entry) in staged {
            self.owner_cells -= self.pages.get(&page).map_or(0, |entry| entry.mask.len());
            self.owner_cells += entry.mask.len();
            self.pages.insert(page, entry);
        }
    }
    /// Only concrete shape/opacity cell changes affect this overlay. Inventory,
    /// emission and derived light transactions do not dirty terrain geometry.
    pub fn mark_changes(&mut self, change: &WorldChange) -> Result<(), ShellError> {
        let cells = change
            .cells
            .iter()
            .filter(|cell| {
                rules::properties(cell.old)
                    .changed_channels(rules::properties(cell.new))
                    .intersects(MESH_CHANNELS)
            })
            .map(|cell| {
                let (x, y, z) = cell.index.xyz();
                Ok(BlockPos {
                    space: cell.page.space,
                    x: cell
                        .page
                        .x
                        .checked_mul(16)
                        .and_then(|n| n.checked_add(i32::from(x)))
                        .ok_or(ShellError::CoordinateOverflow)?,
                    y: cell
                        .page
                        .y
                        .checked_mul(16)
                        .and_then(|n| n.checked_add(i32::from(y)))
                        .ok_or(ShellError::CoordinateOverflow)?,
                    z: cell
                        .page
                        .z
                        .checked_mul(16)
                        .and_then(|n| n.checked_add(i32::from(z)))
                        .ok_or(ShellError::CoordinateOverflow)?,
                })
            });
        let staged = self.stage(cells)?;
        self.apply_staged(staged);
        Ok(())
    }
    pub fn clear_page(&mut self, page: PageId) -> bool {
        if let Some(entry) = self.pages.remove(&page) {
            self.owner_cells -= entry.mask.len();
            true
        } else {
            false
        }
    }
    /// Call only after the replacement base was uploaded. Input validation keeps
    /// a late base mesh from retiring a newer edit's overlay.
    pub fn retire_page(&mut self, page: PageId, world: &World, inputs: &[PageInput]) -> bool {
        let center = inputs.iter().any(|input| {
            input.page == page
                && input.region == PageRegion::WHOLE
                && input.channels.contains(MESH_CHANNELS)
        });
        center && world.matches_inputs(inputs) && self.clear_page(page)
    }

    /// Builds changed pages only. Work and output are bounded by configured owner
    /// cells, not resident world size. All errors preserve every dirty flag/mask.
    pub fn rebuild(
        &mut self,
        world: &World,
        tint: impl Fn(PageId, u8, u8) -> u16,
    ) -> Result<Vec<EditPatch>, ShellError> {
        let mut result = Vec::new();
        for (page, entry) in &self.pages {
            if entry.dirty {
                result.push(build_page_patch(*page, &entry.mask, world, &tint)?);
            }
        }
        for patch in &result {
            let entry = self.pages.get_mut(&patch.page).unwrap();
            entry.dirty = false;
            entry.dirty_owners = OwnerMask::default();
        }
        Ok(result)
    }
    /// Updates fixed per-owner GPU slots, sampling only owners touched since the
    /// previous successful build. A large retained mask never expands this work.
    /// Errors preserve pending updates and mask state atomically.
    pub fn rebuild_owner_updates(
        &self,
        world: &World,
        tint: impl Fn(PageId, u8, u8) -> u16,
    ) -> Result<Vec<EditOwnerUpdates>, ShellError> {
        let mut result = Vec::new();
        for (page, entry) in &self.pages {
            if entry.dirty {
                let mut update =
                    build_owner_updates(*page, &entry.dirty_owners, &entry.mask, world, &tint)?;
                update.inputs = capture_source_bits(*page, entry.source_bits, world)?;
                result.push(update);
            }
        }
        Ok(result)
    }
    /// Acknowledge only after the atomic GPU update. Until then a retry includes
    /// every pending owner, so replacing a budget-blocked batch cannot lose edits.
    pub fn ack_owner_updates(&mut self, update: &EditOwnerUpdates, world: &World) -> bool {
        if !update.is_current(world) {
            return false;
        }
        let Some(entry) = self.pages.get_mut(&update.page) else {
            return false;
        };
        if entry.mask != update.mask {
            return false;
        }
        for patch in &update.updates {
            let Some(index) = LocalIdx::new(patch.owner) else {
                return false;
            };
            if !entry.mask.contains(index) {
                return false;
            }
        }
        for patch in &update.updates {
            entry
                .dirty_owners
                .remove(LocalIdx::new(patch.owner).unwrap());
        }
        entry.dirty = !entry.dirty_owners.is_empty();
        true
    }
}

fn cell_sources([x, y, z]: [u8; 3]) -> u32 {
    let axes = |v: u8| {
        [
            0,
            if v == 0 {
                -1
            } else if v == 15 {
                1
            } else {
                0
            },
        ]
    };
    let mut bits = 0;
    for dy in axes(y) {
        for dz in axes(z) {
            for dx in axes(x) {
                bits |= 1 << ((dx + 1) + 3 * (dz + 1) + 9 * (dy + 1));
            }
        }
    }
    bits
}
fn mask_sources(mask: &OwnerMask) -> u32 {
    mask.cells().fold(0, |bits, cell| bits | cell_sources(cell))
}
fn capture_source_bits(
    page: PageId,
    bits: u32,
    world: &World,
) -> Result<Vec<PageInput>, ShellError> {
    let mut inputs = Vec::with_capacity(bits.count_ones() as usize);
    for dy in -1..=1 {
        for dz in -1..=1 {
            for dx in -1..=1 {
                if bits & (1 << ((dx + 1) + 3 * (dz + 1) + 9 * (dy + 1))) == 0 {
                    continue;
                }
                let source = crate::task::neighbor(page, dx, dy, dz)
                    .ok_or(ShellError::CoordinateOverflow)?;
                if world.page(source).is_none() {
                    return Err(ShellError::UnknownPage(source));
                }
                let region = PageRegion::new([-dx as i8, -dy as i8, -dz as i8])
                    .expect("one-cell source region");
                inputs.push(world.capture_sampled_input(source, MESH_CHANNELS, region));
            }
        }
    }
    Ok(inputs)
}

fn neighbor(page: PageId, [x, y, z]: [i8; 3]) -> Result<PageId, ShellError> {
    Ok(PageId {
        space: page.space,
        x: page
            .x
            .checked_add(i32::from(x.div_euclid(16)))
            .ok_or(ShellError::CoordinateOverflow)?,
        y: page
            .y
            .checked_add(i32::from(y.div_euclid(16)))
            .ok_or(ShellError::CoordinateOverflow)?,
        z: page
            .z
            .checked_add(i32::from(z.div_euclid(16)))
            .ok_or(ShellError::CoordinateOverflow)?,
    })
}
fn local([x, y, z]: [i8; 3]) -> LocalIdx {
    LocalIdx::new(
        x.rem_euclid(16) as u16 + 16 * z.rem_euclid(16) as u16 + 256 * y.rem_euclid(16) as u16,
    )
    .unwrap()
}
fn accumulate(total: &mut MeshWork, work: MeshWork) {
    total.work.accumulate(work.work);
    total.generated_bytes += work.generated_bytes;
    total.rim_cells_scanned += work.rim_cells_scanned;
    total.visibility_words += work.visibility_words;
    total.cached_material_lookups += work.cached_material_lookups;
    total.ao_samples += work.ao_samples;
}
fn sample_sources(
    page: PageId,
    mask: &OwnerMask,
    world: &World,
) -> Result<BTreeMap<PageId, std::sync::Arc<Page>>, ShellError> {
    // Capture only the pages the sparse kernel can sample. Requiring all 27
    // pages here would unnecessarily block an owner at the inner edge of padding.
    let mut sources = BTreeMap::<PageId, std::sync::Arc<Page>>::new();
    for owner in mask.cells() {
        for dy in -1..=1 {
            for dz in -1..=1 {
                for dx in -1..=1 {
                    let sample = [
                        owner[0] as i8 + dx,
                        owner[1] as i8 + dy,
                        owner[2] as i8 + dz,
                    ];
                    let input = neighbor(page, sample)?;
                    if let std::collections::btree_map::Entry::Vacant(entry) = sources.entry(input)
                    {
                        entry.insert(
                            world
                                .snapshot(input)
                                .ok_or(ShellError::UnknownPage(input))?,
                        );
                    }
                }
            }
        }
    }
    Ok(sources)
}
fn capture_sources(
    page: PageId,
    sources: &BTreeMap<PageId, std::sync::Arc<Page>>,
    world: &World,
) -> Vec<PageInput> {
    sources
        .keys()
        .map(|source| {
            let region = PageRegion::new([
                (page.x - source.x) as i8,
                (page.y - source.y) as i8,
                (page.z - source.z) as i8,
            ])
            .expect("sparse sources are at most one page away");
            world.capture_sampled_input(*source, MESH_CHANNELS, region)
        })
        .collect()
}
/// Builds a full sparse replacement, used only when changing the base snapshot.
pub fn build_page_patch(
    page: PageId,
    mask: &OwnerMask,
    world: &World,
    tint: &impl Fn(PageId, u8, u8) -> u16,
) -> Result<EditPatch, ShellError> {
    let sources = sample_sources(page, mask, world)?;
    let inputs = capture_sources(page, &sources, world);
    let mut mesh = PageMesh::with_capacity([mask.len() * 6; 3]);
    let mut chunk_mesh = PageMesh::with_capacity([MAX_CELL_PATCH_QUADS; 3]);
    let mut work = MeshWork::default();
    let mut cells = mask.cells();
    loop {
        let mut chunk = [[0; 3]; MAX_CELL_PATCH_CELLS];
        let mut count = 0;
        for next in cells.by_ref().take(MAX_CELL_PATCH_CELLS) {
            chunk[count] = next;
            count += 1;
        }
        if count == 0 {
            break;
        }
        let chunk_work = mesh_cell_patch(
            &chunk[..count],
            |position| {
                let input = neighbor(page, position).expect("validated sparse sample coordinate");
                let raw = sources[&input].cell(local(position)).raw();
                material(
                    raw,
                    tint(
                        page,
                        position[0].clamp(0, 15) as u8,
                        position[2].clamp(0, 15) as u8,
                    ),
                )
            },
            &mut chunk_mesh,
        )
        .map_err(ShellError::Mesh)?;
        accumulate(&mut work, chunk_work);
        mesh.opaque.append(&mut chunk_mesh.opaque);
        mesh.cutout.append(&mut chunk_mesh.cutout);
        mesh.water.append(&mut chunk_mesh.water);
    }
    Ok(EditPatch {
        page,
        mask: mask.clone(),
        mesh,
        inputs,
        work,
    })
}

/// Public for an atomic full-base rebase: pass `changed=mask` once to initialize
/// fixed slots, then use `EditShell::rebuild_owner_updates` for subsequent edits.
pub fn build_owner_updates(
    page: PageId,
    changed: &OwnerMask,
    mask: &OwnerMask,
    world: &World,
    tint: &impl Fn(PageId, u8, u8) -> u16,
) -> Result<EditOwnerUpdates, ShellError> {
    let sources = sample_sources(page, changed, world)?;
    let inputs = capture_sources(page, &sources, world);
    let mut scratch = PageMesh::with_capacity([6; 3]);
    let mut updates = Vec::with_capacity(changed.len());
    let mut work = MeshWork::default();
    for owner in changed.cells() {
        let next = mesh_cell_patch(
            &[owner],
            |position| {
                let source = neighbor(page, position).expect("validated sparse source");
                let raw = sources[&source].cell(local(position)).raw();
                material(
                    raw,
                    tint(
                        page,
                        position[0].clamp(0, 15) as u8,
                        position[2].clamp(0, 15) as u8,
                    ),
                )
            },
            &mut scratch,
        )
        .map_err(ShellError::Mesh)?;
        accumulate(&mut work, next);
        let index = u16::from(owner[0]) + 16 * u16::from(owner[2]) + 256 * u16::from(owner[1]);
        updates.push(
            OwnerPatch::from_mesh(index, &scratch)
                .expect("one owner has at most six distinct unit faces"),
        );
    }
    Ok(EditOwnerUpdates {
        page,
        mask: mask.clone(),
        updates,
        inputs,
        work,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_types::{Cell, SpaceId, Tick, Version};
    use render::terrain_mesh::{Face, MeshScratch, PageHalo, QuadDescriptor, mesh_page_indexed};
    use world::{CellRead, Transaction};

    fn pos(x: i32, y: i32, z: i32) -> BlockPos {
        BlockPos {
            space: SpaceId(0),
            x,
            y,
            z,
        }
    }
    fn fixture(range: std::ops::RangeInclusive<i32>) -> World {
        let mut world = World::new(rules::properties, 32);
        let mut tx = Transaction::new(Tick(0), SpaceId(0));
        for y in range.clone() {
            for z in range.clone() {
                for x in range.clone() {
                    tx.create_page(
                        PageId {
                            space: SpaceId(0),
                            x,
                            y,
                            z,
                        },
                        vec![Cell::AIR; 4096],
                    );
                }
            }
        }
        world.commit(tx).unwrap();
        world
    }
    #[test]
    fn complete_stencil_crosses_negative_page_corner_and_unions_edits() {
        let mut shell = EditShell::default();
        shell.mark_cells(&[pos(-1, -1, -1)]).unwrap();
        assert_eq!(shell.page_count(), 8);
        assert_eq!(shell.owner_cells(), 27);
        for y in -2..=0 {
            for z in -2..=0 {
                for x in -2..=0 {
                    let owner = pos(x, y, z);
                    assert!(
                        shell
                            .mask(owner.page())
                            .unwrap()
                            .contains(owner.local_index())
                    );
                }
            }
        }
        shell.mark_cells(&[pos(-2, -1, -1)]).unwrap();
        assert_eq!(shell.owner_cells(), 36);
        assert_eq!(shell.mask(pos(0, 0, 0).page()).unwrap().words()[0] & 1, 1);
    }
    #[test]
    fn preflight_and_capacity_errors_do_not_modify_existing_state() {
        let mut shell = EditShell::new(1, 27);
        assert!(shell.can_mark_cells(&[pos(5, 5, 5)]).is_ok());
        assert_eq!(shell.owner_cells(), 0);
        shell.mark_cells(&[pos(5, 5, 5)]).unwrap();
        let old = shell.mask(pos(5, 5, 5).page()).unwrap().clone();
        assert!(matches!(
            shell.mark_cells(&[pos(10, 10, 10)]),
            Err(ShellError::Capacity { .. })
        ));
        assert_eq!(shell.owner_cells(), 27);
        assert_eq!(shell.mask(pos(5, 5, 5).page()), Some(&old));
        assert!(matches!(
            shell.mark_cells(&[pos(i32::MAX, 0, 0)]),
            Err(ShellError::CoordinateOverflow)
        ));
        assert_eq!(shell.owner_cells(), 27);
    }
    #[test]
    fn unknown_is_explicit_and_rebuild_never_generates_or_loses_masks() {
        let world = fixture(0..=0);
        let hash = world.canonical_hash();
        let mut shell = EditShell::default();
        shell.mark_cells(&[pos(0, 0, 0)]).unwrap();
        assert!(matches!(
            shell.rebuild(&world, |_, _, _| u16::MAX),
            Err(ShellError::UnknownPage(_))
        ));
        assert_eq!(shell.owner_cells(), 27);
        assert_eq!(shell.dirty_pages().count(), 8);
        assert_eq!(world.canonical_hash(), hash);
        assert_eq!(world.read(pos(-1, 0, 0)), CellRead::Unknown);
    }
    #[test]
    fn rejected_world_transaction_does_not_create_shell_state() {
        let mut world = fixture(0..=0);
        let hash = world.canonical_hash();
        let shell = EditShell::default();
        shell.can_mark_cells(&[pos(4, 4, 4)]).unwrap();
        let mut tx = Transaction::new(Tick(1), SpaceId(0));
        tx.expect_page(pos(4, 4, 4).page(), Version(u64::MAX));
        tx.set_cell(pos(4, 4, 4), Cell::from_parts(rules::ids::STONE, 0));
        assert!(world.commit(tx).is_err());
        assert_eq!(world.canonical_hash(), hash);
        assert_eq!(shell.owner_cells(), 0);
    }
    #[test]
    fn sparse_rebuild_reads_only_needed_pages_and_repeated_edit_dirties_again() {
        let mut world = fixture(0..=0);
        let mut shell = EditShell::default();
        let mut tx = Transaction::new(Tick(1), SpaceId(0));
        tx.set_cell(pos(4, 4, 4), Cell::from_parts(rules::ids::STONE, 0));
        shell.mark_changes(&world.commit(tx).unwrap()).unwrap();
        let patches = shell.rebuild(&world, |_, _, _| u16::MAX).unwrap();
        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].inputs.len(), 1);
        assert_eq!(patches[0].mesh.quad_count(), 6);
        assert_eq!(patches[0].work.work.cells_scanned, 27 * 27);
        assert_eq!(patches[0].work.work.synchronous_generations, 0);
        assert!(
            shell
                .rebuild(&world, |_, _, _| u16::MAX)
                .unwrap()
                .is_empty()
        );
        let stale = patches[0].inputs.clone();
        let mut tx = Transaction::new(Tick(2), SpaceId(0));
        tx.set_cell(pos(4, 4, 4), Cell::AIR);
        shell.mark_changes(&world.commit(tx).unwrap()).unwrap();
        assert!(!patches[0].is_current(&world));
        assert!(!shell.retire_page(pos(4, 4, 4).page(), &world, &stale));
        let patches = shell.rebuild(&world, |_, _, _| u16::MAX).unwrap();
        assert_eq!(patches[0].mesh.quad_count(), 0);
        assert!(shell.retire_page(patches[0].page, &world, &patches[0].inputs));
        assert_eq!(shell.owner_cells(), 0);
    }
    #[test]
    fn sparse_patch_ignores_neighbor_interior_and_tracks_sampled_boundary() {
        let mut world = fixture(0..=1);
        let mut shell = EditShell::default();
        shell.mark_cells(&[pos(14, 8, 8)]).unwrap();
        let mut patches = shell.rebuild(&world, |_, _, _| u16::MAX).unwrap();
        assert_eq!(patches.len(), 1);
        let patch = patches.pop().unwrap();
        assert_eq!(patch.inputs.len(), 2);
        let mut tx = Transaction::new(Tick(1), SpaceId(0));
        tx.set_cell(pos(24, 8, 8), Cell::from_parts(rules::ids::STONE, 0));
        world.commit(tx).unwrap();
        assert!(patch.is_current(&world));
        let mut tx = Transaction::new(Tick(2), SpaceId(0));
        tx.set_cell(pos(16, 8, 8), Cell::from_parts(rules::ids::STONE, 0));
        world.commit(tx).unwrap();
        assert!(!patch.is_current(&world));
    }

    type FaceKey = ([i16; 3], u8, u8);
    type FaceValue = (u16, [u8; 4], u32, u32);
    fn expanded(mesh: &PageMesh, mask: Option<&OwnerMask>) -> BTreeMap<FaceKey, FaceValue> {
        let mut result = BTreeMap::new();
        for (bucket, quads) in [&mesh.opaque, &mesh.cutout, &mesh.water]
            .into_iter()
            .enumerate()
        {
            for quad in quads {
                let origin = quad.origin().map(i16::from);
                let owner = match quad.face() {
                    Face::Down | Face::West => origin,
                    Face::Up => [origin[0], origin[1] - 1, origin[2] - 1],
                    Face::North => [origin[0] - 1, origin[1], origin[2]],
                    Face::South => [origin[0], origin[1], origin[2] - 1],
                    Face::East => [origin[0] - 1, origin[1], origin[2] - 1],
                };
                let [width, height] = quad.size();
                for v in 0..height {
                    for u in 0..width {
                        let cell = std::array::from_fn(|axis| {
                            owner[axis]
                                + i16::from(quad.face().tangent_u()[axis]) * i16::from(u)
                                + i16::from(quad.face().tangent_v()[axis]) * i16::from(v)
                        });
                        if mask.is_some_and(|mask| mask.contains_xyz(cell.map(|n| n as u8))) {
                            continue;
                        }
                        let old = result.insert(
                            (cell, quad.face() as u8, bucket as u8),
                            (
                                quad.material(),
                                quad.ao(),
                                quad.tint(),
                                quad.words[1] & 0xff00_0000,
                            ),
                        );
                        assert!(old.is_none(), "duplicate owner face");
                    }
                }
            }
        }
        result
    }
    fn full_mesh(world: &World, page: PageId) -> PageMesh {
        let mut halo = PageHalo::filled(0);
        for y in -1..=16 {
            for z in -1..=16 {
                for x in -1..=16 {
                    let input = neighbor(page, [x, y, z]).unwrap();
                    halo.set(
                        x,
                        y,
                        z,
                        world.page(input).unwrap().cell(local([x, y, z])).raw(),
                    );
                }
            }
        }
        let mut mesh = PageMesh::default();
        mesh_page_indexed(
            &halo,
            |raw, _| material(raw, 0x6eaf),
            &mut MeshScratch::new(),
            &mut mesh,
        );
        mesh
    }
    #[test]
    fn masked_base_plus_two_corner_edits_equals_full_current_geometry_ao_tint_uv() {
        let mut world = fixture(-1..=2);
        let mut init = Transaction::new(Tick(1), SpaceId(0));
        for (point, block) in [
            (pos(15, 15, 15), rules::ids::STONE),
            (pos(14, 15, 15), rules::ids::GRASS),
            (pos(15, 14, 15), rules::ids::STONE),
            (pos(15, 15, 14), rules::ids::STONE),
            (pos(17, 16, 16), rules::ids::STONE),
            (pos(16, 17, 16), rules::ids::STONE),
        ] {
            init.set_cell(point, Cell::from_parts(block, 0));
        }
        init.set_cell(pos(14, 15, 16), Cell::from_parts(rules::ids::LOG, 1));
        world.commit(init).unwrap();
        let mut shell = EditShell::default();
        let points = [pos(15, 15, 15), pos(16, 16, 16)];
        shell.can_mark_cells(&points).unwrap();
        shell.mark_cells(&points).unwrap();
        assert_eq!(shell.owner_cells(), 46);
        let old: BTreeMap<_, _> = shell
            .pages
            .keys()
            .map(|page| (*page, full_mesh(&world, *page)))
            .collect();
        let mut edit = Transaction::new(Tick(2), SpaceId(0));
        edit.set_cell(points[0], Cell::AIR);
        edit.set_cell(points[1], Cell::from_parts(rules::ids::GRASS, 0));
        shell.mark_changes(&world.commit(edit).unwrap()).unwrap();
        let patches = shell.rebuild(&world, |_, _, _| 0x6eaf).unwrap();
        assert_eq!(patches.len(), 8);
        assert!(patches.iter().any(|patch| {
            patch
                .mesh
                .iter()
                .any(|quad| quad.words[1] & 0x0300_0000 != 0)
        }));
        for patch in patches {
            assert!(patch.is_current(&world));
            assert!(
                patch
                    .mesh
                    .iter()
                    .all(|quad: &QuadDescriptor| quad.size() == [1, 1])
            );
            let mut composed = expanded(&old[&patch.page], Some(&patch.mask));
            for (key, value) in expanded(&patch.mesh, None) {
                assert!(composed.insert(key, value).is_none());
            }
            assert_eq!(
                composed,
                expanded(&full_mesh(&world, patch.page), None),
                "page {:?}",
                patch.page
            );
        }
    }

    fn rebase_scene() -> crate::ClientScene {
        let config = crate::SceneConfig {
            center: pos(8, 8, 8),
            horizontal_radius: 0,
            vertical_radius: 0,
            coherent_rebasing: true,
            ..crate::SceneConfig::forest()
        };
        let mut scene = crate::ClientScene::new(config).unwrap();
        let pages: Vec<_> = scene.requested_pages().collect();
        let mut tx = Transaction::new(Tick(0), SpaceId(0));
        for page in pages {
            tx.create_page(page, vec![Cell::AIR; 4096]);
        }
        tx.set_cell(pos(15, 15, 15), Cell::from_parts(rules::ids::STONE, 0));
        tx.set_cell(pos(14, 15, 15), Cell::from_parts(rules::ids::STONE, 0));
        scene.apply_transaction(tx).unwrap();
        scene
    }
    fn finish_old_base(scene: &mut crate::ClientScene, dispatch: jobs::Dispatch<crate::SceneTask>) {
        let id = dispatch.id;
        let result = dispatch.payload.execute(&dispatch.cancellation).unwrap();
        scene
            .complete(jobs::Completion {
                id,
                outcome: jobs::JobOutcome::Success(result),
            })
            .unwrap();
        scene.phase(Tick(4), 32).unwrap();
    }
    #[test]
    fn certified_old_base_plus_postcapture_delta_equals_current_geometry() {
        let mut scene = rebase_scene();
        scene.phase(Tick(0), 32).unwrap();
        let job = scene
            .take_task(Tick(0), &mut crate::ClientScene::default_budget())
            .unwrap();
        assert!(matches!(job.payload, crate::SceneTask::Mesh(_)));
        let mut tx = Transaction::new(Tick(1), SpaceId(0));
        tx.set_cell(pos(15, 15, 15), Cell::AIR);
        tx.set_cell(pos(16, 16, 16), Cell::from_parts(rules::ids::STONE, 0));
        scene.apply_transaction(tx).unwrap();
        assert!(!job.cancellation.is_cancelled());
        finish_old_base(&mut scene, job);
        let composite = scene.take_composite().unwrap();
        assert!(!scene.world().matches_inputs(&composite.base.inputs));
        assert!(scene.composite_is_current(&composite));
        let patch = build_page_patch(
            composite.base.page,
            &composite.changed_owners,
            scene.world(),
            &|_, _, _| u16::MAX,
        )
        .unwrap();
        let mut composed = expanded(&composite.base.mesh, Some(&composite.changed_owners));
        for (key, value) in expanded(&patch.mesh, None) {
            assert!(composed.insert(key, value).is_none());
        }
        assert_eq!(
            composed,
            expanded(&full_mesh(scene.world(), composite.base.page), None)
        );
        assert!(scene.ack_composite(&composite));
        // There is still a pending current-base rebuild; old versions were never
        // relabelled current to obtain the visual result.
        assert!(!scene.is_ready());
        scene.phase(Tick(4), 32).unwrap();
        assert!(
            scene
                .take_task(Tick(4), &mut crate::ClientScene::default_budget())
                .is_some()
        );
    }
    #[test]
    fn budget_return_retains_new_edits_and_revokes_taken_certificate() {
        let mut scene = rebase_scene();
        scene.phase(Tick(0), 32).unwrap();
        let job = scene
            .take_task(Tick(0), &mut crate::ClientScene::default_budget())
            .unwrap();
        finish_old_base(&mut scene, job);
        let first = scene.take_composite().unwrap();
        assert!(first.changed_owners.is_empty());
        let mut tx = Transaction::new(Tick(4), SpaceId(0));
        tx.set_cell(pos(8, 8, 8), Cell::from_parts(rules::ids::LOG, 1));
        scene.apply_transaction(tx).unwrap();
        assert!(!scene.composite_is_current(&first));
        assert!(!scene.ack_composite(&first));
        scene.return_composite(first).unwrap();
        let second = scene.take_composite().unwrap();
        assert_eq!(second.changed_owners.len(), 27);
        assert!(scene.composite_is_current(&second));
        assert!(scene.ack_composite(&second));
    }
    #[test]
    fn structural_replacement_cannot_be_certified_as_sparse_rebase() {
        let mut scene = rebase_scene();
        scene.phase(Tick(0), 32).unwrap();
        let job = scene
            .take_task(Tick(0), &mut crate::ClientScene::default_budget())
            .unwrap();
        let mut tx = Transaction::new(Tick(1), SpaceId(0));
        tx.remove_page(pos(16, 16, 16).page());
        scene.apply_transaction(tx).unwrap();
        assert!(job.cancellation.is_cancelled());
        scene.cancelled(job.id).unwrap();
        scene.phase(Tick(4), 32).unwrap();
        assert!(scene.take_composite().is_none());
    }
    #[test]
    fn incremental_work_depends_on_new_edits_not_the_retained_mask() {
        let mut world = fixture(0..=0);
        let mut shell = EditShell::new(1, 4096);
        let mut tx = Transaction::new(Tick(1), SpaceId(0));
        for y in (2..14).step_by(3) {
            for z in (2..14).step_by(3) {
                for x in (2..14).step_by(3) {
                    tx.set_cell(pos(x, y, z), Cell::from_parts(rules::ids::STONE, 0));
                }
            }
        }
        shell.mark_changes(&world.commit(tx).unwrap()).unwrap();
        assert!(shell.owner_cells() > 1000);
        let first = shell
            .rebuild_owner_updates(&world, |_, _, _| u16::MAX)
            .unwrap();
        assert!(shell.ack_owner_updates(&first[0], &world));
        let retained = shell.owner_cells();
        let mut tx = Transaction::new(Tick(2), SpaceId(0));
        tx.set_cell(pos(8, 8, 8), Cell::AIR);
        shell.mark_changes(&world.commit(tx).unwrap()).unwrap();
        let second = shell
            .rebuild_owner_updates(&world, |_, _, _| u16::MAX)
            .unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].updates.len(), 27);
        assert_eq!(second[0].work.work.cells_scanned, 729);
        assert_eq!(shell.owner_cells(), retained);
        assert_eq!(
            shell
                .rebuild_owner_updates(&world, |_, _, _| u16::MAX)
                .unwrap()[0]
                .updates
                .len(),
            27,
            "budget retry must retain all pending updates"
        );
        assert!(shell.ack_owner_updates(&second[0], &world));
        assert!(
            shell
                .rebuild_owner_updates(&world, |_, _, _| u16::MAX)
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn stale_incremental_ack_cannot_drop_later_edits_or_missing_old_sources() {
        let mut world = fixture(0..=1);
        let mut shell = EditShell::default();
        let mut tx = Transaction::new(Tick(1), SpaceId(0));
        tx.set_cell(pos(15, 8, 8), Cell::from_parts(rules::ids::STONE, 0));
        shell.mark_changes(&world.commit(tx).unwrap()).unwrap();
        let old = shell
            .rebuild_owner_updates(&world, |_, _, _| u16::MAX)
            .unwrap();
        let mut tx = Transaction::new(Tick(2), SpaceId(0));
        tx.set_cell(pos(4, 4, 4), Cell::from_parts(rules::ids::STONE, 0));
        shell.mark_changes(&world.commit(tx).unwrap()).unwrap();
        assert!(!shell.ack_owner_updates(
            old.iter().find(|u| u.page == pos(4, 4, 4).page()).unwrap(),
            &world
        ));
        let current = shell
            .rebuild_owner_updates(&world, |_, _, _| u16::MAX)
            .unwrap();
        for update in &current {
            assert!(shell.ack_owner_updates(update, &world));
        }
        let mut tx = Transaction::new(Tick(3), SpaceId(0));
        tx.remove_page(pos(16, 8, 8).page());
        world.commit(tx).unwrap();
        shell.mark_cells(&[pos(4, 4, 4)]).unwrap();
        assert!(
            matches!(
                shell.rebuild_owner_updates(&world, |_, _, _| u16::MAX),
                Err(ShellError::UnknownPage(_))
            ),
            "old masked dependency must remain certified even if the latest edit uses only the centre page"
        );
    }
    #[test]
    fn persistent_owner_slots_match_full_geometry_after_a_hundred_edits() {
        use render::terrain_mesh::MeshClass;
        let mut world = fixture(-1..=1);
        let mut shell = EditShell::new(1, 4096);
        let mut slots = BTreeMap::<u16, OwnerPatch>::new();
        for sequence in 0..100 {
            let rng = core_types::DeterministicRng::from_seed(sequence).next_u64();
            let point = pos(
                2 + (rng % 12) as i32,
                2 + ((rng >> 16) % 12) as i32,
                2 + ((rng >> 32) % 12) as i32,
            );
            let new = if world.read(point) == CellRead::Known(Cell::AIR) {
                Cell::from_parts(
                    if sequence % 3 == 0 {
                        rules::ids::LOG
                    } else {
                        rules::ids::STONE
                    },
                    u16::from(sequence % 3 == 0),
                )
            } else {
                Cell::AIR
            };
            let mut tx = Transaction::new(Tick(sequence + 1), SpaceId(0));
            tx.set_cell(point, new);
            shell.mark_changes(&world.commit(tx).unwrap()).unwrap();
            let updates = shell
                .rebuild_owner_updates(&world, |_, _, _| u16::MAX)
                .unwrap();
            for update in &updates {
                for patch in &update.updates {
                    slots.insert(patch.owner, *patch);
                }
                assert!(shell.ack_owner_updates(update, &world));
            }
        }
        let mut combined = PageMesh::default();
        for patch in slots.values() {
            for face in 0..6 {
                match patch.classes[face] {
                    MeshClass::Empty => {}
                    MeshClass::Opaque => combined.opaque.push(patch.quads[face]),
                    MeshClass::Cutout => combined.cutout.push(patch.quads[face]),
                    MeshClass::Water => combined.water.push(patch.quads[face]),
                }
            }
        }
        assert_eq!(
            expanded(&combined, None),
            expanded(&full_mesh(&world, pos(8, 8, 8).page()), None)
        );
    }
}
