//! Server-only immutable exact-column projection. A far request never creates a
//! World page, wakes a fluid/mob, or seeds LIGHT. Private canonical features are
//! evaluated only in the worker and only sanitized public data leaves it.
#[path = "cold_projection/witness.rs"]
mod witness;
use crate::{
    height_index::{HeightEditIndex, TileEdits, TileKey},
    lod,
};
use core_types::{BlockPos, Cell, ChannelMask, LocalIdx, PageId, SpaceId};
use gen_authority::ServerGenerator;
use std::{collections::BTreeMap, sync::Arc};
pub use witness::ContentWitness;
use world::{Page, PageRegion, SampledPageInput, World};
pub const SOURCE_PAGES: usize = 90;
pub const COLUMN_PAGES: usize = 8;
const CHANNELS: ChannelMask = crate::far_water::capture::CHANNELS;
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ColumnKey {
    pub x: i32,
    pub z: i32,
}
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Legacy resident worlds use the same public policy without creating a
    /// private canonical baseline for unavailable gameplay pages.
    pub allow_cold: bool,
    pub max_edit_cells: usize,
    pub max_edit_bytes: usize,
    pub max_output_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            allow_cold: true,
            max_edit_cells: 65536,
            max_edit_bytes: 8 * 1024 * 1024,
            max_output_bytes: 512 * 1024,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Bounds,
    IndexNotReady,
    Capacity,
    Cancelled,
    Generation,
    Encoding,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Work {
    pub snapshot_lookups: u64,
    pub index_captures: u64,
    pub captured_edit_cells: u64,
    pub captured_edit_bytes: u64,
    pub generated_columns: u64,
    pub public_baseline_columns: u64,
    pub public_baseline_scratch_reads: u64,
    pub public_baseline_scratch_writes: u64,
    pub tint_samples: u64,
    pub public_witness_bytes: u64,
    pub exterior: crate::public_projection::Work,
    pub generation_scratch_reads: u64,
    pub generation_scratch_writes: u64,
    pub projected_geometry_cells: u64,
    pub halo_samples: u64,
    pub resident_cell_reads: u64,
    pub cold_cell_reads: u64,
    pub unknown_samples: u64,
    pub output_bytes: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Resident,
    Cold,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Source {
    input: SampledPageInput,
    kind: Kind,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Certificate {
    pub column: ColumnKey,
    sources: [Source; SOURCE_PAGES],
    indices: [(TileKey, u64); 9],
    geometry_edits: [u64; 8],
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GeometrySignature {
    source: Source,
    edit_version: u64,
}
impl Certificate {
    /// Centre input signature. Exterior-policy equivalence additionally needs
    /// the complete certificate or an exact retained public-body comparison.
    pub fn geometry_signature(&self, page: PageId) -> Option<GeometrySignature> {
        if page.space != SpaceId(0)
            || page.x != self.column.x
            || page.z != self.column.z
            || !(0..8).contains(&page.y)
        {
            return None;
        }
        Some(GeometrySignature {
            source: self.sources[source_index(0, page.y, 0)],
            edit_version: self.geometry_edits[page.y as usize],
        })
    }
    pub fn matches(&self, world: &World, index: &HeightEditIndex) -> bool {
        index.is_ready()
            && self
                .indices
                .iter()
                .all(|&(key, version)| index.source(key) == Ok(version))
            && self.sources.iter().all(|source| {
                let present = world.page(source.input.page).is_some();
                present == (source.kind == Kind::Resident)
                    && world.sampled_input_version(source.input.page, CHANNELS, source.input.region)
                        == source.input.version
                    && (source.kind != Kind::Cold || world.page_version(source.input.page).0 == 0)
            })
    }
    pub fn inputs(&self) -> impl Iterator<Item = SampledPageInput> + '_ {
        self.sources.iter().map(|s| s.input)
    }
    pub fn index_sources(&self) -> &[(TileKey, u64); 9] {
        &self.indices
    }
}
pub struct ColumnSnapshot {
    pub column: ColumnKey,
    certificate: Certificate,
    pages: [Option<Arc<Page>>; SOURCE_PAGES],
    edits: [TileEdits; 9],
    limits: Limits,
    work: Work,
}
pub struct ProjectedPage {
    pub page: PageId,
    pub geometry: Option<lod::PublicGeometryData>,
    /// Sealed, ore-redacted server-only values for the per-observer near view.
    pub sealed_cells: Option<Box<[Cell; 4096]>>,
    pub water: Box<[u8; 5832]>,
    pub witness: ContentWitness,
}
pub struct BuiltColumn {
    pub column: ColumnKey,
    pub certificate: Certificate,
    pub pages: [ProjectedPage; COLUMN_PAGES],
    pub tints: [u16; 256],
    pub work: Work,
}
impl BuiltColumn {
    /// Owned logical heap plus this result box; allocator headers are excluded.
    /// Unlike wire payload bytes this includes palette capacity/padding and the
    /// private fixed certificate, but no captured World snapshots survive.
    pub fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self
                .pages
                .iter()
                .map(|page| {
                    5832 + page
                        .sealed_cells
                        .as_ref()
                        .map_or(0, |_| 4096 * std::mem::size_of::<Cell>())
                        + page.geometry.as_ref().map_or(0, |geometry| {
                            geometry.palette.capacity() * std::mem::size_of::<lod::PublicSurface>()
                                + geometry.indices.capacity()
                        })
                })
                .sum::<usize>()
    }
}
impl ColumnSnapshot {
    /// Captures only bounded source references and the nine indexed edited
    /// footprints. Caller supplies the durable frontier, never an unfsynced view.
    pub fn capture(
        world: &World,
        index: &HeightEditIndex,
        column: ColumnKey,
        limits: Limits,
    ) -> Result<Self, Error> {
        if !index.is_ready() {
            return Err(Error::IndexNotReady);
        }
        if limits.max_edit_cells > 1_000_000
            || limits.max_edit_bytes > 16 * 1024 * 1024
            || limits.max_output_bytes > 4 * 1024 * 1024
            || limits.max_output_bytes < 8 * 5832
        {
            return Err(Error::Bounds);
        }
        for coordinate in [column.x, column.z] {
            coordinate
                .checked_mul(16)
                .and_then(|v| v.checked_sub(17))
                .ok_or(Error::Bounds)?;
            coordinate
                .checked_mul(16)
                .and_then(|v| v.checked_add(32))
                .ok_or(Error::Bounds)?;
        }
        let pages = std::array::from_fn(|i| world.snapshot(source_page(column, i)));
        let sources = std::array::from_fn(|i| {
            let page = source_page(column, i);
            let [dx, y, dz] = source_offset(i);
            let input = world.capture_sampled_input(
                page,
                CHANNELS,
                PageRegion::new([
                    if (0..8).contains(&y) { 0 } else { -dx as i8 },
                    if y == -1 {
                        1
                    } else if y == 8 {
                        -1
                    } else {
                        0
                    },
                    if (0..8).contains(&y) { 0 } else { -dz as i8 },
                ])
                .expect("column halo region"),
            );
            let kind = if pages[i].is_some() {
                Kind::Resident
            } else if limits.allow_cold
                && (0..8).contains(&page.y)
                && world.page_version(page).0 == 0
            {
                Kind::Cold
            } else {
                Kind::Unknown
            };
            Source { input, kind }
        });
        let mut work = Work {
            snapshot_lookups: SOURCE_PAGES as u64,
            ..Work::default()
        };
        let mut captured = Vec::with_capacity(9);
        for dz in -1..=1 {
            for dx in -1..=1 {
                let key = TileKey {
                    space: SpaceId(0),
                    level: 0,
                    x: column.x + dx,
                    z: column.z + dz,
                };
                let pages = (0..8).fold(0u8, |mask, y| {
                    mask | (u8::from(sources[source_index(dx, y, dz)].kind == Kind::Cold) << y)
                });
                let edits = index
                    .capture_body_pages(
                        key,
                        pages,
                        limits
                            .max_edit_cells
                            .saturating_sub(work.captured_edit_cells as usize),
                    )
                    .map_err(|_| Error::Capacity)?;
                work.index_captures += 1;
                work.captured_edit_cells += edits.visited_cells as u64;
                work.captured_edit_bytes += edit_bytes(&edits) as u64;
                if work.captured_edit_cells > limits.max_edit_cells as u64
                    || work.captured_edit_bytes > limits.max_edit_bytes as u64
                {
                    return Err(Error::Capacity);
                }
                captured.push(edits);
            }
        }
        let edits: [TileEdits; 9] = captured.try_into().map_err(|_| Error::Encoding)?;
        let indices = std::array::from_fn(|i| (edits[i].key, edits[i].source));
        let mut geometry_edits = [0; 8];
        for source in &edits[4].columns {
            if source.x.div_euclid(16) != column.x || source.z.div_euclid(16) != column.z {
                continue;
            }
            for cell in &source.cells {
                if (0..128).contains(&cell.y) {
                    let version = &mut geometry_edits[(cell.y / 16) as usize];
                    *version = (*version).max(cell.source_tx.0);
                }
            }
        }
        Ok(Self {
            column,
            certificate: Certificate {
                column,
                sources,
                indices,
                geometry_edits,
            },
            pages,
            edits,
            limits,
            work,
        })
    }
    pub fn certificate(&self) -> &Certificate {
        &self.certificate
    }
    pub fn capture_work(&self) -> Work {
        self.work
    }
    /// Source references do not copy or charge the existing World allocation.
    /// Jobs additionally pin at most90 immutable pages until execute completes.
    pub fn owned_input_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self
                .edits
                .iter()
                .map(|edits| edit_bytes(edits) - std::mem::size_of::<TileEdits>())
                .sum::<usize>()
    }
    pub fn execute(
        self,
        generator: &ServerGenerator,
        cancel: impl Fn() -> bool,
    ) -> Result<Box<BuiltColumn>, Error> {
        if cancel() {
            return Err(Error::Cancelled);
        }
        let mut work = self.work;
        let mut generated: [Option<r#gen::GeneratedColumn>; 9] = std::array::from_fn(|_| None);
        for (i, slot) in generated.iter_mut().enumerate() {
            let needed = self
                .certificate
                .sources
                .iter()
                .any(|s| s.kind == Kind::Cold && column_index(self.column, s.input.page) == i);
            if !needed {
                continue;
            }
            if cancel() {
                return Err(Error::Cancelled);
            }
            let dx = i as i32 % 3 - 1;
            let dz = i as i32 / 3 - 1;
            let data = generator
                .generate_column(self.column.x + dx, self.column.z + dz)
                .map_err(|_| Error::Generation)?;
            work.generated_columns += 1;
            work.generation_scratch_reads += data.work.scratch_reads;
            work.generation_scratch_writes += data.work.scratch_writes;
            *slot = Some(data);
        }
        let mut overrides = BTreeMap::new();
        for tile in &self.edits {
            for column in &tile.columns {
                if column.x.div_euclid(16) != tile.key.x || column.z.div_euclid(16) != tile.key.z {
                    continue;
                }
                for edit in &column.cells {
                    let p = BlockPos {
                        space: SpaceId(0),
                        x: column.x,
                        y: edit.y,
                        z: column.z,
                    };
                    if overrides
                        .insert((p.page(), p.local_index()), edit.cell)
                        .is_some()
                    {
                        return Err(Error::Encoding);
                    }
                }
            }
        }
        let mut reader = Read {
            snapshot: &self,
            generated: &generated,
            overrides: &overrides,
            resident: 0,
            cold: 0,
            unknown: 0,
        };
        let mut public_columns = Vec::with_capacity(9);
        for i in 0..9 {
            if cancel() {
                return Err(Error::Cancelled);
            }
            let data = generator
                .terrain()
                .generate_column(self.column.x + i % 3 - 1, self.column.z + i / 3 - 1)
                .map_err(|_| Error::Generation)?;
            work.public_baseline_columns += 1;
            work.public_baseline_scratch_reads += data.work.scratch_reads;
            work.public_baseline_scratch_writes += data.work.scratch_writes;
            public_columns.push(data);
        }
        let public_cell = |position: [i32; 3]| -> Cell {
            if !(0..128).contains(&position[1]) {
                return Cell::AIR;
            }
            let page = PageId {
                space: SpaceId(0),
                x: position[0].div_euclid(16),
                y: position[1].div_euclid(16),
                z: position[2].div_euclid(16),
            };
            Cell::from_parts(
                public_columns[column_index(self.column, page)].block_padded(
                    position[0].rem_euclid(16) + 1,
                    position[1],
                    position[2].rem_euclid(16) + 1,
                ),
                0,
            )
        };
        let projection = crate::public_projection::PublicProjection::build(
            [self.column.x * 16 - 16, self.column.z * 16 - 16],
            |position| reader.position(position),
            public_cell,
            &cancel,
        )
        .map_err(|_| Error::Cancelled)?;
        work.exterior = projection.work();
        let tints = std::array::from_fn(|i| {
            generator
                .terrain()
                .column_info(
                    self.column.x * 16 + (i % 16) as i32,
                    self.column.z * 16 + (i / 16) as i32,
                )
                .tint
        });
        work.tint_samples += 256;
        let mut output = Vec::with_capacity(8);
        for y in 0..8 {
            if cancel() {
                return Err(Error::Cancelled);
            }
            let page = PageId {
                space: SpaceId(0),
                x: self.column.x,
                y,
                z: self.column.z,
            };
            let known = self.certificate.sources[source_index(0, y, 0)].kind != Kind::Unknown;
            let sealed_cells = if known {
                let mut cells = Box::new([Cell::AIR; 4096]);
                for (i, cell) in cells.iter_mut().enumerate() {
                    let at = LocalIdx::new(i as u16).expect("page cell");
                    let (x, y, z) = at.xyz();
                    let position = [
                        page.x * 16 + i32::from(x),
                        page.y * 16 + i32::from(y),
                        page.z * 16 + i32::from(z),
                    ];
                    let actual = reader.cell(page, at).expect("known centre source");
                    *cell = projection.project(position, actual, public_cell(position));
                }
                Some(cells)
            } else {
                None
            };
            let geometry = sealed_cells
                .as_ref()
                .map(|cells| lod::geometry_from_cells(cells, Some(&tints)));
            work.projected_geometry_cells += u64::from(known) * 4096;
            let mut water = Box::new([0; 5832]);
            for ly in -1i32..=16 {
                if cancel() {
                    return Err(Error::Cancelled);
                }
                for lz in -1i32..=16 {
                    for lx in -1i32..=16 {
                        let index =
                            (lx + 1) as usize + 18 * ((lz + 1) as usize + 18 * (ly + 1) as usize);
                        let source = PageId {
                            space: SpaceId(0),
                            x: page.x + lx.div_euclid(16),
                            y: page.y + ly.div_euclid(16),
                            z: page.z + lz.div_euclid(16),
                        };
                        let at = LocalIdx::new(
                            (lx.rem_euclid(16) + 16 * lz.rem_euclid(16) + 256 * ly.rem_euclid(16))
                                as u16,
                        )
                        .expect("halo local");
                        let position = [page.x * 16 + lx, page.y * 16 + ly, page.z * 16 + lz];
                        water[index] = reader.cell(source, at).map_or(0, |actual| {
                            crate::far_water::capture::classify(projection.project(
                                position,
                                actual,
                                public_cell(position),
                            ))
                        });
                        work.halo_samples += 1;
                    }
                }
            }
            work.output_bytes +=
                geometry.as_ref().map_or(0, |g| g.wire_payload_bytes()) as u64 + 5832;
            if work.output_bytes > self.limits.max_output_bytes as u64 {
                return Err(Error::Capacity);
            }
            let (witness, bytes) = witness::build(geometry.as_ref(), &water, &tints);
            work.public_witness_bytes += bytes;
            output.push(ProjectedPage {
                page,
                geometry,
                sealed_cells,
                water,
                witness,
            });
        }
        work.resident_cell_reads = reader.resident;
        work.cold_cell_reads = reader.cold;
        work.unknown_samples = reader.unknown;
        work.output_bytes += 512;
        if work.output_bytes > self.limits.max_output_bytes as u64 {
            return Err(Error::Capacity);
        }
        Ok(Box::new(BuiltColumn {
            column: self.column,
            certificate: self.certificate,
            pages: output.try_into().map_err(|_| Error::Encoding)?,
            tints,
            work,
        }))
    }
}
struct Read<'a> {
    snapshot: &'a ColumnSnapshot,
    generated: &'a [Option<r#gen::GeneratedColumn>; 9],
    overrides: &'a BTreeMap<(PageId, LocalIdx), Cell>,
    resident: u64,
    cold: u64,
    unknown: u64,
}
impl Read<'_> {
    fn position(&mut self, position: [i32; 3]) -> Option<Cell> {
        let position = BlockPos {
            space: SpaceId(0),
            x: position[0],
            y: position[1],
            z: position[2],
        };
        self.cell(position.page(), position.local_index())
    }
    fn cell(&mut self, page: PageId, at: LocalIdx) -> Option<Cell> {
        if !(-1..=8).contains(&page.y) {
            self.unknown += 1;
            return None;
        }
        let dx = page.x - self.snapshot.column.x;
        let dz = page.z - self.snapshot.column.z;
        if !(-1..=1).contains(&dx) || !(-1..=1).contains(&dz) {
            self.unknown += 1;
            return None;
        }
        let i = source_index(dx, page.y, dz);
        match self.snapshot.certificate.sources[i].kind {
            Kind::Resident => {
                self.resident += 1;
                Some(
                    self.snapshot.pages[i]
                        .as_ref()
                        .expect("resident snapshot")
                        .cell(at),
                )
            }
            Kind::Unknown => {
                self.unknown += 1;
                None
            }
            Kind::Cold => {
                self.cold += 1;
                if let Some(&cell) = self.overrides.get(&(page, at)) {
                    return Some(cell);
                }
                let (x, y, z) = at.xyz();
                let generated = self.generated[column_index(self.snapshot.column, page)]
                    .as_ref()
                    .expect("generated cold column");
                Some(Cell::from_parts(
                    generated.block_padded(
                        i32::from(x) + 1,
                        page.y * 16 + i32::from(y),
                        i32::from(z) + 1,
                    ),
                    0,
                ))
            }
        }
    }
}
fn source_offset(i: usize) -> [i32; 3] {
    [i as i32 % 3 - 1, i as i32 / 9 - 1, i as i32 / 3 % 3 - 1]
}
fn source_page(column: ColumnKey, i: usize) -> PageId {
    let [dx, y, dz] = source_offset(i);
    PageId {
        space: SpaceId(0),
        x: column.x + dx,
        y,
        z: column.z + dz,
    }
}
fn source_index(dx: i32, y: i32, dz: i32) -> usize {
    (dx + 1 + 3 * (dz + 1) + 9 * (y + 1)) as usize
}
fn column_index(column: ColumnKey, page: PageId) -> usize {
    (page.x - column.x + 1 + 3 * (page.z - column.z + 1)) as usize
}
fn edit_bytes(edits: &TileEdits) -> usize {
    std::mem::size_of::<TileEdits>()
        + edits.columns.capacity() * std::mem::size_of::<crate::height_index::EditedColumn>()
        + edits
            .columns
            .iter()
            .map(|c| c.cells.capacity() * std::mem::size_of::<crate::height_index::IndexedCell>())
            .sum::<usize>()
}
