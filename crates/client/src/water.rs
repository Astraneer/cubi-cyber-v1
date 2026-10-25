//! Worker-only water topology from immutable actual-world snapshots. Unknown
//! boundaries are withheld, never treated as air. No simulation or generation.
use core_types::{Cell, ChannelMask, LocalIdx, PageId, Version};
use render::{
    terrain_mesh::{Face, HALO_VOLUME},
    water_gpu::{WaterError, WaterQuad},
};
use std::sync::Arc;
use world::{Page, PageRegion, SampledPageInput, World};
pub const CHANNELS: ChannelMask =
    ChannelMask(ChannelMask::SHAPE.0 | ChannelMask::OPACITY.0 | ChannelMask::FLUID.0);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceCell {
    Unknown,
    Air,
    Solid,
    Water { amount: u8, falling: bool },
}
impl SurfaceCell {
    pub fn from_cell(cell: Cell) -> Self {
        if rules::block(cell.id()).is_none() {
            return Self::Unknown;
        }
        if let Some(f) = rules::survival::fluid::fluid(cell)
            && f.liquid == rules::survival::fluid::Liquid::Water
        {
            return Self::Water {
                amount: f.amount,
                falling: f.falling,
            };
        }
        if rules::properties(cell).opaque {
            Self::Solid
        } else {
            Self::Air
        }
    }
    fn amount(self) -> Option<f32> {
        if let Self::Water { amount, .. } = self {
            Some(f32::from(amount) / 9.)
        } else {
            None
        }
    }
}
pub struct WaterHalo {
    cells: Box<[SurfaceCell; HALO_VOLUME]>,
}
impl WaterHalo {
    pub fn from_fn(mut f: impl FnMut(i8, i8, i8) -> SurfaceCell) -> Self {
        let mut cells = Box::new([SurfaceCell::Unknown; HALO_VOLUME]);
        for y in -1..=16 {
            for z in -1..=16 {
                for x in -1..=16 {
                    cells[index(x, y, z)] = f(x, y, z);
                }
            }
        }
        Self { cells }
    }
    pub fn get(&self, x: i8, y: i8, z: i8) -> SurfaceCell {
        if !(-1..=16).contains(&x) || !(-1..=16).contains(&y) || !(-1..=16).contains(&z) {
            SurfaceCell::Unknown
        } else {
            self.cells[index(x, y, z)]
        }
    }
}
fn index(x: i8, y: i8, z: i8) -> usize {
    (x + 1) as usize + 18 * ((z + 1) as usize + 18 * (y + 1) as usize)
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WaterMeshWork {
    pub snapshot_lookups: u64,
    pub halo_samples: u64,
    pub cells_scanned: u64,
    pub quads: u64,
    pub unknown_surface_cells: u64,
    pub cpu_frame_vertices: u64,
}
#[derive(Debug)]
pub enum WaterMeshError {
    CoordinateOverflow,
    MissingCenter,
    Cancelled,
    Capacity,
    Gpu(WaterError),
}
impl std::fmt::Display for WaterMeshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for WaterMeshError {}
pub struct WaterMesh {
    pub quads: Vec<WaterQuad>,
    pub work: WaterMeshWork,
}
/// Quantization is <=0.5/255 block. Shared corners use the same four known
/// columns, including the halo, so adjacent pages compute identical edge levels.
pub fn mesh_water(
    halo: &WaterHalo,
    tints: &[u32; 256],
    max_quads: usize,
    cancel: impl Fn() -> bool,
) -> Result<WaterMesh, WaterMeshError> {
    let mut out = WaterMesh {
        quads: Vec::with_capacity(max_quads.min(1024)),
        work: WaterMeshWork {
            halo_samples: HALO_VOLUME as u64,
            ..WaterMeshWork::default()
        },
    };
    for y in 0..16i8 {
        if cancel() {
            return Err(WaterMeshError::Cancelled);
        }
        for z in 0..16i8 {
            for x in 0..16i8 {
                out.work.cells_scanned += 1;
                let cell = halo.get(x, y, z);
                if !matches!(cell, SurfaceCell::Water { .. }) {
                    continue;
                }
                let mut heights = [0.; 4];
                let mut complete = true;
                for (i, (cx, cz)) in [(x, z), (x + 1, z), (x, z + 1), (x + 1, z + 1)]
                    .into_iter()
                    .enumerate()
                {
                    if let Some(h) = corner_height(halo, cx, y, cz) {
                        heights[i] = h
                    } else {
                        complete = false;
                    }
                }
                if !complete {
                    out.work.unknown_surface_cells += 1;
                    continue;
                }
                let flow = flow(halo, x, y, z);
                let mut solid_sides = 0;
                for face in [Face::North, Face::South, Face::West, Face::East] {
                    let [dx, dy, dz] = face.normal();
                    if halo.get(x + dx, y + dy, z + dz) == SurfaceCell::Solid {
                        solid_sides += 1;
                    }
                }
                let foam =
                    solid_sides as f32 * 0.075 * (flow[0] * flow[0] + flow[1] * flow[1]).sqrt();
                for face in Face::ALL {
                    let [dx, dy, dz] = face.normal();
                    if halo.get(x + dx, y + dy, z + dz) != SurfaceCell::Air {
                        continue;
                    }
                    let (origin, levels) = match face {
                        Face::Down => ([x, y, z], [1.; 4]),
                        Face::Up => (
                            [x, y + 1, z + 1],
                            [heights[2], heights[3], heights[0], heights[1]],
                        ),
                        Face::North => ([x + 1, y, z], [1., 1., heights[1], heights[0]]),
                        Face::South => ([x, y, z + 1], [1., 1., heights[2], heights[3]]),
                        Face::West => ([x, y, z], [1., 1., heights[0], heights[2]]),
                        Face::East => ([x + 1, y, z + 1], [1., 1., heights[3], heights[1]]),
                    };
                    if out.quads.len() >= max_quads {
                        return Err(WaterMeshError::Capacity);
                    }
                    out.quads.push(
                        WaterQuad::new(
                            origin.map(|v| v as u8),
                            face,
                            [1; 2],
                            levels,
                            flow,
                            tints[x as usize + 16 * z as usize],
                            foam,
                        )
                        .map_err(WaterMeshError::Gpu)?,
                    );
                    out.work.quads += 1;
                }
            }
        }
    }
    Ok(out)
}
fn corner_height(halo: &WaterHalo, x: i8, y: i8, z: i8) -> Option<f32> {
    let cells = [(x - 1, z - 1), (x, z - 1), (x - 1, z), (x, z)];
    for (cx, cz) in cells {
        if matches!(halo.get(cx, y, cz), SurfaceCell::Water { .. })
            && matches!(halo.get(cx, y + 1, cz), SurfaceCell::Water { .. })
        {
            return Some(1.);
        }
    }
    let mut total = 0.;
    let mut weight = 0.;
    for (cx, cz) in cells {
        match halo.get(cx, y, cz) {
            SurfaceCell::Unknown => return None,
            SurfaceCell::Air => weight += 1.,
            SurfaceCell::Solid => {}
            SurfaceCell::Water { amount, .. } => {
                if halo.get(cx, y + 1, cz) == SurfaceCell::Unknown {
                    return None;
                }
                let w = if amount == 8 { 10. } else { 1. };
                total += f32::from(amount) / 9. * w;
                weight += w;
            }
        }
    }
    Some(if weight > 0. { total / weight } else { 0. })
}
fn flow(halo: &WaterHalo, x: i8, y: i8, z: i8) -> [f32; 2] {
    let center = halo.get(x, y, z).amount().unwrap_or(0.);
    let deficit = |dx, dz| match halo.get(x + dx, y, z + dz) {
        SurfaceCell::Water { .. } => center - halo.get(x + dx, y, z + dz).amount().unwrap_or(0.),
        SurfaceCell::Air => halo
            .get(x + dx, y - 1, z + dz)
            .amount()
            .map_or(center, |h| center + 1. - h),
        _ => 0.,
    };
    let v = [
        deficit(1, 0) - deficit(-1, 0),
        deficit(0, 1) - deficit(0, -1),
    ];
    let length = (v[0] * v[0] + v[1] * v[1]).sqrt();
    if length > 0.00001 {
        v.map(|x| x / length)
    } else {
        [0.; 2]
    }
}
pub struct WaterTask {
    pub page: PageId,
    pub version: Version,
    pub inputs: [SampledPageInput; 27],
    pages: [Option<Arc<Page>>; 27],
    tints: Box<[u32; 256]>,
}
pub struct CompletedWater {
    pub page: PageId,
    pub version: Version,
    pub inputs: [SampledPageInput; 27],
    pub mesh: WaterMesh,
}
impl CompletedWater {
    pub fn current(&self, world: &World) -> bool {
        world.matches_inputs(&self.inputs)
    }
}
impl WaterTask {
    /// Foreground captures exactly27 Arc/options+version stamps; all5832 cell reads
    /// and geometry construction happen in execute on a bounded background worker.
    pub fn capture(
        world: &World,
        page: PageId,
        tints: Box<[u32; 256]>,
    ) -> Result<Self, WaterMeshError> {
        if world.page(page).is_none() {
            return Err(WaterMeshError::MissingCenter);
        }
        let mut pages = Vec::with_capacity(27);
        let mut inputs = Vec::with_capacity(27);
        for dy in -1..=1 {
            for dz in -1..=1 {
                for dx in -1..=1 {
                    let p = PageId {
                        space: page.space,
                        x: page
                            .x
                            .checked_add(dx)
                            .ok_or(WaterMeshError::CoordinateOverflow)?,
                        y: page
                            .y
                            .checked_add(dy)
                            .ok_or(WaterMeshError::CoordinateOverflow)?,
                        z: page
                            .z
                            .checked_add(dz)
                            .ok_or(WaterMeshError::CoordinateOverflow)?,
                    };
                    let region = PageRegion::new([-dx as i8, -dy as i8, -dz as i8])
                        .expect("27 halo directions");
                    inputs.push(world.capture_sampled_input(p, CHANNELS, region));
                    pages.push(world.snapshot(p));
                }
            }
        }
        let inputs: [SampledPageInput; 27] = inputs
            .try_into()
            .unwrap_or_else(|_| unreachable!("fixed27stamps"));
        Ok(Self {
            page,
            version: inputs[13].version,
            inputs,
            pages: pages
                .try_into()
                .unwrap_or_else(|_| unreachable!("fixed27snapshots")),
            tints,
        })
    }
    pub fn execute(
        self,
        max_quads: usize,
        cancel: impl Fn() -> bool,
    ) -> Result<CompletedWater, WaterMeshError> {
        if cancel() {
            return Err(WaterMeshError::Cancelled);
        }
        let halo = WaterHalo::from_fn(|x, y, z| {
            let dx = x.div_euclid(16);
            let dy = y.div_euclid(16);
            let dz = z.div_euclid(16);
            let p = ((dx + 1) + 3 * (dz + 1) + 9 * (dy + 1)) as usize;
            let i = x.rem_euclid(16) as u16
                + 16 * z.rem_euclid(16) as u16
                + 256 * y.rem_euclid(16) as u16;
            self.pages[p].as_ref().map_or(SurfaceCell::Unknown, |page| {
                SurfaceCell::from_cell(page.cell(LocalIdx::new(i).expect("16cube index")))
            })
        });
        let mut mesh = mesh_water(&halo, &self.tints, max_quads, cancel)?;
        mesh.work.snapshot_lookups = 27;
        Ok(CompletedWater {
            page: self.page,
            version: self.version,
            inputs: self.inputs,
            mesh,
        })
    }
}
