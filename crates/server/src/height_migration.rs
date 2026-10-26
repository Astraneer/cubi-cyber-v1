//! Bounded legacy baseline comparison. Capturing an Arc<Page> reads no cells;
//! private generation and all4096 comparisons occur only on the worker thread.
use crate::height_index::CellEdit;
use core_types::{BlockPos, LocalIdx, PageId, SpaceId, Tick};
use r#gen as terrain_gen;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    thread::ThreadId,
};
pub const CAPACITY: usize = 2;
#[derive(Debug)]
pub enum Error {
    Spawn,
    Capacity,
    Closed,
    SequenceExhausted,
    Worker { page: PageId },
}
pub struct LegacyResult {
    pub page: PageId,
    pub differences: Vec<CellEdit>,
    pub topology_positions: Vec<BlockPos>,
    pub compared_cells: u32,
    pub baseline_work: terrain_gen::GenerationWork,
    pub worker_thread: ThreadId,
}
struct Task {
    page: PageId,
    snapshot: Arc<world::Page>,
}
pub struct LegacyWorker {
    pool: jobs::NativePool<Task, LegacyResult>,
    pending: BTreeMap<jobs::JobId, PageId>,
    pages: BTreeSet<PageId>,
    next: u64,
}
impl LegacyWorker {
    pub fn new(generator: Arc<gen_authority::ServerGenerator>) -> Result<Self, Error> {
        let pool = jobs::NativePool::new(
            1,
            CAPACITY,
            move |task: Task, cancel: &jobs::CancellationToken| {
                compare(task, generator.as_ref(), cancel)
            },
        )
        .map_err(|_| Error::Spawn)?;
        Ok(Self {
            pool,
            pending: BTreeMap::new(),
            pages: BTreeSet::new(),
            next: 1,
        })
    }
    pub fn inflight(&self) -> usize {
        self.pending.len()
    }
    /// Only durable/recovered page snapshots may be supplied by the owner.
    /// Results occupy one of two slots until drained, even after work completes.
    pub fn try_submit(
        &mut self,
        page: PageId,
        snapshot: Arc<world::Page>,
        tick: Tick,
    ) -> Result<bool, Error> {
        if self.pages.contains(&page) {
            return Ok(false);
        }
        if self.pending.len() == CAPACITY {
            return Err(Error::Capacity);
        }
        let next = self.next.checked_add(1).ok_or(Error::SequenceExhausted)?;
        let id = jobs::JobId(self.next);
        let spec = jobs::JobSpec::derived(
            jobs::JobKey { page, kind: 0x4849 },
            jobs::Priority::Background,
            jobs::WorkCost {
                cells: 4096,
                bytes: 8 * 1024 * 1024,
                ..jobs::WorkCost::ZERO
            },
            8 * 1024 * 1024,
            tick,
        );
        let dispatch = jobs::Dispatch {
            id,
            spec,
            payload: Task { page, snapshot },
            cancellation: jobs::CancellationToken::new(),
        };
        match self.pool.try_submit(dispatch) {
            Ok(()) => {
                self.next = next;
                self.pending.insert(id, page);
                self.pages.insert(page);
                Ok(true)
            }
            Err(jobs::SubmitError::Full(_)) => Err(Error::Capacity),
            Err(jobs::SubmitError::Closed(_)) => Err(Error::Closed),
        }
    }
    pub fn try_recv(&mut self) -> Result<Option<LegacyResult>, Error> {
        let Some(done) = self.pool.try_recv() else {
            return Ok(None);
        };
        let page = self.pending.remove(&done.id).ok_or(Error::Closed)?;
        self.pages.remove(&page);
        match done.outcome {
            jobs::JobOutcome::Success(result) => Ok(Some(result)),
            _ => Err(Error::Worker { page }),
        }
    }
}
fn compare(
    task: Task,
    generator: &gen_authority::ServerGenerator,
    cancel: &jobs::CancellationToken,
) -> Result<LegacyResult, String> {
    if cancel.is_cancelled() {
        return Err("legacy comparison cancelled".into());
    }
    let generated = if task.page.space == SpaceId(0)
        && (0..terrain_gen::PROFILE_HEIGHT / 16).contains(&task.page.y)
    {
        Some(
            generator
                .generate_page(task.page, None)
                .map_err(|_| "legacy baseline generation failed")?
                .page,
        )
    } else {
        None
    };
    if cancel.is_cancelled() {
        return Err("legacy comparison cancelled".into());
    }
    let mut differences = Vec::new();
    let mut topology_positions = Vec::new();
    let mut compared_cells = 0;
    for index in 0..4096u16 {
        if index % 256 == 0 && cancel.is_cancelled() {
            return Err("legacy comparison cancelled".into());
        }
        let local = LocalIdx::new(index).expect("bounded page index");
        let cell = task.snapshot.cell(local);
        let baseline = generated
            .as_ref()
            .map_or(core_types::Cell::AIR, |page| page.cells[usize::from(index)]);
        compared_cells += 1;
        let topology = rules::blocks::bdef(cell.id()).be.is_some()
            || matches!(
                cell.id(),
                rules::ids::UNIVERSAL_CABLE
                    | rules::ids::LOGISTICAL_TRANSPORTER
                    | rules::ids::MECHANICAL_PIPE
                    | rules::ids::PRESSURIZED_TUBE
            );
        if cell != baseline || topology {
            let (x, y, z) = local.xyz();
            let coord = |p: i32, l: u8| {
                p.checked_mul(16)
                    .and_then(|v| v.checked_add(i32::from(l)))
                    .ok_or("legacy coordinate overflow")
            };
            let position = BlockPos {
                space: task.page.space,
                x: coord(task.page.x, x)?,
                y: coord(task.page.y, y)?,
                z: coord(task.page.z, z)?,
            };
            if cell != baseline {
                differences.push(CellEdit { position, cell });
            }
            if topology {
                topology_positions.push(position);
            }
        }
    }
    Ok(LegacyResult {
        page: task.page,
        differences,
        topology_positions,
        compared_cells,
        baseline_work: generated.map_or(terrain_gen::GenerationWork::default(), |page| page.work),
        worker_thread: std::thread::current().id(),
    })
}
