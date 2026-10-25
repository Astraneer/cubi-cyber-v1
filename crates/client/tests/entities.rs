use client::entities::{EntityFrame, EntityFrameError, RegistryItemVisuals};
use core_types::{BlockPos, Cell, SpaceId, Tick};
use rules::entities::mob::Instance;
use world::{LightSample, Transaction, World};
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
fn instance() -> Instance {
    Instance {
        position: [0.125, 0., 0.125],
        body_yaw: 0.,
        head_yaw: 45.,
        pitch: 15.,
        limb_phase: 2.,
        limb_amount: 0.5,
        kind: 0,
        flags: 16,
        color: 0,
        id_low: 1,
        id_high: 0,
        death_roll: 0.,
        scale: 1.,
        reserved: 0.,
    }
}
#[test]
fn thousand_instance_bridge_has_no_allocations_or_cpu_vertices_and_unknown_reads_stay_pure() {
    let world = World::new(rules::properties, 8);
    let source = vec![instance(); 1000];
    let mut frame = EntityFrame::new(1000);
    let icons = RegistryItemVisuals {
        icon_layer: &|_| None,
    };
    let control = telemetry::AllocationScope::begin().unwrap();
    let data = vec![1u8; std::hint::black_box(32)];
    std::hint::black_box(&data);
    drop(data);
    assert!(control.finish().allocating_calls() > 0);
    let scope = telemetry::AllocationScope::begin().unwrap();
    for _ in 0..100 {
        let work = frame
            .prepare(&source, &world, SpaceId(0), [0; 3], &icons)
            .unwrap();
        assert_eq!(work.instances, 1000);
        assert_eq!(work.light_reads, 1000);
        assert_eq!(work.unknown_light_reads, 1000);
        assert_eq!(work.cpu_vertices, 0);
    }
    assert_eq!(scope.finish().allocating_calls(), 0);
    assert_eq!(world.loaded_pages(), 0);
    assert_eq!(frame.instances()[0].head_yaw, 45.);
    assert_eq!(frame.instances()[0].flags, 16);
}
#[test]
fn integer_origin_is_applied_before_light_lookup_without_losing_fraction() {
    let mut source = instance();
    source.position = [0.125, 0.5, 0.125];
    let origin = [1_000_000_000, 0, -1_000_000_000];
    let position = BlockPos {
        space: SpaceId(0),
        x: 1_000_000_000,
        y: 1,
        z: -1_000_000_000,
    };
    let mut world = World::new(rules::properties, 8);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.create_page(position.page(), vec![Cell::AIR; 4096].into_boxed_slice());
    tx.set_light(
        position,
        LightSample {
            sky: 7,
            block: 15,
            rgb: [15, 2, 0],
        },
    );
    world.commit(tx).unwrap();
    let mut frame = EntityFrame::new(1);
    frame
        .prepare(
            &[source],
            &world,
            SpaceId(0),
            origin,
            &RegistryItemVisuals {
                icon_layer: &|_| None,
            },
        )
        .unwrap();
    assert_eq!(frame.instances()[0].position, [0.125, 0.5, 0.125]);
    assert_eq!(frame.lights()[0].rgb(), [15, 2, 0]);
    assert_eq!(frame.lights()[0].sky(), 7);
}
#[test]
fn missing_item_icon_keeps_previous_complete_frame_and_block_items_reuse_atlas() {
    let world = World::new(rules::properties, 8);
    let mut frame = EntityFrame::new(1);
    let mob = instance();
    let missing = RegistryItemVisuals {
        icon_layer: &|_| None,
    };
    frame
        .prepare(&[mob], &world, SpaceId(0), [0; 3], &missing)
        .unwrap();
    let before = frame.instances()[0];
    let mut item = instance();
    item.kind = 7;
    item.flags = 32;
    item.color = u32::from(rules::items::I_APPLE);
    assert_eq!(
        frame.prepare(&[item], &world, SpaceId(0), [0; 3], &missing),
        Err(EntityFrameError::MissingItemIcon(rules::items::I_APPLE))
    );
    assert_eq!(frame.instances()[0], before);
    frame
        .prepare(
            &[item],
            &world,
            SpaceId(0),
            [0; 3],
            &RegistryItemVisuals {
                icon_layer: &|id| (id == rules::items::I_APPLE).then_some(11),
            },
        )
        .unwrap();
    assert_eq!(frame.instances()[0].kind, 7);
    assert_eq!(frame.instances()[0].color, 11);
    item.color = u32::from(rules::ids::STONE);
    frame
        .prepare(&[item], &world, SpaceId(0), [0; 3], &missing)
        .unwrap();
    assert_eq!(frame.instances()[0].kind, 6);
    assert_eq!(frame.instances()[0].color, 3);
}
#[test]
fn local_human_keeps_first_person_shadow_and_rebases_before_f32() {
    use client::entities::local_player_instance;
    use render::entity_gpu::{CROUCH, PRIMARY_HIDDEN};
    let mut player = sim::Player::new([1000000000.125, 4., -1000000000.375]);
    player.prev = player.pos;
    player.yaw = 45.;
    player.pitch = 20.;
    player.self_body.snap(30.);
    player.crouch = true;
    let first = local_player_instance(
        &player,
        0.5,
        sim::CameraMode::FirstPerson,
        [1000000000, 0, -1000000000],
        0x123456789,
    )
    .unwrap();
    assert_eq!(first.position, [0.125, 4., -0.375]);
    assert_eq!(first.kind, 5);
    assert_eq!(first.flags, CROUCH | PRIMARY_HIDDEN);
    assert_eq!(first.body_yaw, 30.);
    assert_eq!(first.head_yaw, 45.);
    assert_eq!(first.scale, 1.);
    let third = local_player_instance(
        &player,
        0.5,
        sim::CameraMode::RightShoulder,
        [1000000000, 0, -1000000000],
        0x123456789,
    )
    .unwrap();
    assert_eq!(third.flags, CROUCH);
    assert_eq!(third.id_high, 1);
    player.set_mode(sim::GameMode::Spectator);
    assert!(
        local_player_instance(&player, 0.5, sim::CameraMode::RightShoulder, [0; 3], 1).is_none()
    );
}
