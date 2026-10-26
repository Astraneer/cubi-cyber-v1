//! Private, resumable observer-view construction. No output slice or partial
//! box is exposed; the caller owns source/observer freshness and publication.
use core_types::{BlockPos, Cell, PAGE_VOLUME, PageId};
use std::sync::Arc;
pub const MAX_CELLS: usize = 8192;
pub const MAX_RAYS: usize = 64;
/// Reuse one mutable budget for every page/call of one peer and logical tick.
/// Creating another budget is a new owner tick, not an implicit continuation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    pub cells: usize,
    pub rays: usize,
}
impl Default for Budget {
    fn default() -> Self {
        Self {
            cells: MAX_CELLS,
            rays: MAX_RAYS,
        }
    }
}
impl Budget {
    #[cfg(test)]
    pub fn new(cells: usize, rays: usize) -> Self {
        Self {
            cells: cells.min(MAX_CELLS),
            rays: rays.min(MAX_RAYS),
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Work {
    pub cells: usize,
    pub rays: usize,
    pub completed: bool,
    pub ray_blocked: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidPage,
}
/// `actual` is owned output-in-progress: unprocessed cells remain private.
/// The sealed Arc is the exact previously approved public view, not raw data.
/// Without a public baseline, STONE is the only approved value, including for
/// non-ore cells: absence of a baseline cannot prove that underground AIR is public.
pub struct PendingView {
    page: PageId,
    raw_source: u64,
    sealed: Option<Arc<[Cell; PAGE_VOLUME]>>,
    output: Option<Box<[Cell; PAGE_VOLUME]>>,
    next: usize,
    origin: [i32; 3],
}
impl std::fmt::Debug for PendingView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingView")
            .field("page", &self.page)
            .field("raw_source", &self.raw_source)
            .field("next", &self.next)
            .field("has_sealed", &self.sealed.is_some())
            .field("completed", &self.is_complete())
            .finish_non_exhaustive()
    }
}
impl PendingView {
    pub fn new(
        page: PageId,
        raw_source: u64,
        actual: Box<[Cell; PAGE_VOLUME]>,
        sealed: Option<Arc<[Cell; PAGE_VOLUME]>>,
    ) -> Result<Self, Error> {
        let axis = |v: i32| {
            v.checked_mul(16)
                .filter(|base| base.checked_add(15).is_some())
                .ok_or(Error::InvalidPage)
        };
        let origin = [axis(page.x)?, axis(page.y)?, axis(page.z)?];
        Ok(Self {
            page,
            raw_source,
            sealed,
            output: Some(actual),
            next: 0,
            origin,
        })
    }
    pub fn page(&self) -> PageId {
        self.page
    }
    pub fn raw_source(&self) -> u64 {
        self.raw_source
    }
    #[cfg(test)]
    pub fn sealed(&self) -> Option<&Arc<[Cell; PAGE_VOLUME]>> {
        self.sealed.as_ref()
    }
    #[cfg(test)]
    pub fn next_index(&self) -> usize {
        self.next
    }
    pub fn is_complete(&self) -> bool {
        self.next == PAGE_VOLUME
    }
    pub fn take_completed(&mut self) -> Option<Box<[Cell; PAGE_VOLUME]>> {
        if self.is_complete() {
            self.output.take()
        } else {
            None
        }
    }
    /// Callback performs current-observer authoritative LOS, including Unknown
    /// and distance policy. Its owner separately counts actual ray segments; a
    /// callback invocation here is one ray, not an invented one-segment ray.
    pub fn advance(
        &mut self,
        budget: &mut Budget,
        mut visible: impl FnMut(BlockPos) -> bool,
    ) -> Work {
        if self.is_complete() {
            return Work {
                completed: true,
                ..Default::default()
            };
        }
        budget.cells = budget.cells.min(MAX_CELLS);
        budget.rays = budget.rays.min(MAX_RAYS);
        let mut work = Work::default();
        while self.next < PAGE_VOLUME && budget.cells != 0 {
            let index = self.next;
            let actual = self.output.as_ref().expect("pending owns output")[index];
            let sealed = self
                .sealed
                .as_ref()
                .map_or(Cell::from_parts(rules::ids::STONE, 0), |cells| cells[index]);
            // Every inspection consumes a cell credit, even when ray capacity blocks
            // this candidate. Repeated calls cannot hide an unbounded scan in a tick.
            budget.cells -= 1;
            work.cells += 1;
            let candidate = actual != sealed;
            if candidate {
                if budget.rays == 0 {
                    work.ray_blocked = true;
                    break;
                }
                budget.rays -= 1;
                work.rays += 1;
                let target = BlockPos {
                    space: self.page.space,
                    x: self.origin[0] + (index & 15) as i32,
                    y: self.origin[1] + (index / 256) as i32,
                    z: self.origin[2] + ((index / 16) & 15) as i32,
                };
                if !visible(target) {
                    self.output.as_mut().expect("pending owns output")[index] = sealed;
                }
            }
            self.next += 1;
        }
        work.completed = self.is_complete();
        work
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use core_types::SpaceId;
    fn page() -> PageId {
        PageId {
            space: SpaceId(7),
            x: -2,
            y: 3,
            z: -4,
        }
    }
    fn stone() -> Cell {
        Cell::from_parts(rules::ids::STONE, 0)
    }
    #[test]
    fn more_than_sixty_four_hidden_air_candidates_never_publish_partial_and_resume_once() {
        let sealed = Arc::new([stone(); PAGE_VOLUME]);
        let mut pending = PendingView::new(
            page(),
            19,
            Box::new([Cell::AIR; PAGE_VOLUME]),
            Some(Arc::clone(&sealed)),
        )
        .unwrap();
        assert_eq!(pending.page(), page());
        assert_eq!(pending.raw_source(), 19);
        assert!(Arc::ptr_eq(pending.sealed().unwrap(), &sealed));
        let mut budget = Budget::default();
        let mut calls = 0;
        let work = pending.advance(&mut budget, |_| {
            calls += 1;
            false
        });
        assert_eq!(work.rays, 64);
        assert_eq!(calls, 64);
        assert_eq!(pending.next_index(), 64);
        assert!(work.ray_blocked);
        assert!(pending.take_completed().is_none());
        let again = pending.advance(&mut budget, |_| panic!("ray budget reused"));
        assert_eq!(again.rays, 0);
        assert_eq!(pending.next_index(), 64);
        while !pending.is_complete() {
            let work = pending.advance(&mut Budget::default(), |_| {
                calls += 1;
                false
            });
            assert!(work.rays <= 64);
            if !work.completed {
                assert!(pending.take_completed().is_none());
            }
        }
        assert_eq!(calls, PAGE_VOLUME);
        let output = pending.take_completed().unwrap();
        assert!(output.iter().all(|cell| *cell == stone()));
        assert!(pending.take_completed().is_none());
        assert!(
            pending
                .advance(&mut Budget::default(), |_| panic!(
                    "completed task repeated"
                ))
                .completed
        );
    }
    #[test]
    fn visible_air_restores_actual_and_targets_use_x_z_y_order() {
        let sealed = Arc::new([stone(); PAGE_VOLUME]);
        let mut cells = Box::new([stone(); PAGE_VOLUME]);
        for index in [0, 15, 16, 255, 256, 4095] {
            cells[index] = Cell::AIR;
        }
        let mut pending = PendingView::new(page(), 2, cells, Some(sealed)).unwrap();
        let mut seen = Vec::new();
        let work = pending.advance(&mut Budget::default(), |target| {
            seen.push(target);
            true
        });
        assert!(work.completed);
        assert_eq!(work.rays, 6);
        let expected = [
            [-32, 48, -64],
            [-17, 48, -64],
            [-32, 48, -63],
            [-17, 48, -49],
            [-32, 49, -64],
            [-17, 63, -49],
        ];
        for (target, expected) in seen.iter().zip(expected) {
            assert_eq!([target.x, target.y, target.z], expected);
            assert_eq!(target.space, SpaceId(7));
        }
        let output = pending.take_completed().unwrap();
        assert_eq!(output.iter().filter(|cell| **cell == Cell::AIR).count(), 6);
    }
    #[test]
    fn all_seven_private_ores_require_los_and_confirmed_equality_skips_rays() {
        let ids = [
            rules::ids::COAL_ORE,
            rules::ids::IRON_ORE,
            rules::ids::OSMIUM_ORE,
            rules::ids::TIN_ORE,
            rules::ids::COPPER_ORE,
            rules::ids::GOLD_ORE,
            rules::ids::REDSTONE_ORE,
        ];
        let mut cells = Box::new([stone(); PAGE_VOLUME]);
        for (index, id) in ids.into_iter().enumerate() {
            cells[index] = Cell::from_parts(id, 3);
        }
        let mut pending = PendingView::new(page(), 1, cells, None).unwrap();
        let mut count = 0;
        let work = pending.advance(&mut Budget::default(), |target| {
            count += 1;
            target.x == -32
        });
        assert!(work.completed);
        assert_eq!(count, 7);
        let output = pending.take_completed().unwrap();
        assert_eq!(output[0], Cell::from_parts(ids[0], 3));
        assert!(output[1..7].iter().all(|cell| *cell == stone()));
        assert!(output[7..].iter().all(|cell| *cell == stone()));
        let sealed = Arc::from(output);
        let mut equal =
            PendingView::new(page(), 2, Box::new(*sealed), Some(Arc::clone(&sealed))).unwrap();
        let work = equal.advance(&mut Budget::new(PAGE_VOLUME, 0), |_| {
            panic!("public/equal value requested LOS")
        });
        assert!(work.completed);
        assert_eq!(work.rays, 0);
        assert_eq!(equal.take_completed().unwrap().as_ref(), sealed.as_ref());
    }
    #[test]
    fn missing_public_baseline_requires_los_for_air_and_nonore_values() {
        let mut cells = Box::new([stone(); PAGE_VOLUME]);
        cells[0] = Cell::AIR;
        cells[1] = Cell::from_parts(rules::ids::DIRT, 0);
        let mut pending = PendingView::new(page(), 1, cells, None).unwrap();
        let mut rays = 0;
        let work = pending.advance(&mut Budget::default(), |_| {
            rays += 1;
            false
        });
        assert!(work.completed);
        assert_eq!(rays, 2);
        assert!(
            pending
                .take_completed()
                .unwrap()
                .iter()
                .all(|c| *c == stone())
        );

        // A concrete approved AIR baseline, rather than its absence, permits
        // known public AIR to skip rays without revealing newly hidden changes.
        let baseline = Arc::new([Cell::AIR; PAGE_VOLUME]);
        let mut public =
            PendingView::new(page(), 2, Box::new(*baseline), Some(Arc::clone(&baseline))).unwrap();
        assert!(
            public
                .advance(&mut Budget::new(PAGE_VOLUME, 0), |_| {
                    panic!("approved public AIR requested LOS")
                })
                .completed
        );
        assert_eq!(public.take_completed().unwrap().as_ref(), baseline.as_ref());
    }
    #[test]
    fn shared_budget_caps_multiple_pages_and_tick_reset_is_explicit() {
        let sealed = Arc::new([stone(); PAGE_VOLUME]);
        let mut first = PendingView::new(
            page(),
            1,
            Box::new([Cell::AIR; PAGE_VOLUME]),
            Some(Arc::clone(&sealed)),
        )
        .unwrap();
        let mut second =
            PendingView::new(page(), 2, Box::new([Cell::AIR; PAGE_VOLUME]), Some(sealed)).unwrap();
        let mut budget = Budget {
            cells: usize::MAX,
            rays: usize::MAX,
        };
        let mut calls = 0;
        let a = first.advance(&mut budget, |_| {
            calls += 1;
            true
        });
        let b = second.advance(&mut budget, |_| {
            calls += 1;
            true
        });
        assert_eq!(calls, 64);
        assert_eq!(a.rays + b.rays, 64);
        assert_eq!(second.next_index(), 0);
        assert_eq!(
            first.advance(&mut Budget::new(0, 64), |_| panic!()).cells,
            0
        );
        assert_eq!(first.next_index(), 64);
    }
    #[test]
    fn invalid_page_and_safe_debug_never_expose_actual_cells() {
        assert!(matches!(
            PendingView::new(
                PageId {
                    x: i32::MAX,
                    ..page()
                },
                1,
                Box::new([Cell::AIR; PAGE_VOLUME]),
                None
            ),
            Err(Error::InvalidPage)
        ));
        let pending = PendingView::new(
            page(),
            123,
            Box::new([Cell::from_raw(0xdeadbeef); PAGE_VOLUME]),
            None,
        )
        .unwrap();
        let debug = format!("{pending:?}");
        assert!(!debug.contains("deadbeef") && !debug.contains("3735928559"));
        assert!(debug.contains("raw_source: 123"));
    }
    #[test]
    fn ten_thousand_ray_blocked_advances_allocate_nothing_and_inspection_budget_is_bounded() {
        let mut pending = PendingView::new(
            page(),
            1,
            Box::new([Cell::AIR; PAGE_VOLUME]),
            Some(Arc::new([stone(); PAGE_VOLUME])),
        )
        .unwrap();
        let mut budget = Budget::new(MAX_CELLS, 0);
        let scope = telemetry::AllocationScope::begin().unwrap();
        let mut cells = 0;
        for _ in 0..10000 {
            let work = std::hint::black_box(pending.advance(&mut budget, |_| panic!()));
            cells += work.cells;
            assert!(!work.completed);
            assert!(pending.take_completed().is_none());
        }
        let measured = scope.finish();
        assert_eq!(measured.allocating_calls(), 0);
        assert_eq!(measured.dealloc_calls, 0);
        assert_eq!(cells, MAX_CELLS);
        assert_eq!(pending.next_index(), 0);
        let scope = telemetry::AllocationScope::begin().unwrap();
        let control = std::hint::black_box(vec![std::hint::black_box(1u8); 128]);
        std::hint::black_box(&control);
        drop(control);
        let positive = scope.finish();
        assert!(
            positive.allocating_calls() > 0 && positive.dealloc_calls > 0,
            "allocator instrumentation missing"
        );
    }
}
