//! Golden world shapes/times from approved donor fluid_tests.rs ASP15.
use core_types::Cell;
use rules::{
    ids::*,
    items::*,
    stack::ItemStack,
    survival::{
        Inventory, SurvivalError,
        fluid::{self, Fluid, FluidQueue, Liquid},
    },
};
use std::{collections::BTreeMap, hint::black_box};
use telemetry::{AllocationCounts, AllocationScope, CountingAllocator};
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator::new(std::alloc::System);
fn stack(id: u16, count: u16) -> ItemStack {
    ItemStack::new(id, count).unwrap()
}

struct Scene {
    cells: BTreeMap<[i32; 3], Cell>,
    queue: FluidQueue,
    time: u64,
    reads: u64,
    events: u64,
}
impl Scene {
    fn new() -> Self {
        Self {
            cells: BTreeMap::new(),
            queue: FluidQueue::new(100_000),
            time: 0,
            reads: 0,
            events: 0,
        }
    }
    fn read(cells: &BTreeMap<[i32; 3], Cell>, p: [i32; 3]) -> Option<Cell> {
        Some(cells.get(&p).copied().unwrap_or_else(|| {
            if p[1] <= 0 {
                Cell::from_parts(STONE, 0)
            } else {
                Cell::AIR
            }
        }))
    }
    fn put(&mut self, p: [i32; 3], value: Cell) {
        self.cells.insert(p, value);
        self.queue
            .wake(self.time, p, |q| Self::read(&self.cells, q))
            .unwrap();
    }
    fn tick(&mut self) {
        self.time += 1;
        let mut budget = 10_000;
        while budget > 0 {
            let Some(event) = self.queue.pop_due(self.time) else {
                break;
            };
            budget -= 1;
            self.events += 1;
            let plan = fluid::plan_tick(|q| Self::read(&self.cells, q), event.position).unwrap();
            assert!(plan.reads <= 845);
            assert!(plan.changes().len() <= 6);
            self.reads += u64::from(plan.reads);
            for change in plan.changes() {
                assert_eq!(
                    Self::read(&self.cells, change.position),
                    Some(change.before)
                );
                self.put(change.position, change.after);
            }
        }
        assert!(budget > 0);
    }
    fn all_fluid(&self) -> BTreeMap<[i32; 3], Fluid> {
        self.cells
            .iter()
            .filter_map(|(&p, &cell)| fluid::fluid(cell).map(|value| (p, value)))
            .collect()
    }
}
#[test]
fn water_flat_spill_113_cells_exact_five_ticks_per_distance_and_drains() {
    let mut scene = Scene::new();
    scene.put([0, 1, 0], Fluid::source(Liquid::Water).cell());
    let mut seen = BTreeMap::from([([0, 1, 0], 0)]);
    for _ in 0..42 {
        scene.tick();
        for position in scene.all_fluid().keys() {
            seen.entry(*position).or_insert(scene.time);
        }
    }
    assert_eq!(seen.len(), 113);
    assert!(scene.queue.is_empty());
    for (position, first) in seen {
        let distance = position[0].abs() + position[2].abs();
        assert_eq!(first, 5 * distance as u64);
        assert_eq!(scene.cells[&position].state(), distance as u16);
        assert_eq!(position[1], 1);
    }
    scene.put([0, 1, 0], Cell::AIR);
    for _ in 0..40 {
        scene.tick();
    }
    assert!(scene.all_fluid().is_empty());
    assert!(scene.queue.is_empty());
}
#[test]
fn lava_flat_spill_radius_three_and_slow_30tick_delay() {
    let mut scene = Scene::new();
    scene.put([0, 1, 0], Fluid::source(Liquid::Lava).cell());
    for _ in 0..29 {
        scene.tick();
    }
    assert_eq!(scene.all_fluid().len(), 1);
    scene.tick();
    assert_eq!(scene.all_fluid().len(), 5);
    for _ in 0..100 {
        scene.tick();
    }
    let cells = scene.all_fluid();
    assert_eq!(cells.len(), 25);
    for (position, _) in cells {
        let distance = position[0].abs() + position[2].abs();
        assert!(distance <= 3);
        assert_eq!(scene.cells[&position].state(), 2 * distance as u16);
    }
}
#[test]
fn nearest_hole_at_five_attracts_flow_but_six_is_outside_search() {
    for (pit, expected) in [(5, 11), (6, 117)] {
        let mut scene = Scene::new();
        for y in -4..=0 {
            scene.cells.insert([pit, y, 0], Cell::AIR);
        }
        scene.put([0, 1, 0], Fluid::source(Liquid::Water).cell());
        for _ in 0..80 {
            scene.tick();
        }
        let cells = scene.all_fluid();
        assert_eq!(cells.len(), expected);
        for y in -4..=0 {
            assert!(cells[&[pit, y, 0]].falling);
        }
        if pit == 5 {
            for x in 1..=5 {
                assert_eq!(cells[&[x, 1, 0]].amount, 8 - x as u8);
            }
        } else {
            assert!(cells.contains_key(&[0, 1, 3]));
            assert!(!cells.contains_key(&[7, 1, 0]));
        }
    }
}
#[test]
fn diagonal_two_by_two_infinite_sources_and_missing_source_recovers() {
    let mut scene = Scene::new();
    for x in -2..=3 {
        for z in -2..=3 {
            if !(0..=1).contains(&x) || !(0..=1).contains(&z) {
                scene.cells.insert([x, 1, z], Cell::from_parts(STONE, 0));
            }
        }
    }
    scene.put([0, 1, 0], Fluid::source(Liquid::Water).cell());
    scene.put([1, 1, 1], Fluid::source(Liquid::Water).cell());
    for _ in 0..12 {
        scene.tick();
    }
    for x in 0..=1 {
        for z in 0..=1 {
            assert_eq!(scene.cells[&[x, 1, z]], Fluid::source(Liquid::Water).cell());
        }
    }
    scene.put([1, 1, 0], Cell::AIR);
    for _ in 0..7 {
        scene.tick();
    }
    assert_eq!(scene.cells[&[1, 1, 0]], Fluid::source(Liquid::Water).cell());
}
#[test]
fn lava_contact_hardens_source_and_flow_and_downward_water_stone() {
    for (level, expected) in [(0, OBSIDIAN), (2, COBBLE)] {
        let query = |p| {
            Some(if p == [0, 1, 0] {
                Cell::from_parts(LAVA, level)
            } else if p == [1, 1, 0] {
                Cell::from_parts(WATER, 0)
            } else {
                Cell::AIR
            })
        };
        let plan = fluid::plan_tick(query, [0, 1, 0]).unwrap();
        assert_eq!(plan.changes().len(), 1);
        assert_eq!(plan.changes()[0].after, Cell::from_parts(expected, 0));
    }
    let query = |p| {
        Some(if p == [0, 1, 0] {
            Cell::from_parts(LAVA, 0)
        } else if p == [0, 0, 0] {
            Cell::from_parts(WATER, 0)
        } else {
            Cell::AIR
        })
    };
    let plan = fluid::plan_tick(query, [0, 1, 0]).unwrap();
    assert!(
        plan.changes()
            .iter()
            .any(|change| change.position == [0, 0, 0] && change.after.id() == STONE)
    );
}
#[test]
fn unknown_dependency_defers_complete_plan_and_keeps_original_queue_age() {
    assert_eq!(
        fluid::plan_tick(
            |p| if p == [0; 3] {
                Some(Fluid::source(Liquid::Water).cell())
            } else {
                None
            },
            [0; 3]
        ),
        Err(SurvivalError::UnknownInput)
    );
    let mut queue = FluidQueue::new(1);
    queue.schedule(0, [0; 3], Liquid::Water).unwrap();
    let event = queue.pop_due(10).unwrap();
    queue.retry(event).unwrap();
    assert_eq!(queue.oldest_overdue(12), 7);
    assert_eq!(queue.pop_due(12), Some(event));
}
#[test]
fn queue_capacity_duplicate_order_persistence_and_wake_are_atomic() {
    let mut queue = FluidQueue::new(2);
    assert_eq!(queue.schedule(0, [2, 0, 0], Liquid::Water), Ok(true));
    assert_eq!(queue.schedule(1, [2, 0, 0], Liquid::Water), Ok(false));
    queue.schedule(0, [1, 0, 0], Liquid::Water).unwrap();
    assert_eq!(
        queue.schedule(0, [3, 0, 0], Liquid::Lava),
        Err(SurvivalError::Capacity)
    );
    let before = queue.snapshot();
    let encoded = queue.encode();
    let restored = FluidQueue::decode(&encoded, 2).unwrap();
    assert_eq!(restored.encode(), encoded);
    assert_eq!(restored.snapshot(), before);
    let mut corrupt = encoded.clone();
    corrupt.push(0);
    assert!(matches!(
        FluidQueue::decode(&corrupt, 2),
        Err(SurvivalError::InvalidEncoding)
    ));
    assert!(matches!(
        FluidQueue::decode(&encoded, 1),
        Err(SurvivalError::Capacity)
    ));
    assert_eq!(
        queue.wake(0, [0; 3], |_| Some(Fluid::source(Liquid::Water).cell())),
        Err(SurvivalError::Capacity)
    );
    assert_eq!(queue.snapshot(), before);
    let mut restored = FluidQueue::restore(2, queue.next_order(), &before).unwrap();
    assert_eq!(restored.pop_due(5).unwrap().position, [2, 0, 0]);
    assert_eq!(restored.pop_due(5).unwrap().position, [1, 0, 0]);
}

#[test]
fn water_mutation_hardens_neighboring_lava_immediately_without_waiting_tick() {
    let query = |p: [i32; 3]| {
        Some(if p == [0; 3] {
            Cell::from_parts(WATER, 0)
        } else if p.iter().map(|value| value.abs()).sum::<i32>() == 1 {
            Cell::from_parts(LAVA, 0)
        } else {
            Cell::AIR
        })
    };
    let plan = fluid::plan_neighbor_hardening(query, [0; 3]).unwrap();
    assert_eq!(plan.changes().len(), 5);
    assert!(plan.reads <= 42);
    assert!(
        plan.changes()
            .iter()
            .all(|change| change.after.id() == OBSIDIAN)
    );
    assert!(
        !plan
            .changes()
            .iter()
            .any(|change| change.position == [0, 1, 0])
    );
    assert_eq!(
        fluid::plan_neighbor_hardening(|p| if p == [1, 0, 0] { None } else { query(p) }, [0; 3]),
        Err(SurvivalError::UnknownInput)
    );
}
#[test]
fn bucket_source_only_exchange_spill_and_full_bucket_pours_source() {
    let mut inv = Inventory::empty();
    inv.set_trusted(0, stack(I_BUCKET, 2)).unwrap();
    assert_eq!(
        fluid::plan_bucket(&inv, [0; 3], Cell::from_parts(WATER, 1)),
        Err(SurvivalError::NoEntitlement)
    );
    let plan = fluid::plan_bucket(&inv, [0; 3], Cell::from_parts(WATER, 0)).unwrap();
    assert_eq!(plan.cell.after, Cell::AIR);
    plan.inventory.apply(&mut inv).unwrap();
    assert_eq!(inv.hand(), stack(I_BUCKET, 1));
    assert_eq!(inv.get(1), Some(stack(I_WATER_BUCKET, 1)));
    let plan = fluid::plan_bucket(&inv, [0; 3], Cell::from_parts(LAVA, 0)).unwrap();
    plan.inventory.apply(&mut inv).unwrap();
    assert_eq!(inv.hand(), stack(I_LAVA_BUCKET, 1));
    let plan = fluid::plan_bucket(&inv, [0; 3], Cell::AIR).unwrap();
    assert_eq!(plan.cell.after, Cell::from_parts(LAVA, 0));
    plan.inventory.apply(&mut inv).unwrap();
    assert_eq!(inv.hand(), stack(I_BUCKET, 1));
    let mut inv = Inventory::empty();
    for slot in 0..36 {
        inv.set_trusted(slot, stack(COBBLE, 64)).unwrap();
    }
    inv.set_trusted(0, stack(I_BUCKET, 2)).unwrap();
    let plan = fluid::plan_bucket(&inv, [0; 3], Cell::from_parts(WATER, 0)).unwrap();
    assert_eq!(plan.spill, stack(I_WATER_BUCKET, 1));
    assert_eq!(plan.inventory.after().hand(), stack(I_BUCKET, 1));
}
#[test]
fn sleeping_ocean_and_100k_frontier_idle_cost_constant_no_allocations() {
    let control = AllocationScope::begin().unwrap();
    let bytes = Vec::<u8>::with_capacity(black_box(11));
    black_box(&bytes);
    assert!(control.finish().allocating_calls() > 0);
    drop(bytes);
    let mut empty = FluidQueue::new(0);
    let mut full = FluidQueue::new(100_000);
    for x in 0..100_000 {
        full.schedule(100, [x, 1, 0], Liquid::Water).unwrap();
    }
    let scope = AllocationScope::begin().unwrap();
    for _ in 0..10_000 {
        black_box(empty.pop_due(0));
        black_box(full.pop_due(0));
        black_box(full.oldest_overdue(0));
    }
    assert_eq!(scope.finish(), AllocationCounts::ZERO);
    assert_eq!(full.len(), 100_000);
}
#[test]
fn complete_fluid_tick_is_bounded_and_allocates_nothing() {
    let query = |p: [i32; 3]| {
        Some(if p == [0, 1, 0] {
            Cell::from_parts(WATER, 0)
        } else if p[1] <= 0 {
            Cell::from_parts(STONE, 0)
        } else {
            Cell::AIR
        })
    };
    let plan = fluid::plan_tick(query, [0, 1, 0]).unwrap();
    assert_eq!(plan.changes().len(), 4);
    assert!(plan.reads < 845);
    let scope = AllocationScope::begin().unwrap();
    for _ in 0..100 {
        black_box(fluid::plan_tick(query, [0, 1, 0]).unwrap());
    }
    assert_eq!(scope.finish(), AllocationCounts::ZERO);
}
