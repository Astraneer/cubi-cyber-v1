use render::terrain_mesh::*;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};

#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);

fn material(raw: u32) -> CellMaterial {
    match raw {
        0 => CellMaterial::EMPTY,
        2 => CellMaterial {
            class: MeshClass::Cutout,
            occludes: false,
            casts_ao: false,
            cull_group: 2,
            face_materials: [20; 6],
            face_uv_rotations: [0; 6],
            tint: 0x88aa_ccff,
            tint_faces: 0x3f,
        },
        3 => CellMaterial {
            class: MeshClass::Water,
            occludes: false,
            casts_ao: false,
            cull_group: 3,
            face_materials: [30; 6],
            face_uv_rotations: [0; 6],
            tint: 0x1122_33ff,
            tint_faces: 0x3f,
        },
        value => CellMaterial::opaque(value as u16),
    }
}

fn run(input: &PageHalo) -> (PageMesh, MeshWork) {
    let mut scratch = MeshScratch::new();
    let mut mesh = PageMesh::default();
    let work = mesh_page(input, material, &mut scratch, &mut mesh);
    (mesh, work)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct UnitFace {
    cell: [i16; 3],
    face: u8,
    bucket: u8,
    material: u16,
    tint: u32,
}

fn bucket(class: MeshClass) -> u8 {
    match class {
        MeshClass::Opaque => 0,
        MeshClass::Cutout => 1,
        MeshClass::Water => 2,
        MeshClass::Empty => panic!("empty has no bucket"),
    }
}

// Independent slow reference: six raw neighbours per cell; no greedy algorithm.
fn expected_faces(input: &PageHalo) -> BTreeMap<UnitFace, usize> {
    let mut faces = BTreeMap::new();
    for y in 0..16_i8 {
        for z in 0..16_i8 {
            for x in 0..16_i8 {
                let here = material(input.get(x, y, z));
                if here.class == MeshClass::Empty {
                    continue;
                }
                for face in Face::ALL {
                    let n = face.normal();
                    let neighbour = material(input.get(x + n[0], y + n[1], z + n[2]));
                    let same_group = here.cull_group != 0
                        && here.class == neighbour.class
                        && here.cull_group == neighbour.cull_group;
                    if neighbour.occludes || same_group {
                        continue;
                    }
                    faces.insert(
                        UnitFace {
                            cell: [i16::from(x), i16::from(y), i16::from(z)],
                            face: face as u8,
                            bucket: bucket(here.class),
                            material: here.face_materials[face as usize],
                            tint: here.tint,
                        },
                        1,
                    );
                }
            }
        }
    }
    faces
}

fn expanded_faces(mesh: &PageMesh) -> BTreeMap<UnitFace, usize> {
    let mut faces = BTreeMap::new();
    for (bucket, quads) in [&mesh.opaque, &mesh.cutout, &mesh.water]
        .into_iter()
        .enumerate()
    {
        for quad in quads {
            let face = quad.face();
            let origin = quad.origin();
            let u = face.tangent_u();
            let v = face.tangent_v();
            // Convert the first outward face corner to its owning voxel.
            let correction = match face {
                Face::Down | Face::West => [0, 0, 0],
                Face::Up => [0, -1, -1],
                Face::North => [-1, 0, 0],
                Face::South => [0, 0, -1],
                Face::East => [-1, 0, -1],
            };
            for dv in 0..quad.size()[1] {
                for du in 0..quad.size()[0] {
                    let cell = std::array::from_fn(|axis| {
                        i16::from(origin[axis])
                            + correction[axis]
                            + i16::from(u[axis]) * i16::from(du)
                            + i16::from(v[axis]) * i16::from(dv)
                    });
                    let entry = faces
                        .entry(UnitFace {
                            cell,
                            face: face as u8,
                            bucket: bucket as u8,
                            material: quad.material(),
                            tint: quad.tint(),
                        })
                        .or_insert(0);
                    *entry += 1;
                    assert_eq!(*entry, 1, "overlapping greedy rectangle");
                }
            }
        }
    }
    faces
}

#[test]
fn empty_page_and_solid_cube_obey_page_work_bound() {
    let (empty, work) = run(&PageHalo::filled(0));
    assert_eq!(empty.quad_count(), 0);
    assert_eq!(work.work.cells_scanned, HALO_VOLUME as u64);
    assert_eq!(work.rim_cells_scanned, HALO_RIM_CELLS as u32);
    assert_eq!(
        work.work.cells_scanned,
        (PAGE_VOLUME + HALO_RIM_CELLS) as u64
    );
    let input = PageHalo::from_page(&[1; PAGE_VOLUME], 0);
    let (mesh, work) = run(&input);
    assert_eq!(mesh.quad_count(), 6);
    assert_eq!(mesh.upload_bytes(), 96);
    assert_eq!(work.generated_bytes, 96);
    assert_eq!(
        work.work.upload_bytes, 0,
        "mesher does not perform GPU uploads"
    );
    assert_eq!(work.work.quads_emitted, 6);
    assert_eq!(work.visibility_words, 6 * 16 * 16);
    for quad in mesh.iter() {
        assert_eq!(quad.size(), [16, 16]);
        assert_eq!(quad.ao(), [3; 4]);
    }
    assert_eq!(expanded_faces(&mesh), expected_faces(&input));
}

#[test]
fn complete_neighbour_rim_hides_all_internal_faces() {
    let input = PageHalo::filled(1);
    let (mesh, _) = run(&input);
    assert_eq!(mesh.quad_count(), 0);
    let mut input = PageHalo::from_page(&[1; PAGE_VOLUME], 0);
    // A neighbouring page's west rim hides the entire east side of this page.
    for y in 0..16 {
        for z in 0..16 {
            input.set(16, y, z, 1);
        }
    }
    let (mesh, _) = run(&input);
    assert_eq!(mesh.quad_count(), 5);
    assert!(mesh.iter().all(|quad| quad.face() != Face::East));
    assert_eq!(expanded_faces(&mesh), expected_faces(&input));
}

#[test]
fn opaque_cutout_water_buckets_and_transparent_groups_are_distinct() {
    let mut input = PageHalo::filled(0);
    input.set(1, 1, 1, 1);
    input.set(5, 1, 1, 2);
    input.set(9, 1, 1, 3);
    let (mesh, _) = run(&input);
    assert_eq!(
        [mesh.opaque.len(), mesh.cutout.len(), mesh.water.len()],
        [6, 6, 6]
    );
    assert_eq!(expanded_faces(&mesh), expected_faces(&input));
    for raw in [2, 3] {
        let mut input = PageHalo::filled(0);
        input.set(5, 5, 5, raw);
        input.set(6, 5, 5, raw);
        let (mesh, _) = run(&input);
        assert_eq!(
            mesh.quad_count(),
            6,
            "equal transparent groups form one prism"
        );
        assert_eq!(expanded_faces(&mesh), expected_faces(&input));
    }
}

#[test]
fn checkerboard_and_mixed_materials_have_exact_face_coverage() {
    let mut input = PageHalo::filled(0);
    for y in 0..16 {
        for z in 0..16 {
            for x in 0..16 {
                if (x + y + z) % 2 == 0 {
                    input.set(x, y, z, 1);
                }
            }
        }
    }
    let (mesh, _) = run(&input);
    assert_eq!(mesh.quad_count(), 2048 * 6);
    assert_eq!(expanded_faces(&mesh), expected_faces(&input));
    // A deterministic varied field checks all orientations, masks and material keys.
    let mut rng = 0x4b1d_5a77_u32;
    for _ in 0..4 {
        for y in -1..=16 {
            for z in -1..=16 {
                for x in -1..=16 {
                    rng ^= rng << 13;
                    rng ^= rng >> 17;
                    rng ^= rng << 5;
                    input.set(x, y, z, if rng.is_multiple_of(5) { 0 } else { rng % 4 });
                }
            }
        }
        let (mesh, _) = run(&input);
        assert_eq!(expanded_faces(&mesh), expected_faces(&input));
        assert!(mesh.quad_count() <= MAX_PAGE_QUADS);
    }
}

#[test]
fn descriptors_are_sixteen_bytes_and_vertices_wind_outward() {
    assert_eq!(std::mem::size_of::<QuadDescriptor>(), 16);
    assert_eq!(std::mem::align_of::<QuadDescriptor>(), 4);
    for face in Face::ALL {
        let input = PageHalo::from_page(&[1; PAGE_VOLUME], 0);
        let (mesh, _) = run(&input);
        let quad = *mesh.iter().find(|quad| quad.face() == face).unwrap();
        let rebuilt = QuadDescriptor::new(
            quad.origin(),
            face,
            [16, 16],
            65535,
            [0, 1, 2, 3],
            0x1234_5678,
        )
        .unwrap()
        .with_object(12345);
        assert_eq!(
            QuadDescriptor::from_le_bytes(rebuilt.to_le_bytes()),
            Ok(rebuilt)
        );
        assert_eq!(rebuilt.material(), 65535);
        assert_eq!(rebuilt.ao(), [0, 1, 2, 3]);
        assert_eq!(rebuilt.tint(), 0x1234_5678);
        assert_eq!(rebuilt.object(), 12345);
        let p0 = quad.vertex(0);
        let p1 = quad.vertex(1);
        let p2 = quad.vertex(2);
        let a: [i16; 3] = std::array::from_fn(|axis| p1[axis] - p0[axis]);
        let b: [i16; 3] = std::array::from_fn(|axis| p2[axis] - p0[axis]);
        let cross = [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ];
        assert_eq!(cross, face.normal().map(|n| i16::from(n) * 256));
        for vertex in 0..4 {
            assert!(
                quad.vertex(vertex)
                    .into_iter()
                    .all(|c| (0..=16).contains(&c))
            );
        }
    }
    assert_eq!(
        QuadDescriptor::new([17, 0, 0], Face::South, [1, 1], 0, [3; 4], 0),
        Err(DescriptorError::Origin)
    );
    assert_eq!(
        QuadDescriptor::new([0; 3], Face::South, [0, 1], 0, [3; 4], 0),
        Err(DescriptorError::Size)
    );
    assert_eq!(
        QuadDescriptor::new([0; 3], Face::South, [1, 1], 0, [4; 4], 0),
        Err(DescriptorError::AmbientOcclusion)
    );
    assert_eq!(
        QuadDescriptor::new([16; 3], Face::South, [1, 1], 0, [3; 4], 0),
        Err(DescriptorError::Bounds)
    );
    let mut malformed = QuadDescriptor::default();
    malformed.words[0] = 6 << 15;
    assert_eq!(
        QuadDescriptor::from_le_bytes(malformed.to_le_bytes()),
        Err(DescriptorError::Face)
    );
    malformed.words[0] = 1 << 26;
    assert_eq!(
        QuadDescriptor::from_le_bytes(malformed.to_le_bytes()),
        Err(DescriptorError::Reserved)
    );
    malformed.words[0] = 0;
    malformed.words[1] = 1 << 26;
    assert_eq!(
        QuadDescriptor::from_le_bytes(malformed.to_le_bytes()),
        Err(DescriptorError::Reserved)
    );
}

#[test]
fn corner_ao_uses_the_complete_rim_and_preserves_nonuniform_vertices() {
    let mut input = PageHalo::filled(0);
    input.set(8, 8, 8, 1);
    input.set(7, 9, 8, 4);
    input.set(8, 9, 9, 4);
    let (mesh, _) = run(&input);
    let up = mesh
        .opaque
        .iter()
        .find(|quad| quad.material() == 1 && quad.face() == Face::Up)
        .unwrap();
    assert_eq!(up.ao(), [0, 2, 2, 3]);
    assert_eq!(up.size(), [1, 1]);
    assert_eq!(expanded_faces(&mesh), expected_faces(&input));
    // At the page corner, all three AO samples can be in the halo's edge/corner.
    let mut input = PageHalo::filled(0);
    input.set(0, 15, 15, 1);
    input.set(-1, 16, 15, 4);
    input.set(0, 16, 16, 4);
    let (mesh, _) = run(&input);
    let up = mesh
        .opaque
        .iter()
        .find(|quad| quad.material() == 1 && quad.face() == Face::Up)
        .unwrap();
    assert_eq!(up.ao(), [0, 2, 2, 3]);
}

#[test]
fn face_materials_and_tints_survive_descriptor_packing() {
    let mut input = PageHalo::filled(0);
    input.set(5, 5, 5, 1);
    let mut scratch = MeshScratch::new();
    let mut mesh = PageMesh::default();
    mesh_page(
        &input,
        |raw| {
            if raw == 0 {
                CellMaterial::EMPTY
            } else {
                CellMaterial {
                    face_materials: [10, 11, 12, 13, 14, 15],
                    face_uv_rotations: [0, 1, 2, 3, 0, 1],
                    tint: 0x1234_56ff,
                    ..CellMaterial::opaque(0)
                }
            }
        },
        &mut scratch,
        &mut mesh,
    );
    assert_eq!(mesh.quad_count(), 6);
    for quad in mesh.iter() {
        assert_eq!(quad.material(), 10 + quad.face() as u16);
        assert_eq!(quad.tint(), 0x1234_56ff);
        assert_eq!(quad.uv_rotation(), quad.face() as u8 % 4);
    }
}

#[test]
fn uv_quarter_turns_round_trip_and_prevent_wrong_greedy_merges() {
    let (cube, _) = run(&PageHalo::from_page(&[1; PAGE_VOLUME], 0));
    for base in cube.iter() {
        for rotation in 0..4 {
            let quad = QuadDescriptor::new(
                base.origin(),
                base.face(),
                [2, 3],
                65535,
                [0, 1, 2, 3],
                0x1234_5678,
            )
            .unwrap()
            .with_object(987)
            .with_uv_rotation(rotation)
            .unwrap();
            assert_eq!(QuadDescriptor::from_le_bytes(quad.to_le_bytes()), Ok(quad));
            assert_eq!(quad.uv_rotation(), rotation);
            assert_eq!(quad.material(), 65535);
            assert_eq!(quad.ao(), [0, 1, 2, 3]);
            assert_eq!(quad.object(), 987);
            for (corner, [u, v]) in [[0.0, 0.0], [2.0, 0.0], [0.0, 3.0], [2.0, 3.0]]
                .into_iter()
                .enumerate()
            {
                let expected = match rotation {
                    0 => [u, v],
                    1 => [v, 1.0 - u],
                    2 => [1.0 - u, 1.0 - v],
                    _ => [1.0 - v, u],
                };
                assert_eq!(quad.vertex_uv(corner as u8), expected);
            }
            assert_eq!(quad.with_uv_rotation(4), Err(DescriptorError::UvRotation));
            assert_eq!(quad.with_uv_rotation(0).unwrap().words[1] & 0x0300_0000, 0);
        }
    }
    let mut input = PageHalo::filled(0);
    input.set(5, 5, 5, 1);
    input.set(6, 5, 5, 1);
    let mut scratch = MeshScratch::new();
    let mut output = PageMesh::default();
    mesh_page_indexed(
        &input,
        |raw, position| CellMaterial {
            face_uv_rotations: [position[0] as u8 & 1; 6],
            ..material(raw)
        },
        &mut scratch,
        &mut output,
    );
    let top: Vec<_> = output
        .iter()
        .filter(|quad| quad.face() == Face::Up)
        .collect();
    assert_eq!(top.len(), 2, "differently rotated cells must not merge");
    assert!(top.iter().all(|quad| quad.size() == [1, 1]));
    assert_ne!(top[0].uv_rotation(), top[1].uv_rotation());
}

#[test]
fn untinted_material_faces_do_not_inherit_biome_colour() {
    let mut input = PageHalo::filled(0);
    input.set(5, 5, 5, 1);
    let mut scratch = MeshScratch::new();
    let mut mesh = PageMesh::default();
    mesh_page(
        &input,
        |raw| {
            if raw == 0 {
                CellMaterial::EMPTY
            } else {
                CellMaterial {
                    tint: 0xff40_a020,
                    tint_faces: 1 << Face::Up as u8,
                    ..CellMaterial::opaque(1)
                }
            }
        },
        &mut scratch,
        &mut mesh,
    );
    for quad in mesh.iter() {
        assert_eq!(
            quad.tint(),
            if quad.face() == Face::Up {
                0xff40_a020
            } else {
                u32::MAX
            }
        );
    }
}

#[test]
fn indexed_classification_preserves_each_columns_biome_tint() {
    let input = PageHalo::from_page(&[1; PAGE_VOLUME], 0);
    let mut scratch = MeshScratch::new();
    let mut mesh = PageMesh::default();
    let work = mesh_page_indexed(
        &input,
        |raw, [x, _, z]| {
            if raw == 0 {
                CellMaterial::EMPTY
            } else {
                CellMaterial {
                    tint: 0xff80_0000 | x as u32 | ((z as u32) << 8),
                    ..CellMaterial::opaque(1)
                }
            }
        },
        &mut scratch,
        &mut mesh,
    );
    let top: Vec<_> = mesh
        .opaque
        .iter()
        .filter(|quad| quad.face() == Face::Up)
        .collect();
    assert_eq!(top.len(), 16 * 16, "distinct tint columns must not merge");
    for quad in top {
        let [x, _, z] = quad.origin();
        assert_eq!(quad.size(), [1, 1]);
        assert_eq!(
            quad.tint(),
            0xff80_0000 | u32::from(x) | (u32::from(z - 1) << 8)
        );
    }
    assert_eq!(work.work.cells_scanned, HALO_VOLUME as u64);
}

#[test]
fn isolated_block_edit_has_a_fixed_input_bound_and_small_payload() {
    let mut input = PageHalo::from_page(&[1; PAGE_VOLUME], 0);
    let (before, before_work) = run(&input);
    input.set(8, 8, 8, 0);
    let (after, after_work) = run(&input);
    assert_eq!(before_work.work.cells_scanned, HALO_VOLUME as u64);
    assert_eq!(
        after_work.work.cells_scanned,
        before_work.work.cells_scanned
    );
    assert_eq!(after.quad_count(), 12);
    assert_eq!(after.upload_bytes() - before.upload_bytes(), 6 * 16);
    assert!(after.upload_bytes() <= 256);
    assert_eq!(expanded_faces(&after), expected_faces(&input));
}

#[test]
fn classifications_are_counted_once_and_reused_without_allocations() {
    let mut input = PageHalo::filled(0);
    for y in 0..16 {
        for z in 0..16 {
            for x in 0..16 {
                input.set(x, y, z, 1 + x as u32 + 16 * (z as u32 + 16 * y as u32));
            }
        }
    }
    let calls = AtomicUsize::new(0);
    let classifier = |raw| {
        calls.fetch_add(1, Ordering::Relaxed);
        if raw == 0 {
            CellMaterial::EMPTY
        } else {
            CellMaterial {
                class: MeshClass::Cutout,
                occludes: false,
                casts_ao: false,
                cull_group: 0,
                ..CellMaterial::opaque(raw as u16)
            }
        }
    };
    let mut scratch = MeshScratch::new();
    let mut mesh = PageMesh::with_capacity([0, MAX_PAGE_QUADS, 0]);
    let positive = telemetry::AllocationScope::begin().unwrap();
    let allocated = std::hint::black_box(vec![7_u8; 1024]);
    std::hint::black_box(&allocated);
    drop(allocated);
    assert!(
        positive.finish().allocating_calls() > 0,
        "counting allocator must be active"
    );
    let scope = telemetry::AllocationScope::begin().unwrap();
    for _ in 0..3 {
        let work = mesh_page(&input, classifier, &mut scratch, &mut mesh);
        assert_eq!(work.work.cells_scanned, HALO_VOLUME as u64);
        assert_eq!(mesh.quad_count(), MAX_PAGE_QUADS);
        assert_eq!(work.generated_bytes, (MAX_PAGE_QUADS * 16) as u64);
    }
    let counts = scope.finish();
    assert_eq!(counts.allocating_calls(), 0, "{counts:?}");
    assert_eq!(counts.realloc_calls, 0, "{counts:?}");
    assert_eq!(calls.load(Ordering::Relaxed), HALO_VOLUME * 3);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct DetailedFace {
    face: UnitFace,
    ao: [u8; 4],
    uv_rotation: u8,
}

fn detailed_faces(mesh: &PageMesh) -> BTreeMap<DetailedFace, usize> {
    let mut faces = BTreeMap::new();
    for (bucket, quads) in [&mesh.opaque, &mesh.cutout, &mesh.water]
        .into_iter()
        .enumerate()
    {
        for quad in quads {
            // Greedy only merges uniform AO, so every covered cell has these
            // exact four values. Nonuniform patterns retain their unit quad.
            if quad.size() != [1, 1] {
                assert!(quad.ao().iter().all(|value| *value == quad.ao()[0]));
            }
            let one = match bucket {
                0 => PageMesh {
                    opaque: vec![*quad],
                    ..PageMesh::default()
                },
                1 => PageMesh {
                    cutout: vec![*quad],
                    ..PageMesh::default()
                },
                _ => PageMesh {
                    water: vec![*quad],
                    ..PageMesh::default()
                },
            };
            for (face, count) in expanded_faces(&one) {
                assert_eq!(face.bucket, bucket as u8);
                let previous = faces.insert(
                    DetailedFace {
                        face,
                        ao: quad.ao(),
                        uv_rotation: quad.uv_rotation(),
                    },
                    count,
                );
                assert!(previous.is_none(), "duplicate unit face");
            }
        }
    }
    faces
}

#[test]
fn edit_patch_replaces_old_masked_faces_with_exact_updated_greedy_faces_and_ao() {
    // Both edits cross a negative-coordinate page corner. Every page samples the
    // same world snapshot; the patch itself only sees local owner coordinates.
    for edit in [[7_i32, 8, 9], [-16, 0, -16]] {
        let base = edit.map(|axis| axis.div_euclid(16));
        for py in base[1] - 1..=base[1] + 1 {
            for pz in base[2] - 1..=base[2] + 1 {
                for px in base[0] - 1..=base[0] + 1 {
                    let page_origin = [px * 16, py * 16, pz * 16];
                    let owners: Vec<[u8; 3]> = (-1..=1)
                        .flat_map(|dy| {
                            (-1..=1).flat_map(move |dz| (-1..=1).map(move |dx| [dx, dy, dz]))
                        })
                        .map(|delta| {
                            std::array::from_fn::<_, 3, _>(|axis| {
                                edit[axis] + delta[axis] - page_origin[axis]
                            })
                        })
                        .filter(|local| local.iter().all(|axis| (0..16).contains(axis)))
                        .map(|local| local.map(|axis| axis as u8))
                        .collect();
                    if owners.is_empty() {
                        continue;
                    }
                    let raw = |world: [i32; 3], edited: bool| {
                        if world == edit {
                            return if edited { 0 } else { 1 };
                        }
                        let hash = world[0].wrapping_mul(73856093)
                            ^ world[1].wrapping_mul(19349663)
                            ^ world[2].wrapping_mul(83492791);
                        if (hash as u32) % 5 < 2 {
                            0
                        } else {
                            1 + (hash as u32) % 3
                        }
                    };
                    let classify = |world: [i32; 3], edited| {
                        let mut result = material(raw(world, edited));
                        result.tint = 0xff00_0000
                            | ((world[0] as u32) & 255)
                            | (((world[2] as u32) & 255) << 8);
                        result.tint_faces = 0b01_1010;
                        result.face_uv_rotations = std::array::from_fn(|face| {
                            (face as u8).wrapping_add(world[0] as u8) & 3
                        });
                        result
                    };
                    let mut old_input = PageHalo::filled(0);
                    let mut new_input = PageHalo::filled(0);
                    for y in -1..=16 {
                        for z in -1..=16 {
                            for x in -1..=16 {
                                let world = [
                                    page_origin[0] + i32::from(x),
                                    page_origin[1] + i32::from(y),
                                    page_origin[2] + i32::from(z),
                                ];
                                old_input.set(x, y, z, raw(world, false));
                                new_input.set(x, y, z, raw(world, true));
                            }
                        }
                    }
                    let mut scratch = MeshScratch::new();
                    let mut old_mesh = PageMesh::default();
                    let mut new_mesh = PageMesh::default();
                    mesh_page_indexed(
                        &old_input,
                        |_, local| {
                            classify(
                                std::array::from_fn(|axis| {
                                    page_origin[axis] + i32::from(local[axis])
                                }),
                                false,
                            )
                        },
                        &mut scratch,
                        &mut old_mesh,
                    );
                    mesh_page_indexed(
                        &new_input,
                        |_, local| {
                            classify(
                                std::array::from_fn(|axis| {
                                    page_origin[axis] + i32::from(local[axis])
                                }),
                                true,
                            )
                        },
                        &mut scratch,
                        &mut new_mesh,
                    );
                    let mut patch = PageMesh::with_capacity([MAX_CELL_PATCH_QUADS; 3]);
                    let calls = AtomicUsize::new(0);
                    let work = mesh_cell_patch(
                        &owners,
                        |local| {
                            assert!(local.iter().all(|axis| (-1..=16).contains(axis)));
                            calls.fetch_add(1, Ordering::Relaxed);
                            classify(
                                std::array::from_fn(|axis| {
                                    page_origin[axis] + i32::from(local[axis])
                                }),
                                true,
                            )
                        },
                        &mut patch,
                    )
                    .unwrap();
                    assert_eq!(calls.load(Ordering::Relaxed), owners.len() * 27);
                    assert_eq!(work.work.cells_scanned, (owners.len() * 27) as u64);
                    assert_eq!(work.work.upload_bytes, 0);
                    assert_eq!(work.work.quads_emitted as usize, patch.quad_count());
                    assert_eq!(work.ao_samples, work.work.quads_emitted * 12);
                    assert!(patch.quad_count() <= owners.len() * 6);
                    let masked = |face: &DetailedFace| {
                        owners
                            .iter()
                            .any(|owner| owner.map(i16::from) == face.face.cell)
                    };
                    let mut composed = detailed_faces(&old_mesh);
                    composed.retain(|face, _| !masked(face));
                    for (face, count) in detailed_faces(&patch) {
                        assert!(masked(&face));
                        assert!(composed.insert(face, count).is_none());
                    }
                    assert_eq!(
                        composed,
                        detailed_faces(&new_mesh),
                        "edit {edit:?}, origin {page_origin:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn diagonal_owner_ao_requires_more_than_the_seven_axial_edit_cells() {
    let mut input = PageHalo::filled(0);
    input.set(8, 8, 8, 1);
    let (old, _) = run(&input);
    // This diagonal corner casts AO on [8,8,8], which is not one of this edit's
    // seven axial owners. Masking/rebuilding only seven would retain stale AO.
    input.set(7, 9, 9, 4);
    let (new, _) = run(&input);
    let up = |mesh: &PageMesh| {
        mesh.iter()
            .find(|quad| quad.material() == 1 && quad.face() == Face::Up)
            .unwrap()
            .ao()
    };
    assert_ne!(up(&old), up(&new));
    let mut patch = PageMesh::with_capacity([6, 0, 0]);
    mesh_cell_patch(
        &[[8, 8, 8]],
        |[x, y, z]| material(input.get(x, y, z)),
        &mut patch,
    )
    .unwrap();
    assert_eq!(up(&patch), up(&new));
    assert_eq!(up(&patch), [2, 3, 3, 3]);
}

#[test]
fn patch_validation_and_capacity_failures_leave_output_unchanged() {
    let marker = QuadDescriptor::new([0, 0, 0], Face::South, [1, 1], 65535, [3; 4], 0).unwrap();
    let mut output = PageMesh::with_capacity([1, 0, 0]);
    output.opaque.push(marker);
    let mut calls = 0;
    let mut sample = |_| {
        calls += 1;
        CellMaterial::opaque(1)
    };
    for (cells, error) in [
        (
            vec![[0; 3]; MAX_CELL_PATCH_CELLS + 1],
            CellPatchError::TooManyCells,
        ),
        (vec![[16, 0, 0]], CellPatchError::Coordinate),
        (vec![[0; 3]; 2], CellPatchError::DuplicateCell),
    ] {
        assert_eq!(
            mesh_cell_patch(&cells, &mut sample, &mut output),
            Err(error)
        );
        assert_eq!(output.opaque, [marker]);
    }
    assert_eq!(calls, 0);
    assert_eq!(
        mesh_cell_patch(
            &[[0; 3]],
            |_| CellMaterial {
                occludes: false,
                ..CellMaterial::opaque(1)
            },
            &mut output
        ),
        Err(CellPatchError::Capacity {
            required: [6, 0, 0]
        })
    );
    assert_eq!(output.opaque, [marker]);
    let work = mesh_cell_patch(&[], |_| panic!("empty patch has no samples"), &mut output).unwrap();
    assert_eq!(work, MeshWork::default());
    assert_eq!(output.quad_count(), 0);
}

#[test]
fn edit_patch_has_no_allocations_and_counts_every_neighbourhood_read() {
    let mut cells = [[0_u8; 3]; MAX_CELL_PATCH_CELLS];
    let mut index = 0;
    for y in 7..=9 {
        for z in 7..=9 {
            for x in 7..=9 {
                cells[index] = [x, y, z];
                index += 1;
            }
        }
    }
    let mut output = PageMesh::with_capacity([0, MAX_CELL_PATCH_QUADS, 0]);
    let calls = AtomicUsize::new(0);
    let sample = |_| {
        calls.fetch_add(1, Ordering::Relaxed);
        CellMaterial {
            class: MeshClass::Cutout,
            occludes: false,
            casts_ao: true,
            ..CellMaterial::opaque(1)
        }
    };
    let positive = telemetry::AllocationScope::begin().unwrap();
    let allocated = std::hint::black_box(vec![7_u8; 1024]);
    std::hint::black_box(&allocated);
    drop(allocated);
    assert!(positive.finish().allocating_calls() > 0);
    let scope = telemetry::AllocationScope::begin().unwrap();
    for _ in 0..3 {
        let work = mesh_cell_patch(&cells, sample, &mut output).unwrap();
        assert_eq!(work.work.cells_scanned, 729);
        assert_eq!(work.work.quads_emitted, 162);
        assert_eq!(work.generated_bytes, 2592);
    }
    let counts = scope.finish();
    assert_eq!(counts.allocating_calls(), 0, "{counts:?}");
    assert_eq!(counts.realloc_calls, 0, "{counts:?}");
    assert_eq!(calls.load(Ordering::Relaxed), 729 * 3);
}
