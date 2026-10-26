use r#gen::{Generator, SEA_LEVEL, TerrainSeed};
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);

#[test]
fn interval_certificates_bound_every_unsampled_integer_surface_across_biomes_and_branches() {
    let mut tested = 0;
    let mut max_ratio = 0f64;
    for seed in [1, 1337, 0x7f23ac15] {
        let g = Generator::new(TerrainSeed(seed));
        let s = g.spawn().center;
        for origin in [
            [s[0] - 64, s[2] - 64],
            [1, 1],
            [880, -350],
            [-2048, 1024],
            [16384, -16384],
            [-65536, -65536],
        ] {
            for level in 0..=3 {
                let bound = g.public_height_error(origin, level).unwrap();
                assert_eq!(bound.intervals, if level <= 2 { 0 } else { 256 });
                assert_eq!(
                    bound.sampled_columns,
                    if level <= 2 {
                        ((16u64 << level) + 1).pow(2)
                    } else {
                        289
                    }
                );
                assert_eq!(
                    bound.unit_cells,
                    if level <= 2 {
                        (16u64 << level).pow(2)
                    } else {
                        0
                    }
                );
                assert!(bound.noise_evaluations <= 256 * 200);
                assert_eq!(bound.noise_evaluations == 0, level <= 2);
                assert!(bound.max_error_blocks.is_finite() && bound.max_error_blocks >= 0.);
                let step = 1i32 << level;
                let mut vertices = [0.; 289];
                for z in 0..17 {
                    for x in 0..17 {
                        vertices[x + 17 * z] = f64::from(
                            g.column_info(origin[0] + x as i32 * step, origin[1] + z as i32 * step)
                                .height
                                .max(SEA_LEVEL)
                                + 1,
                        );
                    }
                }
                let mut actual_max = 0f64;
                for z in 0..=16 * step {
                    for x in 0..=16 * step {
                        let height = g
                            .column_info(origin[0] + x, origin[1] + z)
                            .height
                            .max(SEA_LEVEL)
                            + 1;
                        assert!(
                            (bound.min_height..=bound.max_height).contains(&height),
                            "seed{seed}origin{origin:?}level{level}sample{x},{z}:height{height},bound{bound:?}"
                        );
                        let ix = (x / step).min(15) as usize;
                        let iz = (z / step).min(15) as usize;
                        let fx = f64::from(x - ix as i32 * step) / f64::from(step);
                        let fz = f64::from(z - iz as i32 * step) / f64::from(step);
                        let i = ix + 17 * iz;
                        let plane = if fx + fz <= 1. {
                            vertices[i]
                                + (vertices[i + 1] - vertices[i]) * fx
                                + (vertices[i + 17] - vertices[i]) * fz
                        } else {
                            vertices[i + 18]
                                + (vertices[i + 17] - vertices[i + 18]) * (1. - fx)
                                + (vertices[i + 1] - vertices[i + 18]) * (1. - fz)
                        };
                        let error = (f64::from(height) - plane).abs();
                        assert!(
                            error <= bound.max_error_blocks + 1e-9,
                            "seed{seed}origin{origin:?}level{level}sample{x},{z}:actual{error},bound{bound:?}"
                        );
                        actual_max = actual_max.max(error);
                        tested += 1;
                    }
                }
                if bound.max_error_blocks > 0. {
                    max_ratio = max_ratio.max(actual_max / bound.max_error_blocks);
                }
            }
        }
    }
    println!("PUBLIC_HEIGHT_ERROR samples={tested} actual_to_bound_max={max_ratio}");
}

#[test]
fn far_certificates_are_bounded_allocation_free_and_do_not_scan_the_tile_area() {
    let g = Generator::new(TerrainSeed(1337));
    let near = g.public_height_error([16384, -16384], 1).unwrap();
    let scope = telemetry::AllocationScope::begin().unwrap();
    let far = g.public_height_error([16384, -16384], 10).unwrap();
    assert_eq!(scope.finish().allocating_calls(), 0);
    assert_eq!(far.intervals, 256);
    assert!(near.sampled_columns <= 4225 && far.sampled_columns == 289);
    assert!(near.unit_cells <= 4096 && far.unit_cells == 0);
    assert!(far.noise_evaluations <= 256 * 200);
    assert!(g.public_height_error([i32::MAX, 0], 0).is_err());
    assert!(g.public_height_error([0, i32::MAX], 10).is_err());
    assert!(g.public_height_error([0, 0], 11).is_err());
    for origin in [
        [i32::MIN + 16, i32::MIN + 16],
        [i32::MAX - 32768, i32::MAX - 32768],
    ] {
        let bound = g.public_height_error(origin, 10).unwrap();
        assert!(bound.max_error_blocks.is_finite());
    }
}

#[test]
fn leaf_bound_covers_fractional_voxel_stairs_without_subvoxel_noise_extrema() {
    let g = Generator::new(TerrainSeed(1337));
    for origin in [[-20, 30], [96, 96], [880, -350]] {
        let bound = g.public_height_error(origin, 0).unwrap();
        assert_eq!(bound.noise_evaluations, 0);
        for z in 0..16 {
            for x in 0..16 {
                let height = |dx, dz| {
                    f64::from(
                        g.column_info(origin[0] + x + dx, origin[1] + z + dz)
                            .height
                            .max(SEA_LEVEL)
                            + 1,
                    )
                };
                let corners = [height(0, 0), height(1, 0), height(0, 1), height(1, 1)];
                for fz in [0.125, 0.5, 0.875] {
                    for fx in [0.125, 0.5, 0.875] {
                        let plane = if fx + fz <= 1. {
                            corners[0]
                                + (corners[1] - corners[0]) * fx
                                + (corners[2] - corners[0]) * fz
                        } else {
                            corners[3]
                                + (corners[2] - corners[3]) * (1. - fx)
                                + (corners[1] - corners[3]) * (1. - fz)
                        };
                        assert!((plane - corners[0]).abs() <= bound.max_error_blocks);
                    }
                }
            }
        }
    }
}
