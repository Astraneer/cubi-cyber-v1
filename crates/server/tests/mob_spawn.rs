use core_types::{BlockPos, Cell, PageId, SpaceId, Tick};
use rules::{
    entities::{EntityId, MobKind},
    ids::*,
};
use server::mob_spawn::*;
use telemetry::{AllocationCounts, AllocationScope, CountingAllocator};
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator::new(std::alloc::System);
struct Flat {
    floor: i32,
    unknown: bool,
    light_unknown: bool,
    protected: bool,
    sky: u8,
    block: u8,
}
impl Default for Flat {
    fn default() -> Self {
        Self {
            floor: 63,
            unknown: false,
            light_unknown: false,
            protected: false,
            sky: 0,
            block: 0,
        }
    }
}
impl SpawnWorld for Flat {
    fn cell(&mut self, p: BlockPos) -> Result<Cell, Error> {
        if self.unknown {
            return Err(Error::Unknown);
        }
        Ok(if p.y == self.floor {
            Cell::from_parts(GRASS, 0)
        } else if p.y < self.floor {
            Cell::from_parts(STONE, 0)
        } else {
            Cell::AIR
        })
    }
    fn column(&mut self, _: i32, _: i32) -> Result<Column, Error> {
        Ok(Column {
            height: 63,
            biome: Biome::Plains,
        })
    }
    fn light(&mut self, _: BlockPos) -> Result<Light, Error> {
        if self.light_unknown {
            Err(Error::Unknown)
        } else {
            Ok(Light {
                sky: self.sky,
                block: self.block,
            })
        }
    }
    fn protected(&self, _: BlockPos) -> bool {
        self.protected
    }
}
fn herd(seed: u64) -> Request {
    Request {
        kind: Kind::Herd,
        space: SpaceId(0),
        column: [0, 0],
        tick: Tick(1),
        map_seed: seed,
        rng_before: 12345,
        available: 32,
        day: 18000,
    }
}
fn monster() -> Request {
    Request {
        kind: Kind::Monster,
        column: [4, 0],
        rng_before: 140,
        ..herd(0)
    }
}
fn players() -> [Player; 1] {
    [Player {
        space: SpaceId(0),
        position: [0.5, 64., 0.5],
    }]
}
#[test]
fn herd_golden_preserves_two_rng_streams_kind_position_yaw_and_baby_draws() {
    // Independently translated approved game/mob/spawn.rs + GameRng formulas.
    let result = plan(&mut Flat::default(), herd(3), &[], Budget::default()).unwrap();
    assert_eq!(result.status, Status::Complete);
    assert_eq!(result.rng_after, 0xf1bbcdcbfa5410e1);
    let expected = [
        (MobKind::Chicken, 4.5, 2.5, 0x3fb40b40),
        (MobKind::Chicken, 4.5, 2.5, 0x42d26b43),
        (MobKind::Chicken, 4.5, 2.5, 0x435be0a8),
        (MobKind::Chicken, 4.5, 2.5, 0x4306bf61),
        (MobKind::Cow, 14.5, 7.5, 0x436cd3f7),
        (MobKind::Cow, 14.5, 7.5, 0x431d93c5),
        (MobKind::Cow, 14.5, 7.5, 0x43330896),
        (MobKind::Cow, 14.5, 7.5, 0x422d2ee3),
    ];
    assert_eq!(result.candidates.len(), 8);
    for (candidate, (kind, x, z, yaw)) in result.candidates.iter().zip(expected) {
        assert_eq!(candidate.kind, kind);
        assert_eq!(candidate.position, [x, 64., z]);
        assert_eq!(candidate.yaw.to_bits(), yaw);
        assert!(!candidate.baby);
    }
    let result = plan(&mut Flat::default(), herd(4), &[], Budget::default()).unwrap();
    assert_eq!(result.rng_after, 0x78dde6e5fd2a208d);
    assert_eq!(
        result.candidates.iter().map(|c| c.baby).collect::<Vec<_>>(),
        [false, false, true, false]
    );
    let mob = result.candidates[2]
        .instantiate(EntityId(7), SpaceId(0), 1337, Tick(100))
        .unwrap();
    assert!(mob.baby);
    assert_eq!(mob.last_tick, Tick(100));
    assert_eq!(mob.body.yaw.to_bits(), 0x43a24de4);
}
#[test]
fn sheep_color_uses_global_spawn_rng_while_herd_yaw_uses_column_rng() {
    let result = plan(&mut Flat::default(), herd(84), &[], Budget::default()).unwrap();
    assert_eq!(result.candidates.len(), 12);
    assert_eq!(result.rng_after, 0xbe1e08c4728765c8);
    let sheep: Vec<_> = result
        .candidates
        .iter()
        .filter(|c| c.kind == MobKind::Sheep)
        .collect();
    assert_eq!(
        sheep.iter().map(|c| c.color).collect::<Vec<_>>(),
        [0, 0, 0, 1]
    );
    assert_eq!(sheep[0].yaw.to_bits(), 0x438d6663);
    assert_eq!(sheep[3].yaw.to_bits(), 0x439592b8);
}
#[test]
fn monster_golden_has_exact_three_pack_steps_weights_distance_and_rng_continuation() {
    let result = plan(
        &mut Flat::default(),
        monster(),
        &players(),
        Budget::default(),
    )
    .unwrap();
    assert_eq!(result.status, Status::Complete);
    assert_eq!(result.rng_after, 0xde0497cf65c3ef95);
    assert_eq!(result.candidates.len(), 4);
    for (candidate, (x, z, yaw)) in result.candidates.iter().zip([
        (68.5, 3.5, 0x4257ff98),
        (68.5, 2.5, 0x4109361a),
        (68.5, -2.5, 0x438f1f6f),
        (64.5, -4.5, 0x4339704c),
    ]) {
        assert_eq!(candidate.kind, MobKind::Zombie);
        assert_eq!(candidate.position, [x, 64., z]);
        assert_eq!(candidate.yaw.to_bits(), yaw);
        assert!(!candidate.baby);
    }
    assert_eq!(monster_cap(289), 70);
    assert_eq!(monster_cap(1), 0);
    assert_eq!(monster_cap(577), 139);
    assert_eq!(ambient_darkness(6000), 0);
    assert_eq!(ambient_darkness(18000), 11);
    for (biome, empty) in [
        (Biome::Plains, 6),
        (Biome::Forest, 5),
        (Biome::Mountain, 5),
        (Biome::Birch, 0),
        (Biome::Taiga, 20),
        (Biome::Snowy, 20),
        (Biome::Savanna, 12),
    ] {
        let table = herd_table(biome).unwrap();
        assert_eq!(table.weights, [12, 10, 10, 8]);
        assert_eq!(table.empty, empty);
        assert_eq!(table.chance, 0.1);
    }
    assert!(herd_table(Biome::Other).is_none());
}
#[test]
fn unknown_or_budget_discards_partial_candidates_and_preserves_rng_for_retry() {
    for kind in [Kind::Herd, Kind::Monster] {
        let request = if kind == Kind::Herd {
            herd(3)
        } else {
            monster()
        };
        let result = plan(
            &mut Flat {
                unknown: true,
                ..Flat::default()
            },
            request,
            &players(),
            Budget::default(),
        )
        .unwrap();
        assert_eq!(result.status, Status::DeferredUnknown);
        assert!(result.candidates.is_empty());
        assert_eq!(result.rng_after, request.rng_before);
    }
    let result = plan(
        &mut Flat::default(),
        herd(3),
        &[],
        Budget {
            cells: 0,
            ..Budget::default()
        },
    )
    .unwrap();
    assert_eq!(result.status, Status::DeferredBudget);
    assert_eq!(result.rng_after, 12345);
    assert!(result.candidates.is_empty());
    let result = plan(
        &mut Flat {
            light_unknown: true,
            ..Flat::default()
        },
        monster(),
        &players(),
        Budget::default(),
    )
    .unwrap();
    assert_eq!(result.status, Status::DeferredUnknown);
    assert_eq!(result.rng_after, 140);
    assert!(result.candidates.is_empty());
}
#[test]
fn natural_checks_protection_distance_bright_light_capacity_and_edited_surface() {
    for flat in [
        Flat {
            protected: true,
            ..Flat::default()
        },
        Flat {
            sky: 15,
            ..Flat::default()
        },
        Flat {
            block: 15,
            ..Flat::default()
        },
    ] {
        let result = plan(
            &mut { flat },
            Request {
                day: 6000,
                ..monster()
            },
            &players(),
            Budget::default(),
        )
        .unwrap();
        assert!(result.candidates.is_empty());
    }
    let near = [Player {
        space: SpaceId(0),
        position: [68.5, 64., 3.5],
    }];
    assert!(
        plan(&mut Flat::default(), monster(), &near, Budget::default())
            .unwrap()
            .candidates
            .is_empty()
    );
    let far = [Player {
        space: SpaceId(0),
        position: [-100., 64., 0.],
    }];
    assert!(
        plan(&mut Flat::default(), monster(), &far, Budget::default())
            .unwrap()
            .candidates
            .is_empty()
    );
    let result = plan(
        &mut Flat::default(),
        Request {
            available: 1,
            ..monster()
        },
        &players(),
        Budget::default(),
    )
    .unwrap();
    assert_eq!(result.candidates.len(), 1);
    for (floor, y) in [(66, 67.), (60, 61.)] {
        let result = plan(
            &mut Flat {
                floor,
                ..Flat::default()
            },
            herd(4),
            &[],
            Budget::default(),
        )
        .unwrap();
        assert_eq!(result.candidates.len(), 4);
        assert!(result.candidates.iter().all(|c| c.position[1] == y));
    }
}
#[test]
fn actual_arc_snapshot_retains_old_cells_and_rejects_changed_or_newly_known_dependencies() {
    let mut world = world::World::new(rules::properties, 1000);
    let p = BlockPos {
        space: SpaceId(0),
        x: 0,
        y: 0,
        z: 0,
    };
    let mut tx = world::Transaction::new(Tick(0), SpaceId(0));
    tx.create_page(p.page(), vec![Cell::from_parts(GRASS, 0); 4096]);
    world.commit(tx).unwrap();
    let generator = r#gen::Generator::new(r#gen::TerrainSeed(1337));
    let mut snapshot =
        Snapshot::capture(&world, generator.clone(), SpaceId(0), &[(p.page(), false)]).unwrap();
    assert_eq!(snapshot.captured_pages(), 1);
    assert_eq!(snapshot.captured_inputs().len(), 1);
    assert_eq!(snapshot.cell(p).unwrap().id(), GRASS);
    assert_eq!(snapshot.light(p), Err(Error::Unknown));
    assert!(Snapshot::validate_inputs(&world, &snapshot.inputs()));
    let mut tx = world::Transaction::new(Tick(1), SpaceId(0));
    tx.set_cell(p, Cell::AIR);
    world.commit(tx).unwrap();
    assert_eq!(snapshot.cell(p).unwrap().id(), GRASS);
    assert!(!Snapshot::validate_inputs(&world, &snapshot.inputs()));
    let missing = BlockPos { x: 32, ..p };
    assert_eq!(snapshot.cell(missing), Err(Error::Unknown));
    let mut tx = world::Transaction::new(Tick(2), SpaceId(0));
    tx.create_page(missing.page(), vec![Cell::AIR; 4096]);
    world.commit(tx).unwrap();
    assert!(!Snapshot::validate_inputs(&world, &snapshot.inputs()));
}
#[test]
fn actual_native_spawn_worker_defers_missing_pages_without_generating_or_marking_population() {
    use jobs::{CancellationToken, Dispatch, JobId, JobOutcome, JobSpec, Priority, WorkCost};
    use std::{
        thread,
        time::{Duration, Instant},
    };
    let generator = r#gen::Generator::new(r#gen::TerrainSeed(1337));
    let mut selected = None;
    for cx in -8i32..=8 {
        let info = generator.column_info(cx * 16 + 8, 8);
        if herd_table(Biome::from_public_name(info.biome)).is_some() {
            selected = Some(cx);
            break;
        }
    }
    let cx = selected.unwrap();
    let world = world::World::new(rules::properties, 1000);
    let snapshot = Snapshot::capture(&world, generator, SpaceId(0), &[]).unwrap();
    let request = Request {
        column: [cx, 0],
        map_seed: 3 ^ (cx as i64 as u64).wrapping_mul(0x9e3779b97f4a7c15),
        ..herd(3)
    };
    let workers = Workers::new(1, 2).unwrap();
    let task = Task {
        request,
        snapshot,
        players: vec![],
        budget: Budget::default(),
    };
    let dispatch = Dispatch {
        id: JobId(1),
        spec: JobSpec::event(Priority::Background, WorkCost::ZERO, 1024, Tick(1)),
        payload: task,
        cancellation: CancellationToken::new(),
    };
    assert!(workers.try_submit(dispatch).is_ok());
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(completion) = workers.try_recv() {
            let JobOutcome::Success(result) = completion.outcome else {
                panic!("native spawn failed")
            };
            assert_eq!(result.status, Status::DeferredUnknown);
            assert!(result.candidates.is_empty());
            assert_eq!(result.rng_after, 12345);
            assert!(!result.inputs.is_empty());
            assert!(result.inputs.iter().all(|i| i.version.is_none()));
            assert!(Snapshot::validate_inputs(&world, &result.inputs));
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    assert!(
        world
            .page(PageId {
                space: SpaceId(0),
                x: cx,
                y: 4,
                z: 0
            })
            .is_none()
    );
    workers.cancel_all();
}

#[test]
fn invalid_spawn_bounds_reject_before_queries_and_idle_worker_polling_allocates_nothing() {
    for column in [[i32::MIN / 16, 0], [i32::MAX / 16, 0], [1_875_000, 0]] {
        assert!(matches!(
            plan(
                &mut Flat::default(),
                Request { column, ..herd(3) },
                &[],
                Budget::default()
            ),
            Err(Error::Bounds)
        ));
    }
    assert!(matches!(
        plan(
            &mut Flat::default(),
            herd(3),
            &[],
            Budget {
                min_y: i32::MIN,
                ..Budget::default()
            }
        ),
        Err(Error::Bounds)
    ));
    let workers = Workers::new(1, 2).unwrap();
    let scope = AllocationScope::begin().unwrap();
    for _ in 0..10_000 {
        assert!(workers.try_recv().is_none());
    }
    assert_eq!(scope.finish(), AllocationCounts::ZERO);
}
