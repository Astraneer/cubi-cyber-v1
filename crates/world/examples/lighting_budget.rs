//! CPU work-budget experiment. Host durations are observations, not target-hardware
//! acceptance. Preparation, snapshot restoration and output are outside each step.
use core_types::{BlockPos, Cell, CellProperties, PageId, SpaceId, Tick};
use std::{collections::VecDeque, time::Instant};
use world::{
    JournalLimits, Transaction, World,
    lighting::{LightBudget, LightPriority, LightingEngine, LightingLimits},
};
fn properties(cell: Cell) -> CellProperties {
    if cell == Cell::AIR {
        CellProperties::AIR
    } else {
        CellProperties::SOLID
    }
}
fn main() {
    let mut initial = World::new(properties, 4096);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    let mut pages = Vec::new();
    for x in -1..=1 {
        for z in -1..=1 {
            for y in 0..8 {
                let page = PageId {
                    space: SpaceId(0),
                    x,
                    y,
                    z,
                };
                tx.create_page(
                    page,
                    vec![
                        if y < 4 {
                            Cell::from_parts(1, 0)
                        } else {
                            Cell::AIR
                        };
                        4096
                    ],
                );
                pages.push(page);
            }
        }
    }
    initial.commit(tx).unwrap();
    let state = initial.checkpoint().to_restore_state();
    for cells in [1024u64, 4096, 16384] {
        let mut world =
            World::restore(properties, JournalLimits::default(), state.clone()).unwrap();
        let mut engine = LightingEngine::attach(&mut world, LightingLimits::default()).unwrap();
        for daylight in [true, false] {
            let mut pending = VecDeque::from(pages.clone());
            let mut durations = Vec::new();
            let (mut examined, mut updates, mut seeds, mut peak) = (0, 0, 0, 0);
            for _ in 0..20000 {
                // Both modes use the same bounded2-page admission, as the owner.
                for _ in 0..2 {
                    let Some(page) = pending.front().copied() else {
                        break;
                    };
                    let admitted = if daylight {
                        engine.queue_page(page, Tick(0), LightPriority::Background)
                    } else {
                        engine.queue_sky_boundary_page(page, Tick(0), LightPriority::Interactive)
                    };
                    if admitted.is_err() {
                        break;
                    }
                    pending.pop_front();
                }
                let begin = Instant::now();
                let progress = engine
                    .step(
                        &mut world,
                        Tick(0),
                        LightBudget {
                            max_cells: cells,
                            max_neighbor_reads: 6 * cells,
                            max_seed_cells: cells,
                            max_updates: cells,
                        },
                        &|p: BlockPos| (daylight && p.y == 127).then_some(15),
                    )
                    .unwrap();
                durations.push(begin.elapsed().as_nanos() as u64);
                examined += progress.work.cells_examined;
                updates += progress.work.light_updates;
                seeds += progress.work.seed_cells_examined;
                peak = peak.max(progress.pending_cells);
                if pending.is_empty() && engine.is_idle() {
                    break;
                }
            }
            assert!(
                engine.is_idle() && pending.is_empty(),
                "work must settle at fixed quota"
            );
            for page in &pages {
                for i in 0..4096 {
                    let local = core_types::LocalIdx::new(i).unwrap();
                    let expected = if daylight && page.y >= 4 { 15 } else { 0 };
                    assert_eq!(world.page(*page).unwrap().light(local).sky, expected);
                }
            }
            let phases = durations.len();
            durations.sort_unstable();
            let percentile = |p: usize| durations[(durations.len() - 1) * p / 100] as f64 / 1e6;
            println!(
                "LIGHT_BUDGET cells={cells} daylight={daylight} pages=72 phases={phases} examined={examined} updates={updates} seeds={seeds} frontier_peak={peak} step_p50_ms={:.6} step_p95_ms={:.6} step_p99_ms={:.6}",
                percentile(50),
                percentile(95),
                percentile(99)
            );
        }
    }
}
