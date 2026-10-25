use rules::machines::{Face, grid::*};
use std::hint::black_box;
use telemetry::{AllocationCounts, AllocationScope, CountingAllocator};
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator::new(std::alloc::System);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Destination {
    rooms: [u64; 2],
    got: [u64; 2],
}
impl GridWorld for Destination {
    fn room(&mut self, endpoint: Endpoint, _: Resource, _: u8) -> u64 {
        self.rooms[endpoint.owner as usize]
    }
    fn give(&mut self, endpoint: Endpoint, _: Resource, _: u8, amount: u64) -> u64 {
        let index = endpoint.owner as usize;
        let taken = amount.min(self.rooms[index]);
        self.rooms[index] -= taken;
        self.got[index] += taken;
        taken
    }
}
fn scene(resource: Resource) -> (Grid, NetworkId) {
    let mut grid = Grid::new(8, 2);
    for x in 0..3 {
        grid.add([x, 0, 0], resource, 63, &mut GridWork::default())
            .unwrap();
    }
    let id = grid.network_at([0, 0, 0]).unwrap();
    grid.set_endpoints(
        id,
        &[
            Endpoint {
                owner: 0,
                position: [0, 1, 0],
                face: Face::Down,
                pull: false,
            },
            Endpoint {
                owner: 1,
                position: [2, 1, 0],
                face: Face::Down,
                pull: false,
            },
        ],
    )
    .unwrap();
    (grid, id)
}
#[test]
fn detached_projection_matches_detailed_grid_at100000_boundaries_per_resource() {
    for resource in [Resource::Energy, Resource::Fluid, Resource::Gas] {
        let (mut detailed, id) = scene(resource);
        let mut projected = Grid::restore(8, 2, &detailed.snapshot()).unwrap();
        let mut scratch = PhaseScratch::new(2);
        let substance = match resource {
            Resource::Energy => 0,
            Resource::Fluid => 3,
            Resource::Gas => 4,
        };
        for tick in 0..100_000 {
            if tick % 3 == 0 {
                detailed.insert([0, 0, 0], substance, 237).unwrap();
                projected.insert([0, 0, 0], substance, 237).unwrap();
            }
            let mut a = Destination {
                rooms: [tick % 107, tick % 89],
                got: [0; 2],
            };
            let mut b = a;
            let mut work_a = GridWork::default();
            let mut work_b = GridWork::default();
            let awake_a = detailed.phase(id, &mut a, &mut work_a).unwrap();
            let before = projected.network(id).unwrap().reservoir();
            let (after, awake_b) = projected
                .project_phase(id, before, &mut b, &mut work_b, &mut scratch)
                .unwrap();
            assert_eq!(projected.network(id).unwrap().reservoir(), before);
            projected.apply_reservoir(id, before, after).unwrap();
            assert_eq!(
                detailed.network(id).unwrap().reservoir(),
                after,
                "{resource:?} tick{tick}"
            );
            assert_eq!((a, awake_a, work_a), (b, awake_b, work_b));
        }
    }
}
#[test]
fn exact_topology_handles_endpoints_and_reservoirs_survive_snapshot_restore() {
    let (mut grid, id) = scene(Resource::Energy);
    grid.insert([1, 0, 0], 0, 17234).unwrap();
    let saved = grid.snapshot();
    let restored = Grid::restore(8, 2, &saved).unwrap();
    assert_eq!(restored.snapshot(), saved);
    assert_eq!(restored.network_ids().collect::<Vec<_>>(), [id]);
    let mut duplicate = saved.clone();
    duplicate.slots[id.slot as usize]
        .network
        .as_mut()
        .unwrap()
        .members
        .push([0, 0, 0]);
    assert!(Grid::restore(8, 2, &duplicate).is_err());
    let mut bad = saved.clone();
    bad.slots[id.slot as usize].generation = 0;
    assert!(Grid::restore(8, 2, &bad).is_err());
    let mut bad = saved.clone();
    bad.slots[id.slot as usize]
        .network
        .as_mut()
        .unwrap()
        .reservoir
        .stored = u64::MAX;
    assert!(Grid::restore(8, 2, &bad).is_err());
    assert!(Grid::restore(2, 2, &saved).is_err());
    let before = grid.network(id).unwrap().reservoir();
    let mut after = before;
    after.stored -= 1;
    grid.apply_reservoir(id, before, after).unwrap();
    assert_eq!(
        grid.apply_reservoir(id, before, after),
        Err(GridError::Stale)
    );
}
#[test]
fn warmed_detached_projection_allocates_nothing() {
    let (grid, id) = scene(Resource::Energy);
    let mut scratch = PhaseScratch::new(2);
    let before = grid.network(id).unwrap().reservoir();
    let mut destination = Destination {
        rooms: [100; 2],
        got: [0; 2],
    };
    grid.project_phase(
        id,
        before,
        &mut destination,
        &mut GridWork::default(),
        &mut scratch,
    )
    .unwrap();
    let positive = AllocationScope::begin().unwrap();
    let allocation = Vec::<u8>::with_capacity(black_box(7));
    black_box(&allocation);
    assert!(positive.finish().allocating_calls() > 0);
    drop(allocation);
    let scope = AllocationScope::begin().unwrap();
    for _ in 0..10_000 {
        black_box(
            grid.project_phase(
                id,
                before,
                &mut destination,
                &mut GridWork::default(),
                &mut scratch,
            )
            .unwrap(),
        );
    }
    assert_eq!(scope.finish(), AllocationCounts::ZERO);
}
