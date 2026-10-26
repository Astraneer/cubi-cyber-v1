//! Immutable actual-world input. Only classification runs on the worker; source
//! versions/availability remain private and are checked again by the owner.
use core_types::{ChannelMask, LocalIdx, PageId};
use std::sync::Arc;
use world::{Page, PageRegion, SampledPageInput, World};
pub const HALO_VOLUME: usize = 18 * 18 * 18;
pub const CHANNELS: ChannelMask =
    ChannelMask(ChannelMask::SHAPE.0 | ChannelMask::OPACITY.0 | ChannelMask::FLUID.0);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    MissingCenter,
    CoordinateOverflow,
    Cancelled,
    Capacity,
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
    pub classified_cells: u64,
    pub tint_samples: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Certificate {
    inputs: [SampledPageInput; 27],
    available: u32,
}
impl Certificate {
    pub fn inputs(&self) -> &[SampledPageInput; 27] {
        &self.inputs
    }
    pub fn available(&self, index: usize) -> bool {
        index < 27 && self.available & (1 << index) != 0
    }
    pub fn matches(&self, world: &World) -> bool {
        self.inputs.iter().enumerate().all(|(index, input)| {
            world.page(input.page).is_some() == self.available(index)
                && world.sampled_input_version(input.page, input.channels, input.region)
                    == input.version
        })
    }
}
pub struct Snapshot {
    pub(super) page: PageId,
    pub(super) certificate: Certificate,
    pages: [Option<Arc<Page>>; 27],
}
pub struct Classified {
    pub page: PageId,
    pub cells: Box<[u8; HALO_VOLUME]>,
    pub tints: [u16; 256],
    pub work: Work,
}
impl Snapshot {
    /// 27 snapshot and 27 version lookups, zero cell reads and no generation.
    pub fn capture(world: &World, page: PageId) -> Result<Self, Error> {
        if world.page(page).is_none() {
            return Err(Error::MissingCenter);
        }
        for coordinate in [page.x, page.y, page.z] {
            coordinate.checked_sub(1).ok_or(Error::CoordinateOverflow)?;
            coordinate.checked_add(1).ok_or(Error::CoordinateOverflow)?;
            coordinate
                .checked_mul(16)
                .and_then(|v| v.checked_add(15))
                .ok_or(Error::CoordinateOverflow)?;
        }
        let pages = std::array::from_fn(|index| {
            let target = neighbor(page, index);
            world.snapshot(target)
        });
        let inputs = std::array::from_fn(|index| {
            let [dx, dy, dz] = offset(index);
            world.capture_sampled_input(
                neighbor(page, index),
                CHANNELS,
                PageRegion::new([-dx as i8, -dy as i8, -dz as i8]).expect("27 halo directions"),
            )
        });
        let available = pages
            .iter()
            .enumerate()
            .fold(0, |mask, (i, page)| mask | (u32::from(page.is_some()) << i));
        Ok(Self {
            page,
            certificate: Certificate { inputs, available },
            pages,
        })
    }
    pub fn certificate(&self) -> &Certificate {
        &self.certificate
    }
    pub fn missing(&self) -> impl Iterator<Item = PageId> + '_ {
        self.certificate
            .inputs
            .iter()
            .enumerate()
            .filter_map(|(i, input)| (!self.certificate.available(i)).then_some(input.page))
    }
    pub fn classify(
        self,
        terrain: &r#gen::Generator,
        cancelled: impl Fn() -> bool,
    ) -> Result<Classified, Error> {
        let mut cells = Box::new([0; HALO_VOLUME]);
        let mut work = Work {
            snapshot_lookups: 27,
            ..Work::default()
        };
        for y in -1i32..=16 {
            if cancelled() {
                return Err(Error::Cancelled);
            }
            for z in -1i32..=16 {
                for x in -1i32..=16 {
                    let index = (x + 1) as usize + 18 * ((z + 1) as usize + 18 * (y + 1) as usize);
                    let source = (x.div_euclid(16)
                        + 1
                        + 3 * (z.div_euclid(16) + 1)
                        + 9 * (y.div_euclid(16) + 1)) as usize;
                    let Some(page) = &self.pages[source] else {
                        continue;
                    };
                    let at = LocalIdx::new(
                        (x.rem_euclid(16) + 16 * z.rem_euclid(16) + 256 * y.rem_euclid(16)) as u16,
                    )
                    .expect("halo cell");
                    work.classified_cells += 1;
                    cells[index] = classify(page.cell(at));
                }
            }
        }
        let mut tints = [u16::MAX; 256];
        if self.page.space == core_types::SpaceId(0) {
            for z in 0..16 {
                if cancelled() {
                    return Err(Error::Cancelled);
                }
                for x in 0..16 {
                    tints[x + 16 * z] = terrain
                        .column_info(self.page.x * 16 + x as i32, self.page.z * 16 + z as i32)
                        .tint;
                    work.tint_samples += 1;
                }
            }
        }
        Ok(Classified {
            page: self.page,
            cells,
            tints,
            work,
        })
    }
}
fn offset(index: usize) -> [i32; 3] {
    [
        index as i32 % 3 - 1,
        index as i32 / 9 - 1,
        index as i32 / 3 % 3 - 1,
    ]
}
fn neighbor(page: PageId, index: usize) -> PageId {
    let [dx, dy, dz] = offset(index);
    PageId {
        space: page.space,
        x: page.x + dx,
        y: page.y + dy,
        z: page.z + dz,
    }
}
pub(crate) fn classify(cell: core_types::Cell) -> u8 {
    if rules::block(cell.id()).is_none() {
        return 0;
    }
    if let Some(fluid) = rules::survival::fluid::fluid(cell)
        && fluid.liquid == rules::survival::fluid::Liquid::Water
    {
        return 2 + fluid.amount + if fluid.falling { 8 } else { 0 };
    }
    if rules::properties(cell).opaque { 2 } else { 1 }
}
