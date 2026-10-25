#[path = "support/lod_fixtures.rs"]
mod fixtures;
use fixtures::fixture;
use render::lod::*;
use render::terrain_mesh::{MeshScratch, PageMesh};

fn stone() -> PublicVoxel {
    PublicVoxel::solid(3, u32::MAX)
}

fn one_tile(level: u8) -> ClipmapPlan {
    let span = 1_i64 << level;
    ClipmapPlan {
        tiles: vec![ClipmapTile { level, x: 0, z: 0 }],
        outer_bounds_pages: [0, 0, span, span],
        max_level: level,
        visited_nodes: 1,
    }
}

fn complete_parent() -> LodForest {
    let mut forest = LodForest::new(1).unwrap();
    let page = PublicPage::filled(stone());
    for y in 0..2 {
        for z in 0..2 {
            for x in 0..2 {
                forest
                    .upsert_page(LodKey::page(0, x, y, z), 1, &page)
                    .unwrap();
            }
        }
    }
    forest
}

#[test]
fn one_actual_edit_updates_only_ancestors_and_survives_at_the_horizon() {
    let mut forest = LodForest::new(10).unwrap();
    let a = LodKey::page(0, -100_000, -3, 20_000);
    let unrelated = LodKey::page(0, 100_000, 7, -20_000);
    let empty = PublicPage::filled(PublicVoxel::Air);
    forest.upsert_page(a, 1, &empty).unwrap();
    forest.upsert_page(unrelated, 1, &empty).unwrap();
    let unrelated_revision = forest.get(unrelated).unwrap().revision;
    let old_count = forest.node_count();
    let mut actual = empty.clone();
    actual.set([7, 8, 9], stone());
    let work = forest.upsert_page(a, 2, &actual).unwrap();
    assert_eq!(work.nodes_written, 11);
    assert_eq!(work.source_cells_read, 8192);
    assert_eq!(work.child_cells_read, 10 * 64 * 8);
    assert!(work.feature_samples_read >= 4096);
    assert_eq!(forest.node_count(), old_count);
    assert_eq!(forest.get(unrelated).unwrap().revision, unrelated_revision);
    let mut key = a;
    for _ in 0..=10 {
        let node = forest.get(key).unwrap();
        assert_eq!(
            node.occupied_cells, 1,
            "edited building vanished in ancestor {key:?}"
        );
        assert_eq!(node.revision, forest.revision());
        assert!(
            (0..4).any(|y| (0..4).any(|z| (0..4).any(|x| node.grid.get([x, y, z]).occupied())))
        );
        if let Some(parent) = key.parent() {
            key = parent;
        }
    }
    forest.upsert_page(a, 3, &empty).unwrap();
    let mut key = a;
    for _ in 0..=10 {
        assert_eq!(
            forest.get(key).unwrap().occupied_cells,
            0,
            "removed block left a stale LOD building"
        );
        if let Some(parent) = key.parent() {
            key = parent;
        }
    }
}

#[test]
fn negative_parent_coordinates_and_child_resampling_are_exact() {
    let key = LodKey::page(3, -1, -2, -3);
    let parent = key.parent().unwrap();
    assert_eq!([parent.x, parent.y, parent.z], [-1, -1, -2]);
    assert_eq!(parent.child(3), Some(key));
    let mut forest = LodForest::new(1).unwrap();
    for y in 0..2 {
        for z in 0..2 {
            for x in 0..2 {
                let page = PublicPage::filled(if [x, y, z] == [1, 0, 1] {
                    stone()
                } else {
                    PublicVoxel::Air
                });
                forest
                    .upsert_page(LodKey::page(0, x, y, z), 1, &page)
                    .unwrap();
            }
        }
    }
    let parent = forest
        .get(LodKey {
            space: 0,
            level: 1,
            x: 0,
            y: 0,
            z: 0,
        })
        .unwrap();
    assert!(parent.complete);
    assert_eq!(parent.occupied_cells, 4096);
    for y in 0..4 {
        for z in 0..4 {
            for x in 0..4 {
                assert_eq!(
                    parent.grid.get([x, y, z]).occupied(),
                    x >= 2 && y < 2 && z >= 2
                );
            }
        }
    }
}

#[test]
fn versions_and_node_budget_reject_before_mutation() {
    let key = LodKey::page(0, 0, 0, 0);
    let page = PublicPage::filled(PublicVoxel::Air);
    let mut forest = LodForest::with_limits(3, 4).unwrap();
    forest.upsert_page(key, 5, &page).unwrap();
    let revision = forest.revision();
    assert!(!forest.upsert_page(key, 5, &page).unwrap().changed);
    assert_eq!(forest.revision(), revision);
    assert_eq!(
        forest.upsert_page(key, 4, &page),
        Err(UpdateError::StaleVersion)
    );
    assert_eq!(
        forest.upsert_page(key, 5, &PublicPage::filled(stone())),
        Err(UpdateError::ConflictingVersion)
    );
    assert_eq!(
        forest.upsert_page(LodKey::page(0, 1000, 0, 1000), 1, &page),
        Err(UpdateError::NodeBudgetExceeded)
    );
    assert_eq!(forest.node_count(), 4);
    assert_eq!(forest.revision(), revision);
    forest
        .upsert_page(key, 6, &PublicPage::filled(stone()))
        .unwrap();
    assert_eq!(forest.node_count(), 4);
    assert!(forest.revision() > revision);
}

#[test]
fn public_unknown_is_not_air_or_a_generator_request() {
    let page = PublicPage::from_exposed(|[x, _, _]| {
        if x == 0 {
            stone()
        } else {
            PublicVoxel::Unknown
        }
    });
    let mut forest = LodForest::new(0).unwrap();
    let key = LodKey::page(0, 0, 0, 0);
    forest.upsert_page(key, 1, &page).unwrap();
    assert!(!forest.get(key).unwrap().complete);
    assert_eq!(
        forest.get(key).unwrap().grid.get([3, 0, 0]),
        PublicVoxel::Unknown
    );
    let result = SelectionState::default()
        .select(
            &forest,
            &one_tile(0),
            0,
            CameraOrigin {
                block: [0; 3],
                fractional: [0.0; 3],
            },
            SelectionOptions {
                min_y_page: 0,
                max_y_page: 0,
                ..SelectionOptions::default()
            },
        )
        .unwrap();
    assert!(!result.coverage_complete);
    assert!(result.objects.is_empty());
    assert_eq!(result.requests, [key]);
}

#[test]
fn uniform_summary_compression_and_parent_errors_remain_conservative() {
    let forest = complete_parent();
    let parent = forest
        .get(LodKey {
            space: 0,
            level: 1,
            x: 0,
            y: 0,
            z: 0,
        })
        .unwrap();
    assert!(parent.complete);
    assert_eq!(parent.geometric_error_blocks, 0.0);
    assert!(parent.grid.payload_bytes() < SUMMARY_VOLUME * std::mem::size_of::<PublicVoxel>());
    let mut forest = LodForest::new(1).unwrap();
    // Every coarse cell sees solid, but fine occupancy is a checkerboard.
    let page = PublicPage::from_exposed(|[x, y, z]| {
        if (x + y + z) % 2 == 0 {
            stone()
        } else {
            PublicVoxel::Air
        }
    });
    for y in 0..2 {
        for z in 0..2 {
            for x in 0..2 {
                forest
                    .upsert_page(LodKey::page(0, x, y, z), 1, &page)
                    .unwrap();
            }
        }
    }
    let parent = forest
        .get(LodKey {
            space: 0,
            level: 1,
            x: 0,
            y: 0,
            z: 0,
        })
        .unwrap();
    assert!(matches!(parent.grid, SummaryGrid::Uniform(_)));
    assert!(
        parent.geometric_error_blocks > 0.0,
        "an occupied coarse box is not proof of exact geometry"
    );
}

#[test]
fn integer_camera_anchor_avoids_far_world_jitter() {
    for x in [1_i64 << 45, -(1_i64 << 45)] {
        let key = LodKey::page(0, x, 0, 0);
        let origin = key.origin_blocks().unwrap();
        let camera = CameraOrigin {
            block: [origin[0] + 5, 0, 0],
            fractional: [0.25, 0.0, 0.0],
        };
        assert_eq!(camera.relative(key), Some([-5.25, 0.0, 0.0]));
        assert_eq!(
            camera.relative(LodKey::page(0, x + 1, 0, 0)),
            Some([10.75, 0.0, 0.0])
        );
    }
    assert_eq!(
        CameraOrigin {
            block: [0; 3],
            fractional: [f32::NAN, 0.0, 0.0]
        }
        .relative(LodKey::page(0, 0, 0, 0)),
        None
    );
    assert_eq!(LodKey::page(0, i64::MAX, 0, 0).origin_blocks(), None);
}

#[test]
fn pixel_error_hysteresis_rejects_one_pixel_jitter() {
    let mut choice = RefinementHysteresis::new(2.0, 1.0).unwrap();
    for _ in 0..1000 {
        assert!(!choice.update(1.0));
        assert!(!choice.update(3.0));
    }
    assert!(choice.update(3.1));
    for _ in 0..1000 {
        assert!(choice.update(1.0));
        assert!(choice.update(3.0));
    }
    assert!(!choice.update(0.9));
    assert!(choice.update(f64::INFINITY));
}

#[test]
fn reach_doubling_adds_a_bounded_ring_and_tiles_cover_without_gaps() {
    for camera in [[0, 0], [-1, -1], [31, -97], [100_000_003, -100_000_009]] {
        let mut previous = 0;
        for level in 0..=10 {
            let plan = clipmap_plan(camera, level, 2048).unwrap();
            assert!(
                plan.exact_coverage(),
                "coverage has a hole/overlap at {camera:?} level{level}"
            );
            if level > 0 {
                assert!(
                    plan.tiles.len() <= previous + 48,
                    "reach cost grew by more than one ring"
                );
            }
            assert!(plan.tiles.len() <= 16 + 48 * usize::from(level));
            assert!(plan.visited_nodes as usize <= 16 + 64 * usize::from(level));
            if level > 0 {
                assert!(plan.tiles.iter().any(|tile| tile.level == level));
            }
            for seam in plan.seams() {
                assert_eq!(
                    plan.tiles[seam.tile]
                        .level
                        .abs_diff(plan.tiles[seam.neighbour].level),
                    1,
                    "transition is not 2:1 balanced"
                );
            }
            previous = plan.tiles.len();
        }
    }
    let plan = clipmap_plan([0, 0], 10, 2048).unwrap();
    println!(
        "LOD_RING_WORK reach_chunks=1024 tiles={} visible_quads_flat_upper={} visited={}",
        plan.tiles.len(),
        plan.tiles.len() * 6,
        plan.visited_nodes
    );
    assert!(clipmap_plan([0, 0], 10, 16).is_err());
}

#[test]
fn selection_uses_screen_error_and_defers_instead_of_publishing_bad_coverage() {
    let mut forest = complete_parent();
    let key = LodKey {
        space: 0,
        level: 1,
        x: 0,
        y: 0,
        z: 0,
    };
    let plan = one_tile(1);
    let camera = CameraOrigin {
        block: [40, 8, 8],
        fractional: [0.25, 0.0, 0.0],
    };
    let options = SelectionOptions {
        min_y_page: 0,
        max_y_page: 1,
        max_visits: 64,
        ..SelectionOptions::default()
    };
    let mut state = SelectionState::default();
    let coarse = state.select(&forest, &plan, 0, camera, options).unwrap();
    assert!(coarse.coverage_complete);
    assert_eq!(coarse.objects.len(), 1);
    assert_eq!(coarse.objects[0].geometry, GeometrySource::SummaryGrid);
    assert_eq!(coarse.objects[0].cell_scale, 2);
    let mut edited = PublicPage::filled(stone());
    edited.set([7, 8, 7], PublicVoxel::Air);
    forest
        .upsert_page(LodKey::page(0, 0, 0, 0), 2, &edited)
        .unwrap();
    let fine = state.select(&forest, &plan, 0, camera, options).unwrap();
    assert!(fine.coverage_complete);
    assert_eq!(fine.objects.len(), 8);
    assert!(
        fine.objects
            .iter()
            .all(|object| object.geometry == GeometrySource::FinePage)
    );
    let limited = state
        .select(
            &forest,
            &plan,
            0,
            camera,
            SelectionOptions {
                max_visits: 1,
                ..options
            },
        )
        .unwrap();
    assert!(!limited.coverage_complete);
    assert!(limited.objects.is_empty());
    assert_eq!(limited.deferred, [key]);
    assert!(state.retained_choices() <= options.max_visits);
}

#[test]
fn h4_fifty_buildings_measure_protection_cost_without_faking_the_budget() {
    let mut scratch = MeshScratch::new();
    let mut mesh = PageMesh::default();
    let mut total_plain = 0;
    let mut total_guarded = 0;
    let mut lost_air = 0;
    let mut lost_solid = 0;
    let mut maximum_overhead = 0.0_f64;
    let mut worst = String::new();
    let mut unprotected_losses = 0;
    for index in 0..50 {
        let fixture = fixture(index);
        for edge in [2, 4, 8] {
            let plain = coarsen_page(&fixture.page, edge, false);
            let guarded = coarsen_page(&fixture.page, edge, true);
            assert!(
                guarded.protection.count > 0,
                "fixture lacks protected features: {}",
                fixture.name
            );
            mesh_public_page(&plain.page, PublicVoxel::Air, &mut scratch, &mut mesh);
            let plain_quads = mesh.quad_count();
            mesh_public_page(&guarded.page, PublicVoxel::Air, &mut scratch, &mut mesh);
            let guarded_quads = mesh.quad_count();
            total_plain += plain_quads;
            total_guarded += guarded_quads;
            let overhead = (guarded_quads as f64 / plain_quads.max(1) as f64 - 1.0) * 100.0;
            if overhead > maximum_overhead {
                maximum_overhead = overhead;
                worst = format!("{}-edge{edge}", fixture.name);
            }
            lost_air += fixture
                .air
                .iter()
                .filter(|at| guarded.page.get(**at) != PublicVoxel::Air)
                .count();
            let missing: Vec<_> = fixture
                .air
                .iter()
                .filter(|at| guarded.page.get(**at) != PublicVoxel::Air)
                .collect();
            if !missing.is_empty() {
                println!(
                    "H4_LOST fixture={} edge={edge} cells={missing:?}",
                    fixture.name
                );
            }
            lost_solid += fixture
                .solid
                .iter()
                .filter(|at| !guarded.page.get(**at).occupied())
                .count();
            unprotected_losses += fixture
                .air
                .iter()
                .filter(|at| plain.page.get(**at) != PublicVoxel::Air)
                .count();
        }
    }
    assert!(
        unprotected_losses > 0,
        "fixtures must exercise openings that naive LOD closes"
    );
    assert_eq!(lost_air, 0, "topology guard lost a protected opening");
    assert_eq!(
        lost_solid, 0,
        "topology guard lost a protected thin structure"
    );
    let h4_passes = maximum_overhead <= 25.0;
    println!(
        "H4_MEASUREMENT fixtures=50 transitions=150 plain_quads={total_plain} guarded_quads={total_guarded} lost_air={lost_air} lost_solid={lost_solid} unprotected_air_losses={unprotected_losses} max_overhead_percent={maximum_overhead:.3} worst={worst} h4_passes={h4_passes}"
    );
    // This prototype preserves topology, but does not meet the SPEC's universal
    // <=25% overhead hypothesis. Keep that result explicit rather than weakening it.
    assert!(
        !h4_passes,
        "a changed hypothesis outcome requires recorded review of its measured numbers"
    );
}

fn no_protection() -> ProtectionMask {
    ProtectionMask {
        words: [0; 64],
        count: 0,
        coarse_block_edge: 1,
        sample_reads: 0,
    }
}

// Independent reference: intersect every nonair voxel AABB, retaining the first
// positive-length intersection. It does not use DDA, rank lookup or proxy masks.
fn reference_ray(
    page: &PublicPage,
    origin: [f64; 3],
    direction: [f64; 3],
    max_t: f64,
) -> Option<([u8; 3], PublicVoxel, f64)> {
    let mut best = None;
    for y in 0..16_u8 {
        for z in 0..16_u8 {
            for x in 0..16_u8 {
                let cell = [x, y, z];
                let voxel = page.get(cell);
                if voxel == PublicVoxel::Air {
                    continue;
                }
                let mut enter = 0.0_f64;
                let mut exit = max_t;
                for axis in 0..3 {
                    let lo = f64::from(cell[axis]);
                    let hi = lo + 1.0;
                    if direction[axis] == 0.0 {
                        if !(lo..hi).contains(&origin[axis]) {
                            exit = -1.0;
                        }
                    } else {
                        let a = (lo - origin[axis]) / direction[axis];
                        let b = (hi - origin[axis]) / direction[axis];
                        enter = enter.max(a.min(b));
                        exit = exit.min(a.max(b));
                    }
                }
                if exit > enter && best.is_none_or(|(_, _, t)| enter < t) {
                    best = Some((cell, voxel, enter));
                }
            }
        }
    }
    best
}

#[test]
fn masked_proxy_keeps_public_palette_unknowns_and_matches_independent_rays() {
    let page = PublicPage::from_exposed(|[x, y, z]| {
        let value = u32::from(x) * 97 + u32::from(y) * 71 + u32::from(z) * 53;
        match value % 11 {
            0 => PublicVoxel::Unknown,
            1..=6 => PublicVoxel::Air,
            _ => PublicVoxel::Solid {
                material: 65000 + x as u16,
                tint: 0xff00_0000 | value,
                kind: match value % 3 {
                    0 => SurfaceKind::Opaque,
                    1 => SurfaceKind::Cutout,
                    _ => SurfaceKind::Water,
                },
            },
        }
    });
    let proxy = MaskedRayProxy::from_public_page(&page, no_protection());
    for y in 0..16 {
        for z in 0..16 {
            for x in 0..16 {
                assert_eq!(proxy.voxel([x, y, z]), page.get([x, y, z]));
            }
        }
    }
    let mut random = 0x413f_55c1_u32;
    for ray in 0..400 {
        let mut next = || {
            random ^= random << 13;
            random ^= random >> 17;
            random ^= random << 5;
            f64::from(random) / f64::from(u32::MAX)
        };
        let mut origin = [next() * 16.0, next() * 16.0, next() * 16.0];
        origin[ray % 3] = if ray % 2 == 0 { -2.0 } else { 18.0 };
        let target = [next() * 16.0, next() * 16.0, next() * 16.0];
        let direction = std::array::from_fn(|axis| target[axis] - origin[axis]);
        let reference = reference_ray(&page, origin, direction, 2.0);
        let actual = proxy
            .trace(origin, direction, 2.0, MAX_PROXY_RAY_STEPS)
            .unwrap();
        assert!(actual.steps() <= MAX_PROXY_RAY_STEPS);
        match (reference, actual) {
            (None, ProxyTrace::Miss { .. }) => {}
            (Some((expected, PublicVoxel::Unknown, time)), ProxyTrace::Unknown { cell, t, .. }) => {
                assert_eq!(cell, expected);
                assert!((time - t).abs() < 1e-10);
            }
            (Some((expected, value, time)), ProxyTrace::Hit { cell, voxel, t, .. }) => {
                assert_eq!(cell, expected);
                assert_eq!(voxel, value);
                assert!((time - t).abs() < 1e-10);
            }
            (expected, actual) => panic!("ray {ray} expected {expected:?}, got {actual:?}"),
        }
    }
}

#[test]
fn masked_proxy_ray_bound_budget_and_grid_corner_rules_are_explicit() {
    let page = PublicPage::filled(PublicVoxel::Air);
    let proxy = MaskedRayProxy::from_public_page(&page, no_protection());
    assert_eq!(
        proxy.trace([-0.1, 0.02, 0.03], [1.0, 0.99, 0.98], 100.0, 999),
        Ok(ProxyTrace::Miss { steps: 46 })
    );
    assert_eq!(
        proxy.trace([-1.0, 8.5, 8.5], [1.0, 0.0, 0.0], 100.0, 2),
        Ok(ProxyTrace::BudgetExceeded { steps: 2 })
    );
    assert_eq!(
        proxy.trace([-1.0, 8.5, 8.5], [1.0, 0.0, 0.0], 100.0, 0),
        Ok(ProxyTrace::BudgetExceeded { steps: 0 })
    );
    assert_eq!(
        proxy.trace([0.0; 3], [0.0; 3], 10.0, 46),
        Err(ProxyRayError::ZeroDirection)
    );
    assert_eq!(
        proxy.trace([f64::NAN, 0.0, 0.0], [1.0, 0.0, 0.0], 10.0, 46),
        Err(ProxyRayError::NonFinite)
    );
    assert_eq!(
        proxy.trace([0.0; 3], [1.0, 0.0, 0.0], 0.0, 46),
        Err(ProxyRayError::InvalidDistance)
    );
    let mut page = page;
    page.set([1, 0, 0], stone()); // touched only at an edge by the diagonal
    page.set([1, 1, 1], PublicVoxel::solid(65535, 0xff12_3456));
    let proxy = MaskedRayProxy::from_public_page(&page, no_protection());
    let hit = proxy.trace([-1.0; 3], [1.0; 3], 100.0, 46).unwrap();
    assert!(matches!(
        hit,
        ProxyTrace::Hit {
            cell: [1, 1, 1],
            t: 2.0,
            steps: 2,
            ..
        }
    ));
    assert_eq!(proxy.proxy_quads().len(), 6);
    assert_eq!(
        proxy.memory().heap_payload_bytes,
        2 * 2 + 2 * std::mem::size_of::<PublicVoxel>()
    );
    let solid = MaskedRayProxy::from_public_page(&PublicPage::filled(stone()), no_protection());
    for axis in 0..3 {
        for sign in [-1.0, 1.0] {
            let mut origin = [8.5; 3];
            let mut direction = [0.0; 3];
            origin[axis] = if sign > 0.0 { -1.0 } else { 17.0 };
            direction[axis] = sign;
            let ProxyTrace::Hit {
                normal, t, steps, ..
            } = solid.trace(origin, direction, 100.0, 46).unwrap()
            else {
                panic!("solid proxy missed");
            };
            let mut expected = [0; 3];
            expected[axis] = -sign as i8;
            assert_eq!(normal, expected);
            assert_eq!(t, 1.0);
            assert_eq!(steps, 1);
        }
    }
}

#[test]
fn h4b_fifty_buildings_measure_masks_and_ray_cost_beside_fixed_proxy_quads() {
    let mut histogram = [0_u64; MAX_PROXY_RAY_STEPS as usize + 1];
    let mut rays = 0_u64;
    let mut steps = 0_u64;
    let mut hits = 0_u64;
    let mut resident_bytes_sum = 0;
    let mut resident_bytes_max = 0;
    let mut plain_geometry_bytes = 0;
    let mut scratch = MeshScratch::new();
    let mut mesh = PageMesh::default();
    for index in 0..50 {
        let fixture = fixture(index);
        for edge in [2, 4, 8] {
            let guarded = coarsen_page(&fixture.page, edge, true);
            let proxy = MaskedRayProxy::from_public_page(&guarded.page, guarded.protection);
            let plain = coarsen_page(&fixture.page, edge, false);
            mesh_public_page(&plain.page, PublicVoxel::Air, &mut scratch, &mut mesh);
            plain_geometry_bytes += mesh.upload_bytes();
            assert_eq!(proxy.proxy_quads().len(), 6);
            for at in &fixture.air {
                assert_eq!(proxy.voxel(*at), PublicVoxel::Air);
            }
            for at in &fixture.solid {
                assert!(proxy.voxel(*at).occupied());
            }
            let memory = proxy.memory().resident_bytes();
            resident_bytes_sum += memory;
            resident_bytes_max = resident_bytes_max.max(memory);
            // 16² samples from every side: 1536 rays/transition. Independent
            // integer scans compare first hits across every possible axial row.
            for axis in 0..3 {
                for sign in [-1.0, 1.0] {
                    for v in 0..16_u8 {
                        for u in 0..16_u8 {
                            let mut origin = [0.0; 3];
                            origin[axis] = if sign > 0.0 { -1.0 } else { 17.0 };
                            origin[(axis + 1) % 3] = f64::from(u) + 0.5;
                            origin[(axis + 2) % 3] = f64::from(v) + 0.5;
                            let mut direction = [0.0; 3];
                            direction[axis] = sign;
                            let expected = (0..16_u8).find_map(|distance| {
                                let mut cell = [0; 3];
                                cell[axis] = if sign > 0.0 { distance } else { 15 - distance };
                                cell[(axis + 1) % 3] = u;
                                cell[(axis + 2) % 3] = v;
                                let voxel = guarded.page.get(cell);
                                voxel.occupied().then_some((cell, voxel))
                            });
                            let trace = proxy
                                .trace(origin, direction, 100.0, MAX_PROXY_RAY_STEPS)
                                .unwrap();
                            match (expected, trace) {
                                (None, ProxyTrace::Miss { .. }) => {}
                                (Some((expected, value)), ProxyTrace::Hit { cell, voxel, .. }) => {
                                    assert_eq!(cell, expected);
                                    assert_eq!(voxel, value);
                                    hits += 1;
                                }
                                (expected, actual) => panic!(
                                    "{} edge{edge} expected {expected:?} got {actual:?}",
                                    fixture.name
                                ),
                            }
                            rays += 1;
                            steps += u64::from(trace.steps());
                            histogram[trace.steps() as usize] += 1;
                        }
                    }
                }
            }
        }
    }
    assert_eq!(rays, 50 * 3 * 6 * 16 * 16);
    let mut cumulative = 0;
    let p95_steps = histogram
        .iter()
        .position(|count| {
            cumulative += count;
            cumulative * 100 >= rays * 95
        })
        .unwrap();
    let max_steps = histogram.iter().rposition(|count| *count != 0).unwrap();
    println!(
        "H4B_MEASUREMENT fixtures=50 transitions=150 proxy_quads=900 proxy_geometry_bytes=14400 masks_resident_bytes_sum={resident_bytes_sum} masks_resident_bytes_max={resident_bytes_max} plain_geometry_bytes={plain_geometry_bytes} rays={rays} dda_steps={steps} p95_steps={p95_steps} max_steps={max_steps} hits={hits} lost_air=0 lost_solid=0 gpu_measured=false h4_acceptance=false"
    );
    // Fixed geometry does not make mask storage and fragment work disappear.
    assert!(resident_bytes_sum + 14400 > plain_geometry_bytes);
    assert!(steps > rays);
}
