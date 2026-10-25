#[path = "../src/block_models.rs"]
mod bridge;
use bridge::*;
use core_types::{BlockPos, Cell, SpaceId};
use render::block_models::{ACTIVE, BlockInstance, kind};
use rules::{arms, blocks::ST_LIT, ids::*};
use std::hint::black_box;
use telemetry::{AllocationCounts, AllocationScope, CountingAllocator};
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator::new(std::alloc::System);
fn sample(id: u16, state: u16) -> BlockSample {
    BlockSample {
        position: BlockPos {
            space: SpaceId(0),
            x: 0,
            y: 1,
            z: 0,
        },
        cell: Cell::from_parts(id, state),
        light: 0xa321,
        tint: 0xff1f7f3f,
        animation: Animation::default(),
    }
}
#[test]
fn defined_model_families_preserve_catalog_ids_and_do_not_duplicate_terrain() {
    for (id, want) in [
        (CHEST, kind::CHEST),
        (LADDER, kind::LADDER),
        (SNOW_LAYER, kind::SNOW),
        (WALL_TORCH, kind::WALL_TORCH),
        (TORCH, kind::CROSS),
        (METALLURGIC_INFUSER, kind::INFUSER),
        (ELECTROLYTIC_SEPARATOR, kind::SEPARATOR),
        (HEAT_GENERATOR, kind::HEAT_GENERATOR),
        (WIND_GENERATOR, kind::WIND_GENERATOR),
        (ENERGY_CUBE, kind::ENERGY_CUBE),
        (UNIVERSAL_CABLE, kind::CABLE),
        (PRESSURIZED_TUBE, kind::TUBE),
        (MECHANICAL_PIPE, kind::PIPE),
        (LOGISTICAL_TRANSPORTER, kind::TRANSPORTER),
    ] {
        let output = instance(sample(id, 0), [0; 3]).unwrap().unwrap();
        assert_eq!(output.kind, want, "id{id}");
        assert_eq!(output.light, 0xa321);
        assert_eq!(output.position, [0.5, 1.0, 0.5], "id{id}");
    }
    for id in [
        AIR,
        STONE,
        GLASS,
        WATER,
        ELECTRIC_PUMP,
        ENRICHMENT_CHAMBER,
        ENERGIZED_SMELTER,
        CRUSHER,
        PURIFICATION_CHAMBER,
        BOUNDING,
    ] {
        assert!(
            instance(sample(id, 0), [0; 3]).unwrap().is_none(),
            "terrain/hidden id{id}"
        );
    }
    assert_eq!(std::mem::size_of::<BlockInstance>(), 64);
}
#[test]
fn four_facings_layers_animation_and_waterlogged_connection_modes_are_exact() {
    for id in [CHEST, LADDER, WALL_TORCH, WIND_GENERATOR] {
        for (facing, yaw) in [
            (3, 0.0),
            (4, std::f32::consts::FRAC_PI_2),
            (2, std::f32::consts::PI),
            (5, 3.0 * std::f32::consts::FRAC_PI_2),
        ] {
            assert_eq!(
                instance(sample(id, facing), [0; 3]).unwrap().unwrap().yaw,
                yaw
            );
        }
    }
    for layers in 1..=8 {
        let snow = instance(sample(SNOW_LAYER, layers - 1), [0; 3])
            .unwrap()
            .unwrap();
        assert_eq!(snow.amount, f32::from(layers) / 8.0);
    }
    let modes = 1 | 2 << 2 | 3 << 4 | 1 << 6 | 2 << 8 | 3 << 10;
    let state = arms::with_arms(1 << 7, modes);
    for id in [
        UNIVERSAL_CABLE,
        PRESSURIZED_TUBE,
        MECHANICAL_PIPE,
        LOGISTICAL_TRANSPORTER,
    ] {
        let output = instance(sample(id, state), [0; 3]).unwrap().unwrap();
        assert_eq!(output.connections, u32::from(modes));
        assert_eq!(output.yaw, 0.0);
    }
    let mut chest = sample(CHEST, 3);
    chest.animation.chest_open = 0.75;
    assert_eq!(instance(chest, [0; 3]).unwrap().unwrap().phase, 0.75);
    let mut wind = sample(WIND_GENERATOR, 3 | ST_LIT);
    wind.animation.rotor_radians = 1.25;
    let wind = instance(wind, [0; 3]).unwrap().unwrap();
    assert_eq!(wind.phase, 1.25);
    assert_eq!(wind.flags, ACTIVE);
    let mut wind = sample(WIND_GENERATOR, 3);
    wind.animation.active = true;
    assert_eq!(instance(wind, [0; 3]).unwrap().unwrap().flags, ACTIVE);
}
#[test]
fn cube_machine_local_face_atlas_keeps_front_lit_and_horizontal_rotation() {
    assert_eq!(
        cube_machine_instance(sample(ELECTRIC_PUMP, 3), [0; 3])
            .unwrap()
            .unwrap()
            .kind,
        kind::PUMP
    );
    assert!(
        cube_machine_instance(sample(STONE, 0), [0; 3])
            .unwrap()
            .is_none()
    );
    for (id, off, on) in [
        (ENRICHMENT_CHAMBER, L_ENRICH_FRONT, L_ENRICH_FRONT_ON),
        (ENERGIZED_SMELTER, L_SMELTER_FRONT, L_SMELTER_FRONT_ON),
        (CRUSHER, L_CRUSHER_FRONT, L_CRUSHER_FRONT_ON),
        (PURIFICATION_CHAMBER, L_PURIFIER_FRONT, L_PURIFIER_FRONT_ON),
    ] {
        for facing in 2..=5 {
            for (bit, want) in [(0, off), (ST_LIT, on)] {
                let output = cube_machine_instance(sample(id, facing | bit), [0; 3])
                    .unwrap()
                    .unwrap();
                assert_eq!(output.reserved[1] >> 16, want); // Local South is front.
                assert_eq!(output.reserved[1] & 65535, L_MEK_SIDE);
                assert_eq!(output.reserved[0] >> 16, L_MEK_TOP);
                assert_eq!(output.reserved[3], 0);
            }
        }
    }
}
#[test]
fn cross_crop_age_biome_tint_offset_and_origin_are_preserved_before_f32() {
    for (id, first, last) in [
        (WHEAT, L_WHEAT, L_WHEAT + 7),
        (CARROTS, L_CARROTS, L_CARROTS + 3),
        (POTATOES, L_POTATOES, L_POTATOES + 3),
    ] {
        assert_eq!(
            instance(sample(id, 0), [0; 3]).unwrap().unwrap().layer,
            first
        );
        assert_eq!(
            instance(sample(id, 7), [0; 3]).unwrap().unwrap().layer,
            last
        );
    }
    let mut grass = sample(TALL_GRASS, 0);
    grass.position.x = 1_000_000_000;
    grass.position.z = -1_000_000_000;
    let output = instance(grass, [1_000_000_000, 0, -1_000_000_000])
        .unwrap()
        .unwrap();
    let offset = rules::blocks::plant_offset(TALL_GRASS, grass.position.x, grass.position.z);
    assert_eq!(
        output.position,
        [
            (0.5 + offset[0]) as f32,
            (1.0 + offset[1]) as f32,
            (0.5 + offset[2]) as f32
        ]
    );
    assert_eq!(output.tint, grass.tint);
    assert_eq!(
        instance(sample(CHEST, 3), [0; 3]).unwrap().unwrap().tint,
        u32::MAX
    );
}
#[test]
fn failures_keep_previous_complete_frame_and_warmed_thousand_records_allocate_nothing() {
    let mut frame = BlockModelFrame::new(1000);
    let samples = vec![sample(CHEST, 3); 1000];
    let work = frame.prepare(&samples, [0; 3]).unwrap();
    assert_eq!(work.instances, 1000);
    assert_eq!(work.cpu_vertices, 0);
    let before = frame.instances().to_vec();
    let mut invalid = sample(CHEST, 3);
    invalid.animation.chest_open = f32::NAN;
    assert_eq!(
        frame.prepare(&[invalid], [0; 3]),
        Err(ModelError::InvalidAnimation)
    );
    assert_eq!(frame.instances(), before);
    assert_eq!(
        instance(sample(u16::MAX, 0), [0; 3]),
        Err(ModelError::UnknownBlock(u16::MAX))
    );
    assert_eq!(
        frame.prepare(&samples, [i64::MAX, 0, 0]),
        Err(ModelError::InvalidPosition)
    );
    let positive = AllocationScope::begin().unwrap();
    let allocation = Vec::<u8>::with_capacity(black_box(17));
    black_box(&allocation);
    assert!(positive.finish().allocating_calls() > 0);
    drop(allocation);
    let scope = AllocationScope::begin().unwrap();
    for _ in 0..100 {
        black_box(frame.prepare(black_box(&samples), [0; 3]).unwrap());
    }
    assert_eq!(scope.finish(), AllocationCounts::ZERO);
    assert_eq!(std::mem::size_of_val(frame.instances()), 64_000);
    let work = frame.prepare(&[sample(STONE, 0)], [0; 3]).unwrap();
    assert_eq!(work.skipped, 1);
    assert!(frame.instances().is_empty());
}
