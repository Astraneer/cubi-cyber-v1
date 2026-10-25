use core_types::Cell;
use rules::{
    entities::{
        EntityId, MobKind,
        mob::{Control, Mob},
    },
    ids::*,
};
use sim::{
    collision::Bufs,
    entities::{
        self,
        path::{NavSearch, NavSpec, PathBudget, SearchStatus},
    },
};
use std::{cell::Cell as Flag, hint::black_box};
use telemetry::{AllocationCounts, AllocationScope, CountingAllocator};
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator::new(std::alloc::System);
fn known(block: u16) -> world::CellRead {
    world::CellRead::Known(Cell::from_parts(block, 0))
}
fn floor(p: [i32; 3]) -> world::CellRead {
    known(if p[1] <= 0 { STONE } else { AIR })
}
fn finish(search: &mut NavSearch, query: &impl sim::QueryWorld) {
    for _ in 0..1000 {
        let work = search.advance(
            query,
            PathBudget {
                expansions: 8,
                cells: 4096,
            },
        );
        assert!(work.expanded <= 8);
        assert!(work.reads <= 4096);
        if !matches!(search.status(), SearchStatus::Pending) {
            return;
        }
    }
    panic!("bounded search did not terminate")
}
#[test]
fn bounded_straight_path_and_wall_detour_have_valid_adjacent_steps() {
    let query = |p: [i32; 3]| {
        known(
            if p[1] <= 0 || (p[0] == 3 && p[2].abs() <= 2 && p[1] <= 2) {
                STONE
            } else {
                AIR
            },
        )
    };
    let mut search = NavSearch::new(
        [0, 1, 0],
        [8, 1, 0],
        NavSpec::for_mob(MobKind::Pig, false),
        12,
    )
    .unwrap();
    finish(&mut search, &query);
    assert_eq!(search.status(), SearchStatus::Found);
    let path = search.result().unwrap();
    assert_eq!(path.points().first(), Some(&[0, 1, 0]));
    assert_eq!(path.points().last(), Some(&[8, 1, 0]));
    assert!(path.points().iter().any(|p| p[2].abs() > 2));
    for pair in path.points().windows(2) {
        assert!((pair[0][0] - pair[1][0]).abs() <= 1);
        assert!((pair[0][2] - pair[1][2]).abs() <= 1);
        assert_eq!(query(pair[1]), known(AIR));
    }
}
#[test]
fn actual_pig_follows_path_jumps_one_block_and_stands_on_top_before40() {
    let query = |p: [i32; 3]| {
        known(if p[1] <= 0 || (p[2] >= 3 && p[1] <= 1) {
            STONE
        } else {
            AIR
        })
    };
    let mut search = NavSearch::new(
        [0, 1, 0],
        [0, 2, 5],
        NavSpec::for_mob(MobKind::Pig, false),
        1,
    )
    .unwrap();
    finish(&mut search, &query);
    assert_eq!(search.status(), SearchStatus::Found);
    let path = search.result().unwrap().points();
    assert!(path.iter().any(|p| p[1] == 2));
    let mut mob = Mob::new(EntityId(1), MobKind::Pig, [0.5, 1.0, 0.5]).unwrap();
    let mut index = 1;
    let mut bufs = Bufs::default();
    let mut on_top = None;
    let mut apex = 0.0f64;
    for tick in 0..80 {
        let target = entities::path::waypoint(&query, path[index]).unwrap();
        if (mob.body.position[0] - target[0]).abs() < 0.35
            && (mob.body.position[2] - target[2]).abs() < 0.35
            && (mob.body.position[1] - target[1]).abs() < 0.6
            && index + 1 < path.len()
        {
            index += 1;
        }
        let target = entities::path::waypoint(&query, path[index]).unwrap();
        let plan = entities::plan_body(
            &mob,
            &query,
            Control {
                destination: Some(target),
                speed_multiplier: 1.0,
                ..Control::default()
            },
            &mut bufs,
        )
        .unwrap();
        mob = plan.after;
        apex = apex.max(mob.body.position[1] - 1.0);
        if on_top.is_none() && mob.body.on_ground && mob.body.position[1] >= 2.0 {
            on_top = Some(tick);
        }
    }
    assert!(on_top.is_some_and(|tick| tick < 40), "on top {on_top:?}");
    assert!((apex - 1.2522).abs() < 0.01, "apex {apex}");
}
#[test]
fn unknown_dependency_pauses_and_resumes_without_poisoning_nodes() {
    let loaded = Flag::new(false);
    let query = |p: [i32; 3]| {
        if !loaded.get() && p[0] >= 2 {
            world::CellRead::Unknown
        } else {
            floor(p)
        }
    };
    let mut search = NavSearch::new(
        [0, 1, 0],
        [5, 1, 0],
        NavSpec::for_mob(MobKind::Zombie, false),
        7,
    )
    .unwrap();
    search.advance(&query, PathBudget::default());
    assert!(matches!(search.status(), SearchStatus::WaitingForKnown(_)));
    loaded.set(true);
    finish(&mut search, &query);
    assert_eq!(search.status(), SearchStatus::Found);
    assert_eq!(search.source_epoch(), 7);
    assert!(!search.cancel_if_epoch(7));
    assert!(search.cancel_if_epoch(8));
    assert_eq!(search.status(), SearchStatus::Cancelled);
    assert!(search.result().is_none());
}
#[test]
fn logical_budget_bounds_expansion_and_nodes_return_explicit_partial_path() {
    let mut spec = NavSpec::for_mob(MobKind::Pig, false);
    spec.maximum_nodes = 4;
    let mut search = NavSearch::new([0, 1, 0], [15, 1, 0], spec, 1).unwrap();
    let work = search.advance(
        &floor,
        PathBudget {
            expansions: 100,
            cells: 511,
        },
    );
    assert_eq!(work.expanded, 0);
    assert_eq!(work.reads, 0);
    finish(&mut search, &floor);
    assert_eq!(search.status(), SearchStatus::Partial);
    assert!(!search.result().unwrap().reached);
    assert!(search.result().unwrap().points().len() <= 4);
}
#[test]
fn cactus_lava_and_leaves_are_not_walkable_and_low_ceiling_blocks_jump() {
    for blocked in [CACTUS, LAVA, LEAVES] {
        let query = |p: [i32; 3]| {
            if p == [1, 1, 0] {
                known(blocked)
            } else {
                floor(p)
            }
        };
        let mut search = NavSearch::new(
            [0, 1, 0],
            [3, 1, 0],
            NavSpec::for_mob(MobKind::Cow, false),
            1,
        )
        .unwrap();
        finish(&mut search, &query);
        assert_eq!(search.status(), SearchStatus::Found);
        assert!(!search.result().unwrap().points().contains(&[1, 1, 0]));
    }
    let query = |p: [i32; 3]| {
        known(if p[1] <= 0 || (p[0] >= 1 && p[1] <= 1) || p[1] >= 3 {
            STONE
        } else {
            AIR
        })
    };
    let mut search = NavSearch::new(
        [0, 1, 0],
        [2, 2, 0],
        NavSpec::for_mob(MobKind::Cow, false),
        1,
    )
    .unwrap();
    finish(&mut search, &query);
    assert_ne!(search.status(), SearchStatus::Found);
}
#[test]
fn warmed_incremental_path_job_has_zero_allocation_and_matches_split_budget() {
    let spec = NavSpec::for_mob(MobKind::Zombie, false);
    let mut a = NavSearch::new([0, 1, 0], [20, 1, 9], spec, 1).unwrap();
    let mut b = NavSearch::new([0, 1, 0], [20, 1, 9], spec, 1).unwrap();
    let positive = AllocationScope::begin().unwrap();
    let allocation = Vec::<u8>::with_capacity(black_box(11));
    black_box(&allocation);
    assert!(positive.finish().allocating_calls() > 0);
    drop(allocation);
    let scope = AllocationScope::begin().unwrap();
    for _ in 0..1000 {
        a.advance(
            &floor,
            PathBudget {
                expansions: 1,
                cells: 512,
            },
        );
        if a.result().is_some() {
            break;
        }
    }
    assert_eq!(scope.finish(), AllocationCounts::ZERO);
    finish(&mut b, &floor);
    assert_eq!(a.status(), SearchStatus::Found);
    assert_eq!(a.result(), b.result());
}
