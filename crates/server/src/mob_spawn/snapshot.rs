use super::*;
use core_types::ChannelMask;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use world::{Page, World};
pub const CHANNELS: ChannelMask = ChannelMask(
    ChannelMask::COLLISION.0
        | ChannelMask::FLUID.0
        | ChannelMask::OPACITY.0
        | ChannelMask::LIGHTING.0,
);
struct Captured {
    page: Option<Arc<Page>>,
    version: Option<core_types::Version>,
    light_known: bool,
}
pub struct Snapshot {
    space: SpaceId,
    generator: r#gen::Generator,
    pages: BTreeMap<PageId, Captured>,
    observed: BTreeSet<PageId>,
    spawn_protection: Option<([f64; 2], i32)>,
}
impl Snapshot {
    /// The caller explicitly certifies settled authoritative light per page.
    /// Cloning an Arc does not unpack 4096 cells or perform terrain generation.
    pub fn capture(
        world: &World,
        generator: r#gen::Generator,
        space: SpaceId,
        pages: &[(PageId, bool)],
    ) -> Result<Self, Error> {
        if pages.len() > 256 {
            return Err(Error::Bounds);
        }
        let mut captured = BTreeMap::new();
        for &(id, light_known) in pages {
            if id.space != space || captured.contains_key(&id) {
                return Err(Error::Bounds);
            }
            let page = world.snapshot(id);
            let version = page.as_ref().map(|_| world.input_version(id, CHANNELS));
            captured.insert(
                id,
                Captured {
                    page,
                    version,
                    light_known,
                },
            );
        }
        Ok(Self {
            space,
            generator,
            pages: captured,
            observed: BTreeSet::new(),
            spawn_protection: None,
        })
    }
    pub fn with_spawn_protection(mut self, center: [f64; 2], radius: i32) -> Result<Self, Error> {
        if radius < 0
            || center
                .iter()
                .any(|v| !v.is_finite() || v.abs() > 30_000_000.)
        {
            return Err(Error::Bounds);
        }
        self.spawn_protection = Some((center, radius));
        Ok(self)
    }
    pub fn inputs(&self) -> Vec<jobs::InputVersion> {
        self.observed
            .iter()
            .map(|&page| jobs::InputVersion {
                page,
                version: self.pages.get(&page).and_then(|p| p.version),
            })
            .collect()
    }
    pub fn captured_inputs(&self) -> Vec<jobs::InputVersion> {
        self.pages
            .iter()
            .map(|(&page, captured)| jobs::InputVersion {
                page,
                version: captured.version,
            })
            .collect()
    }
    pub fn validate_inputs(world: &World, inputs: &[jobs::InputVersion]) -> bool {
        inputs.iter().all(|input| {
            world
                .page(input.page)
                .map(|_| world.input_version(input.page, CHANNELS))
                == input.version
        })
    }
    pub fn captured_pages(&self) -> usize {
        self.pages.len()
    }
}
impl SpawnWorld for Snapshot {
    fn cell(&mut self, p: BlockPos) -> Result<Cell, Error> {
        if p.space != self.space {
            return Err(Error::Unknown);
        }
        self.observed.insert(p.page());
        self.pages
            .get(&p.page())
            .and_then(|p| p.page.as_ref())
            .map(|page| page.cell(p.local_index()))
            .ok_or(Error::Unknown)
    }
    fn column(&mut self, x: i32, z: i32) -> Result<Column, Error> {
        let info = self.generator.column_info(x, z);
        Ok(Column {
            height: info.height,
            biome: Biome::from_public_name(info.biome),
        })
    }
    fn light(&mut self, p: BlockPos) -> Result<Light, Error> {
        if p.space != self.space {
            return Err(Error::Unknown);
        }
        self.observed.insert(p.page());
        let entry = self
            .pages
            .get(&p.page())
            .filter(|p| p.light_known)
            .ok_or(Error::Unknown)?;
        let value = entry
            .page
            .as_ref()
            .ok_or(Error::Unknown)?
            .light(p.local_index());
        Ok(Light {
            sky: value.sky,
            block: value.block,
        })
    }
    fn protected(&self, p: BlockPos) -> bool {
        self.generator.protects_spawn(p.x, p.z)
            || self.spawn_protection.is_some_and(|(center, radius)| {
                (f64::from(p.x) - center[0]).abs() <= f64::from(radius)
                    && (f64::from(p.z) - center[1]).abs() <= f64::from(radius)
            })
    }
}
pub struct Task {
    pub request: Request,
    pub snapshot: Snapshot,
    pub players: Vec<Player>,
    pub budget: Budget,
}
pub struct Workers {
    pool: jobs::NativePool<Task, Plan>,
}
impl Workers {
    pub fn new(workers: usize, maximum: usize) -> Result<Self, Error> {
        if workers == 0 || workers > 8 || maximum == 0 || maximum > 16 {
            return Err(Error::Bounds);
        }
        let pool = jobs::NativePool::new(workers, maximum, |mut task: Task, cancel| {
            if cancel.is_cancelled() {
                return Err("spawn cancelled".into());
            }
            let mut result = plan(&mut task.snapshot, task.request, &task.players, task.budget)
                .map_err(|e| format!("spawn {e:?}"))?;
            result.inputs = task.snapshot.inputs();
            if cancel.is_cancelled() {
                return Err("spawn cancelled".into());
            }
            Ok(result)
        })
        .map_err(|_| Error::Bounds)?;
        Ok(Self { pool })
    }
    pub fn try_submit(
        &self,
        task: jobs::Dispatch<Task>,
    ) -> Result<(), Box<jobs::SubmitError<Task>>> {
        // Preserve the complete rejected dispatch for retry; the small error
        // envelope allocates only on backpressure, never on idle polling.
        self.pool.try_submit(task).map_err(Box::new)
    }
    pub fn try_recv(&self) -> Option<jobs::Completion<Plan>> {
        self.pool.try_recv()
    }
    pub fn cancel_all(&self) {
        self.pool.cancel_all();
    }
}
