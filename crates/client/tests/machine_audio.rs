use client::{
    ClientScene, SceneConfig,
    audio_bridge::{MachineCommand, MachineSounds},
    props_source::{PropsLimits, PropsSource},
};
use core_types::{BlockPos, Cell, LocalIdx, PageId, SpaceId, Tick, Version};
use net::{
    PageSnapshot, PageUpdate,
    props::{Frame, Hint, Kind},
};
use std::sync::Arc;
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
fn page() -> PageId {
    BlockPos {
        space: SpaceId(0),
        x: 0,
        y: 0,
        z: 0,
    }
    .page()
}
fn cells(view: u64) -> PageUpdate {
    PageUpdate::Snapshot(PageSnapshot {
        page: page(),
        version: view,
        source_version: Version(100),
        cells: Box::new([Cell::from_parts(rules::ids::HEAT_GENERATOR, 0); 4096]),
    })
}
fn scene() -> ClientScene {
    let mut scene = ClientScene::new(SceneConfig {
        center: BlockPos {
            space: SpaceId(0),
            x: 0,
            y: 0,
            z: 0,
        },
        horizontal_radius: 0,
        vertical_radius: 0,
        max_loaded_pages: 27,
        authoritative_remote: true,
        ..SceneConfig::forest()
    })
    .unwrap();
    scene
        .import_authoritative_page(&cells(10), Tick(0))
        .unwrap();
    scene
}
fn source() -> PropsSource {
    PropsSource::attach(
        &mut world::World::new(rules::properties, 1024),
        PropsLimits {
            max_pages: 4,
            max_instances: 512,
            ..Default::default()
        },
    )
    .unwrap()
}
fn frame(view: u64, cell_view: u64, count: u16, active: bool) -> Arc<Frame> {
    Arc::new(Frame {
        page: page(),
        cell_view,
        view,
        source: Version(view),
        tick: Tick(view),
        hints: (0..count)
            .map(|i| {
                Hint::from_visual(
                    LocalIdx::new(i).unwrap(),
                    Kind::Machine,
                    None,
                    active,
                    None,
                    None,
                )
                .unwrap()
            })
            .collect(),
    })
}
#[test]
fn only_exact_confirmed_active_bindings_start_loops_and_raw_view_change_stops_before_source_pump() {
    let mut scene = scene();
    let mut source = source();
    let mut sound = MachineSounds::default();
    let mut commands = Vec::new();
    assert!(
        source
            .publish_public_frame(frame(1, 10, 1, true), 10)
            .unwrap()
    );
    let work = sound.visit(1, &scene, &source, [0.; 3], |c| commands.push(c));
    assert_eq!(work.started, 1);
    assert_eq!(sound.active(), 1);
    let MachineCommand::Start(cue) = commands[0] else {
        panic!("expected loop")
    };
    assert_eq!(cue.event, "machine.generator_loop");
    assert_eq!(cue.position, [0.5; 3]);
    let world_version = scene.world().version();
    scene
        .import_authoritative_page(&cells(11), Tick(1))
        .unwrap();
    assert_eq!(
        scene.world().version(),
        world_version,
        "identical raw cells must not fabricate a World edit"
    );
    commands.clear();
    let work = sound.visit(1, &scene, &source, [0.; 3], |c| commands.push(c));
    assert_eq!(work.stopped, 1);
    assert_eq!(sound.active(), 0);
    assert!(matches!(commands.as_slice(), [MachineCommand::Stop(_)]));
    source
        .publish_public_frame(frame(2, 11, 1, true), 11)
        .unwrap();
    commands.clear();
    assert_eq!(
        sound
            .visit(1, &scene, &source, [0.; 3], |c| commands.push(c))
            .started,
        1
    );
    source
        .publish_public_frame(frame(3, 11, 1, false), 11)
        .unwrap();
    assert_eq!(sound.visit(1, &scene, &source, [0.; 3], |_| {}).stopped, 1);
    source
        .publish_public_frame(frame(4, 11, 1, true), 11)
        .unwrap();
    sound.visit(1, &scene, &source, [0.; 3], |_| {});
    source.revoke_public_frame(page(), Tick(5)).unwrap();
    assert_eq!(sound.visit(1, &scene, &source, [0.; 3], |_| {}).stopped, 1);
}
#[test]
fn confirmed_cube_machine_activity_is_audible_independent_of_its_geometry_owner() {
    for item in [
        rules::ids::ENRICHMENT_CHAMBER,
        rules::ids::ENERGIZED_SMELTER,
        rules::ids::CRUSHER,
        rules::ids::PURIFICATION_CHAMBER,
        rules::ids::CHEST,
    ] {
        let mut scene = scene();
        let update = PageUpdate::Snapshot(PageSnapshot {
            page: page(),
            version: 11,
            source_version: Version(200),
            cells: Box::new([Cell::from_parts(item, 0); 4096]),
        });
        scene.import_authoritative_page(&update, Tick(1)).unwrap();
        let mut source = source();
        source
            .publish_public_frame(frame(1, 11, 1, true), 11)
            .unwrap();
        let mut sound = MachineSounds::default();
        let mut event = None;
        let work = sound.visit(1, &scene, &source, [0.; 3], |command| {
            if let MachineCommand::Start(cue) = command {
                event = Some(cue.event);
            }
        });
        if item == rules::ids::CHEST {
            assert_eq!(
                work.started, 0,
                "a machine-shaped hint cannot turn a chest into a machine"
            );
            assert_eq!(event, None);
        } else {
            assert_eq!(work.started, 1);
            assert_eq!(event, Some("machine.loop"));
        }
    }
}

#[test]
fn discovery_voice_count_and_idle_allocations_are_bounded() {
    let scene = scene();
    let mut source = source();
    let mut sounds = MachineSounds::default();
    source
        .publish_public_frame(frame(1, 10, 200, true), 10)
        .unwrap();
    for _ in 0..8 {
        let work = sounds.visit(1, &scene, &source, [0.; 3], |_| {});
        assert!(work.headers <= 2 && work.hints <= 64);
        assert!(sounds.active() <= 32);
    }
    assert_eq!(sounds.active(), 32);
    let scope = telemetry::AllocationScope::begin().unwrap();
    for _ in 0..10_000 {
        let work = sounds.visit(1, &scene, &source, [0.; 3], |_| {
            panic!("idle must not restart voices")
        });
        assert_eq!(work.started + work.stopped + work.headers + work.hints, 0);
        assert_eq!(work.revalidated, 32);
    }
    let counts = scope.finish();
    assert_eq!(counts.allocating_calls(), 0);
    assert_eq!(counts.dealloc_calls, 0);
    let work = sounds.visit(1, &scene, &source, [1000.; 3], |_| {});
    assert_eq!(work.stopped, 32);
    assert_eq!(sounds.active(), 0);
}
