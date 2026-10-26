use core_types::{BlockPos, Cell, PAGE_VOLUME, PageId, SpaceId, Tick};
use jobs::{WorkBudget, WorkCost};
use rules::{
    entities::{
        EntityError, EntityId, MobKind,
        mob::{Mob, PlayerProbe},
    },
    ids::*,
    survival::health::DamageKind,
};
use server::entities::{self, Budget, Config, MobDelta, MobRuntime, Snapshot, path_jobs::PathJobs};
use std::{
    hint::black_box,
    time::{Duration, Instant},
};
use telemetry::{AllocationCounts, AllocationScope, CountingAllocator};
use world::{Transaction, World};
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator::new(std::alloc::System);
fn pos(x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x,
        y,
        z,
    }
}
fn scene() -> World {
    let mut world = World::new(rules::properties, 4096);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    for x in -1..=1 {
        for y in -1..=0 {
            for z in -1..=1 {
                let page = PageId {
                    space: SpaceId(0),
                    x,
                    y,
                    z,
                };
                tx.create_page(
                    page,
                    (0..PAGE_VOLUME)
                        .map(|index| {
                            Cell::from_parts(
                                if y * 16 + (index / 256) as i32 <= 0 {
                                    STONE
                                } else {
                                    AIR
                                },
                                0,
                            )
                        })
                        .collect::<Vec<_>>()
                        .into_boxed_slice(),
                );
            }
        }
    }
    world.commit(tx).unwrap();
    world
}
fn config(maximum: usize) -> Config {
    Config {
        maximum_entities: maximum,
        seed: 1337,
        path_workers: 1,
        maximum_path_jobs: 4,
    }
}
fn player() -> PlayerProbe {
    PlayerProbe {
        id: EntityId(99),
        space: SpaceId(0),
        position: [0.5, 1.0, 8.5],
        eye: [0.5, 2.62, 8.5],
        hands: [0; 2],
        alive: true,
        creative: false,
        spectator: false,
        protected: false,
    }
}
fn commit(world: &mut World, runtime: &mut MobRuntime, plan: entities::PhasePlan) {
    runtime.validate(world, &plan).unwrap();
    assert!(plan.drops.is_empty());
    for tx in plan.transactions {
        world.commit(tx).unwrap();
    }
    let encoded = plan.delta.encode().unwrap();
    runtime
        .acknowledge(MobDelta::decode(&encoded, 5000).unwrap())
        .unwrap();
}

#[test]
fn exhausted_cell_budget_preserves_unvisited_cursor_and_same_tick_retry_updates_each_body_once() {
    let mut world = scene();
    let mut runtime = MobRuntime::new(config(8)).unwrap();
    for id in 1..=8 {
        runtime
            .insert_initial(
                Mob::new(EntityId(id), MobKind::Pig, [id as f64 * 1.5, 1., 3.5]).unwrap(),
                true,
            )
            .unwrap();
    }
    let tiny = runtime
        .plan_tick(
            &world,
            Tick(1),
            &[],
            Budget {
                actors: 8,
                cells: 1,
                ..Budget::default()
            },
        )
        .unwrap();
    assert!(tiny.work.budget_exhausted);
    assert_eq!(tiny.work.visited, 0);
    assert_eq!(
        tiny.work.waiting_world, 0,
        "quota pressure is distinct from actual Unknown cells"
    );
    assert!(
        tiny.delta.is_empty(),
        "no actor or cursor state is consumed without completing a plan"
    );
    assert!(runtime.entities().rows().iter().all(|mob| mob.age == 0));
    let complete = runtime
        .plan_tick(
            &world,
            Tick(1),
            &[],
            Budget {
                actors: 8,
                ..Budget::default()
            },
        )
        .unwrap();
    assert_eq!(complete.work.visited, 8);
    commit(&mut world, &mut runtime, complete);
    assert!(
        runtime
            .entities()
            .rows()
            .iter()
            .all(|mob| mob.age == 1 && mob.last_tick == Tick(1))
    );
    let repeat = runtime
        .plan_tick(
            &world,
            Tick(1),
            &[],
            Budget {
                actors: 8,
                ..Budget::default()
            },
        )
        .unwrap();
    assert_eq!(repeat.work.visited, 8);
    assert_eq!(repeat.work.actors, 0);
    assert!(repeat.delta.is_empty());
}

#[test]
fn real_owner_zombie_uses_worker_path_moves_and_attacks_static_player() {
    let mut world = scene();
    let mut runtime = MobRuntime::new(config(10)).unwrap();
    runtime
        .insert_initial(
            Mob::new(EntityId(1), MobKind::Zombie, [0.5, 1.0, 0.5]).unwrap(),
            true,
        )
        .unwrap();
    let mut hits = 0;
    let started = Instant::now();
    for tick in 1..=500 {
        let plan = runtime
            .plan_tick(&world, Tick(tick), &[player()], Budget::default())
            .unwrap();
        assert!(plan.work.queries <= Budget::default().cells);
        hits += plan.attacks.len();
        commit(&mut world, &mut runtime, plan);
        if hits > 0 {
            break;
        }
        std::thread::yield_now();
        assert!(started.elapsed() < Duration::from_secs(5));
    }
    assert!(runtime.entities().get(EntityId(1)).unwrap().body.position[2] > 5.0);
    assert!(hits > 0);
    assert!(runtime.path_counters().completed > 0);
    assert!(runtime.path_counters().worker_reads > 0);
    assert_eq!(
        runtime.entities().get(EntityId(1)).unwrap().life.maximum(),
        20.0
    );
}
#[test]
fn actual_snapshot_worker_rejects_stale_collision_input_and_keeps_authority() {
    let mut world = scene();
    let mob = Mob::new(EntityId(1), MobKind::Pig, [0.5, 1.0, 0.5]).unwrap();
    let mut paths = PathJobs::new(1, 4).unwrap();
    let mut snapshot_budget = 200_000;
    assert!(
        paths
            .request(&world, &mob, [0, 1, 8], Tick(1), &mut snapshot_budget)
            .unwrap()
    );
    let mut dispatch = WorkBudget::unrestricted(WorkCost {
        cells: 1_000_000,
        bytes: 32 * 1024 * 1024,
        quads: 0,
        rays: 0,
    });
    paths.pump(&world, Tick(1), &mut dispatch);
    let mut tx = Transaction::new(Tick(2), SpaceId(0));
    tx.set_cell(pos(0, 1, 3), Cell::from_parts(STONE, 0));
    world.commit(tx).unwrap();
    let started = Instant::now();
    while paths.counters.stale == 0 {
        paths.pump(&world, Tick(2), &mut dispatch);
        std::thread::yield_now();
        assert!(started.elapsed() < Duration::from_secs(5));
    }
    assert!(paths.take_ready(mob.id).is_none());
    assert_eq!(mob.body.position, [0.5, 1.0, 0.5]);
}
#[test]
fn unknown_actor_stays_unchanged_and_live_delta_replay_is_rejected() {
    let mut runtime = MobRuntime::new(config(1)).unwrap();
    let mob = Mob::new(EntityId(1), MobKind::Cow, [0.5, 1.0, 0.5]).unwrap();
    runtime.insert_initial(mob, true).unwrap();
    let world = World::new(rules::properties, 10);
    let plan = runtime
        .plan_tick(&world, Tick(1), &[], Budget::default())
        .unwrap();
    assert_eq!(plan.work.waiting_world, 1);
    assert!(plan.delta.patches.is_empty());
    runtime.validate(&world, &plan).unwrap();
    let duplicate = MobDelta::decode(&plan.delta.encode().unwrap(), 1).unwrap();
    runtime.acknowledge(plan.delta).unwrap();
    assert_eq!(runtime.entities().get(mob.id), Some(&mob));
    assert_eq!(runtime.acknowledge(duplicate), Err(EntityError::StalePlan));
}
#[test]
fn mob_checkpoint_restores_exact_rows_seed_active_cursor_and_strict_sizes() {
    let world = scene();
    let mut runtime = MobRuntime::new(config(5)).unwrap();
    for id in 0..5 {
        runtime
            .insert_initial(
                Mob::new(
                    EntityId(id),
                    MobKind::ALL[id as usize],
                    [id as f64, 1.0, 0.5],
                )
                .unwrap(),
                id % 2 == 0,
            )
            .unwrap();
    }
    let plan = runtime
        .plan_tick(
            &world,
            Tick(1),
            &[],
            Budget {
                actors: 1,
                ..Budget::default()
            },
        )
        .unwrap();
    runtime.acknowledge(plan.delta).unwrap();
    let bytes = runtime.snapshot().encode().unwrap();
    let snapshot = Snapshot::decode(&bytes, 5).unwrap();
    let restored = MobRuntime::restore(config(5), &snapshot).unwrap();
    assert_eq!(restored.snapshot(), runtime.snapshot());
    let mut corrupt = bytes.clone();
    corrupt.push(0);
    assert!(matches!(
        Snapshot::decode(&corrupt, 5),
        Err(EntityError::InvalidEncoding)
    ));
    assert!(matches!(
        Snapshot::decode(&bytes, 4),
        Err(EntityError::Capacity)
    ));
}
#[test]
fn corpse_drop_is_once_then_removed_at20_ticks() {
    let world = scene();
    let mut runtime = MobRuntime::new(config(1)).unwrap();
    let mut mob = Mob::new(EntityId(1), MobKind::Pig, [0.5, 1.0, 0.5]).unwrap();
    mob.life.damage(DamageKind::Generic, 100.0, (0, 0.0), false);
    runtime.insert_initial(mob, true).unwrap();
    let plan = runtime
        .plan_tick(&world, Tick(1), &[], Budget::default())
        .unwrap();
    assert_eq!(plan.drops.len(), 1);
    assert!(
        plan.drops[0]
            .stacks
            .iter()
            .any(|stack| stack.item() == rules::items::I_PORKCHOP)
    );
    runtime.acknowledge(plan.delta).unwrap();
    for tick in 2..=20 {
        let plan = runtime
            .plan_tick(&world, Tick(tick), &[], Budget::default())
            .unwrap();
        assert!(plan.drops.is_empty());
        runtime.acknowledge(plan.delta).unwrap();
    }
    assert!(runtime.entities().is_empty());
}
#[test]
fn thousand_sleeping_entities_add_no_idle_work_or_main_thread_allocations() {
    let world = World::new(rules::properties, 4);
    let mut empty = MobRuntime::new(config(0)).unwrap();
    let mut sleeping = MobRuntime::new(config(1000)).unwrap();
    for id in 0..1000 {
        sleeping
            .insert_initial(
                Mob::new(EntityId(id), MobKind::Pig, [id as f64, 1.0, 0.0]).unwrap(),
                false,
            )
            .unwrap();
    }
    empty
        .plan_tick(&world, Tick(1), &[], Budget::default())
        .unwrap();
    sleeping
        .plan_tick(&world, Tick(1), &[], Budget::default())
        .unwrap();
    let positive = AllocationScope::begin().unwrap();
    let allocation = Vec::<u8>::with_capacity(black_box(23));
    black_box(&allocation);
    assert!(positive.finish().allocating_calls() > 0);
    drop(allocation);
    let scope = AllocationScope::begin().unwrap();
    for tick in 1..=100 {
        let a = empty
            .plan_tick(&world, Tick(tick), &[], Budget::default())
            .unwrap();
        let b = sleeping
            .plan_tick(&world, Tick(tick), &[], Budget::default())
            .unwrap();
        assert_eq!(a.work, b.work);
        assert_eq!(a.work.actors, 0);
        black_box(a);
        black_box(b);
    }
    assert_eq!(scope.finish(), AllocationCounts::ZERO);
}

#[test]
fn chicken_egg_event_is_once_adult_only_seeded_and_replayable() {
    let world = scene();
    let mut runtime = MobRuntime::new(config(2)).unwrap();
    let mut adult = Mob::new(EntityId(1), MobKind::Chicken, [0.5, 1.0, 0.5]).unwrap();
    adult.egg_remaining = 1;
    let mut baby = Mob::spawned(
        EntityId(2),
        MobKind::Chicken,
        SpaceId(0),
        [4.5, 1.0, 0.5],
        33,
    )
    .unwrap();
    assert!((6000..12000).contains(&baby.egg_remaining));
    baby.baby = true;
    baby.egg_remaining = 1;
    runtime.insert_initial(adult, true).unwrap();
    runtime.insert_initial(baby, true).unwrap();
    let before = runtime.snapshot();
    let plan = runtime
        .plan_tick(&world, Tick(1), &[], Budget::default())
        .unwrap();
    assert_eq!(plan.drops.len(), 1);
    assert_eq!(plan.drops[0].stacks[0].item(), rules::items::I_EGG);
    assert_eq!(plan.drops[0].stacks[0].count(), 1);
    let after = plan.delta.encode().unwrap();
    runtime.acknowledge(plan.delta).unwrap();
    let egg_remaining = runtime.entities().get(adult.id).unwrap().egg_remaining;
    assert!((6000..12000).contains(&egg_remaining));
    assert_eq!(runtime.entities().get(baby.id).unwrap().egg_remaining, 1);
    let mut restored = MobRuntime::restore(config(2), &before).unwrap();
    restored
        .acknowledge(MobDelta::decode(&after, 2).unwrap())
        .unwrap();
    assert_eq!(restored.snapshot(), runtime.snapshot());
    let plan = runtime
        .plan_tick(&world, Tick(2), &[], Budget::default())
        .unwrap();
    assert!(plan.drops.is_empty());
}

#[test]
fn overlapping_mobs_and_players_push_with_bounded_spatial_queries() {
    let world = scene();
    let mut runtime = MobRuntime::new(config(2)).unwrap();
    for (id, x) in [(1, 0.5), (2, 0.75)] {
        runtime
            .insert_initial(
                Mob::new(EntityId(id), MobKind::Pig, [x, 1.0, 0.5]).unwrap(),
                true,
            )
            .unwrap();
    }
    let plan = runtime
        .plan_tick(&world, Tick(1), &[], Budget::default())
        .unwrap();
    assert_eq!(plan.work.push_pairs, 1);
    assert!(plan.work.push_candidates <= 16_384);
    runtime.acknowledge(plan.delta).unwrap();
    assert!((runtime.entities().get(EntityId(1)).unwrap().body.velocity[0] + 0.05).abs() < 1e-12);
    assert!((runtime.entities().get(EntityId(2)).unwrap().body.velocity[0] - 0.05).abs() < 1e-12);
    let mut single = MobRuntime::new(config(1)).unwrap();
    single
        .insert_initial(
            Mob::new(EntityId(1), MobKind::Pig, [0.75, 1.0, 0.5]).unwrap(),
            true,
        )
        .unwrap();
    let mut p = player();
    p.position = [0.5, 1.0, 0.5];
    p.eye = [0.5, 2.62, 0.5];
    let plan = single
        .plan_tick(&world, Tick(1), &[p], Budget::default())
        .unwrap();
    single.acknowledge(plan.delta).unwrap();
    assert!((single.entities().get(EntityId(1)).unwrap().body.velocity[0] - 0.05).abs() < 1e-12);
    let plan = runtime
        .plan_tick(
            &world,
            Tick(2),
            &[],
            Budget {
                push_candidates: 1,
                ..Budget::default()
            },
        )
        .unwrap();
    assert!(plan.work.push_candidates <= 1);
    assert!(plan.work.push_deferred > 0);
    assert!(plan.work.push_probes <= Budget::default().push_probes);
    assert!(plan.work.push_buckets <= Budget::default().push_buckets);
}

#[test]
fn pushing_known_neighbor_never_changes_an_actor_whose_body_crosses_unknown_page() {
    let mut world = World::new(rules::properties, 100);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.create_page(
        PageId {
            space: SpaceId(0),
            x: 0,
            y: 0,
            z: 0,
        },
        (0..PAGE_VOLUME)
            .map(|i| Cell::from_parts(if i / 256 == 0 { STONE } else { AIR }, 0))
            .collect::<Vec<_>>(),
    );
    world.commit(tx).unwrap();
    let mut runtime = MobRuntime::new(config(2)).unwrap();
    for (id, x) in [(1, 15.0), (2, 15.8)] {
        let mut mob = Mob::new(EntityId(id), MobKind::Cow, [x, 1.0, 0.5]).unwrap();
        mob.ai.goal_until = Tick(100);
        runtime.insert_initial(mob, true).unwrap();
    }
    let unknown = *runtime.entities().get(EntityId(2)).unwrap();
    let plan = runtime
        .plan_tick(&world, Tick(1), &[], Budget::default())
        .unwrap();
    assert_eq!(plan.work.actors, 1);
    assert_eq!(plan.work.waiting_world, 1);
    assert_eq!(plan.work.push_pairs, 0);
    runtime.validate(&world, &plan).unwrap();
    runtime.acknowledge(plan.delta).unwrap();
    assert_eq!(*runtime.entities().get(EntityId(2)).unwrap(), unknown);
    assert_eq!(
        runtime.entities().get(EntityId(1)).unwrap().body.velocity[0],
        0.0
    );
}
