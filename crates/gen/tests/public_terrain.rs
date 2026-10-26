use r#gen::{Generator, PROFILE_HEIGHT, SCRATCH_EDGE, TerrainSeed};
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
#[test]
fn authoritative_surface_matches_full_canonical_undecorated_base() {
    let generator = Generator::new(TerrainSeed(1337));
    let mut compared = 0;
    for [cx, cz] in [
        [0, 0],
        [7, -3],
        [-21, 12],
        [55, 38],
        [1024, 0],
        [-1024, 1024],
    ] {
        let draft = generator.draft_column(cx, cz).unwrap();
        let origin = draft.origin();
        for z in 0..SCRATCH_EDGE {
            for x in 0..SCRATCH_EDGE {
                let column = generator
                    .public_terrain_column(origin[0] + x, origin[1] + z)
                    .unwrap();
                let cell = |y: i32| {
                    draft.blocks().as_slice()
                        [((z * SCRATCH_EDGE + x) * PROFILE_HEIGHT + y) as usize]
                };
                assert_eq!(u16::from(cell(column.ground_y)), column.ground.id());
                assert_eq!(u16::from(cell(column.surface_y)), column.surface.id());
                if column.surface_y + 1 < PROFILE_HEIGHT {
                    assert_eq!(cell(column.surface_y + 1), 0);
                }
                compared += 1;
            }
        }
    }
    assert_eq!(compared, 1944);
    assert!(generator.public_terrain_column(i32::MIN, 0).is_err());
    assert!(generator.public_terrain_column(0, i32::MAX).is_err());
    println!(
        "M5 canonicalbase1944columns ground+surface exact,sharedmaterialpolicy,noFinalDecorationClaim"
    );
}
#[test]
fn public_sampling_is_allocation_free_and_spawn_extent_is_conservative() {
    let generator = Generator::new(TerrainSeed(1337));
    let scope = telemetry::AllocationScope::begin().unwrap();
    for n in 0..1000 {
        let surface = generator.public_terrain_column(16384 + n, -16384).unwrap();
        assert_eq!(surface.sampled_columns, 5);
    }
    assert_eq!(scope.finish().allocating_calls(), 0);
    let b = generator.public_structure_bounds().unwrap();
    let s = generator.spawn();
    assert!(b[0] <= s.center[0] && b[2] > s.center[0]);
}
