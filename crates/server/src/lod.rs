//! Pure worker-side public geometry summaries for selected protected leaves.
//! This module never reads World, generates terrain, or depends on a renderer.
//! Every ore is replaced with host stone before visual material selection.
use core_types::LocalIdx;
use std::collections::BTreeMap;
use world::Page;
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PublicSurfaceKind {
    Opaque,
    Cutout,
    Water,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PublicSurface {
    pub atlas_layer: u16,
    pub tint: u32,
    pub kind: PublicSurfaceKind,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicGeometryData {
    pub known: [u8; 512],
    pub occupied: [u8; 512],
    pub palette: Vec<PublicSurface>,
    pub index_bits: u8,
    /// One palette index per local cell, X then Z then Y. Empty/Unknown indices
    /// are canonical zero. Occupancy/known masks distinguish them from palette 0.
    pub indices: Vec<u8>,
    pub scanned_cells: u64,
}
impl PublicGeometryData {
    pub fn palette_index(&self, index: LocalIdx) -> u16 {
        if self.index_bits == 0 {
            return 0;
        }
        let bit = usize::from(index.get()) * usize::from(self.index_bits);
        let byte = bit / 8;
        let shift = bit % 8;
        let mut value = u32::from(self.indices[byte]);
        if byte + 1 < self.indices.len() {
            value |= u32::from(self.indices[byte + 1]) << 8;
        }
        if byte + 2 < self.indices.len() {
            value |= u32::from(self.indices[byte + 2]) << 16;
        }
        ((value >> shift) & ((1 << self.index_bits) - 1)) as u16
    }
    pub fn wire_payload_bytes(&self) -> usize {
        1024 + self.palette.len() * 7 + self.indices.len()
    }
}
pub fn geometry_from_page(page: &Page, public_tints: Option<&[u16; 256]>) -> PublicGeometryData {
    geometry_from_read(|index| Some(page.cell(index)), public_tints)
}
pub fn geometry_from_cells(
    cells: &[core_types::Cell; 4096],
    public_tints: Option<&[u16; 256]>,
) -> PublicGeometryData {
    geometry_from_read(|index| Some(cells[usize::from(index.get())]), public_tints)
}
/// Shared public policy for resident and cold immutable sources. Unknown stays
/// outside the known mask; private ore IDs always become host stone.
pub fn geometry_from_read(
    mut read: impl FnMut(LocalIdx) -> Option<core_types::Cell>,
    public_tints: Option<&[u16; 256]>,
) -> PublicGeometryData {
    let mut out = PublicGeometryData {
        known: [0; 512],
        occupied: [0; 512],
        palette: Vec::new(),
        index_bits: 0,
        indices: Vec::new(),
        scanned_cells: 4096,
    };
    let mut lookup = BTreeMap::new();
    let mut indices = [0u16; 4096];
    for (index, output_palette) in indices.iter_mut().enumerate() {
        let at = LocalIdx::new(index as u16).expect("page index");
        let Some(mut cell) = read(at) else { continue };
        cell = redact_private_material(cell);
        let Some(definition) = rules::block(cell.id()) else {
            continue;
        };
        out.known[index / 8] |= 1 << (index % 8);
        let kind = match definition.render {
            rules::blocks::Render::None
            | rules::blocks::Render::Cross
            | rules::blocks::Render::Model(_) => continue,
            rules::blocks::Render::Cube => PublicSurfaceKind::Opaque,
            rules::blocks::Render::Leaves | rules::blocks::Render::Glass => {
                PublicSurfaceKind::Cutout
            }
            rules::blocks::Render::Water => PublicSurfaceKind::Water,
        };
        out.occupied[index / 8] |= 1 << (index % 8);
        let (x, _, z) = at.xyz();
        let face = rules::blocks::mesh_face(1);
        let tint = if rules::blocks::tinted(cell.id(), face) {
            let rgb565 = public_tints.map_or(u16::MAX, |tints| {
                tints[usize::from(x) + 16 * usize::from(z)]
            });
            let r = u32::from((rgb565 >> 11) & 31) * 255 / 31;
            let g = u32::from((rgb565 >> 5) & 63) * 255 / 63;
            let b = u32::from(rgb565 & 31) * 255 / 31;
            r | (g << 8) | (b << 16) | 0xff000000
        } else {
            u32::MAX
        };
        let surface = PublicSurface {
            atlas_layer: rules::blocks::layer_cell(cell.id(), cell.state(), face).0 as u16,
            tint,
            kind,
        };
        let palette = *lookup.entry(surface).or_insert_with(|| {
            let index = out.palette.len() as u16;
            out.palette.push(surface);
            index
        });
        *output_palette = palette;
    }
    out.index_bits = if out.palette.len() <= 1 {
        0
    } else {
        (32 - (out.palette.len() as u32 - 1).leading_zeros()) as u8
    };
    out.indices
        .resize((4096 * usize::from(out.index_bits)).div_ceil(8), 0);
    for (index, palette) in indices.into_iter().enumerate() {
        let bit = index * usize::from(out.index_bits);
        for offset in 0..out.index_bits {
            if palette & (1 << offset) != 0 {
                let bit = bit + usize::from(offset);
                out.indices[bit / 8] |= 1 << (bit % 8);
            }
        }
    }
    out
}
pub(crate) fn redact_private_material(cell: core_types::Cell) -> core_types::Cell {
    if ore(cell.id()) {
        core_types::Cell::from_parts(rules::ids::STONE, 0)
    } else {
        cell
    }
}
fn ore(id: u16) -> bool {
    matches!(
        id,
        rules::ids::COAL_ORE
            | rules::ids::IRON_ORE
            | rules::ids::OSMIUM_ORE
            | rules::ids::TIN_ORE
            | rules::ids::COPPER_ORE
            | rules::ids::GOLD_ORE
            | rules::ids::REDSTONE_ORE
    )
}
