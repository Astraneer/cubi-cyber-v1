#[path = "../src/height_index.rs"]
#[allow(dead_code)]
mod height_index;
#[path = "../src/height_tile.rs"]
#[allow(dead_code)]
mod height_tile;
use core_types::{BlockPos, Cell, SpaceId, TxId};
use height_index::*;
use height_tile::*;
fn key(level: u8, x: i32, z: i32) -> TileKey {
    TileKey {
        space: SpaceId(0),
        level,
        x,
        z,
    }
}
fn baseline(x: i32, z: i32) -> Option<PublicTerrainSurface> {
    Some(PublicTerrainSurface {
        height: 32 + (x * x + 3 * z * z).rem_euclid(13),
        ground_y: 31 + (x * x + 3 * z * z).rem_euclid(13),
        atlas_layer: 3,
        tint: 0xff123456,
        protected: false,
    })
}
#[test]
fn persisted_air_and_roof_edits_protect_far_geometry_after_page_eviction() {
    let mut index = HeightEditIndex::new(IndexLimits::default()).unwrap();
    let p = BlockPos {
        space: SpaceId(0),
        x: 16384 + 7,
        y: 33,
        z: 9,
    };
    let edits = [
        CellEdit {
            position: p,
            cell: Cell::AIR,
        },
        CellEdit {
            position: BlockPos { y: 200, ..p },
            cell: Cell::from_parts(rules::ids::STONE, 0),
        },
    ];
    index.apply_committed(TxId(1), &edits).unwrap();
    let restored =
        HeightEditIndex::decode_snapshot(&index.encode_snapshot().unwrap(), IndexLimits::default())
            .unwrap();
    let snapshot = restored.capture(key(0, 1024, 0), 4096, 65536).unwrap();
    let (tile, work) = build_tile(&snapshot, baseline, HeightBuildLimits::default()).unwrap();
    assert!(tile.cell_protected(7, 9));
    assert_eq!(work.protected_cells, 256);
    assert_eq!(work.edited_cells, 2);
    assert_eq!(work.world_reads, 0);
    assert_eq!(work.baseline_columns, 290);
    assert_eq!(
        tile.heights[7 + 17 * 9],
        baseline(p.x, p.z).unwrap().height,
        "coarse ramp invented through an arch"
    );
    let mut later =
        HeightEditIndex::decode_snapshot(&index.encode_snapshot().unwrap(), IndexLimits::default())
            .unwrap();
    later
        .apply_committed(
            TxId(2),
            &[CellEdit {
                position: p,
                cell: Cell::from_parts(rules::ids::DIRT, 0),
            }],
        )
        .unwrap();
    assert_ne!(
        later.source(tile.key).unwrap(),
        tile.source,
        "far edit did not invalidate source"
    );
    let other = key(0, 1025, 0);
    assert_eq!(
        later.source(other).unwrap(),
        1,
        "unrelated tile source changed"
    );
}
#[test]
fn exact_shared_samples_match_every_level_and_unknown_does_not_turn_into_ground() {
    let index = HeightEditIndex::new(IndexLimits::default()).unwrap();
    let fine = build_tile(
        &index.capture(key(0, 1, 0), 4096, 65536).unwrap(),
        baseline,
        HeightBuildLimits::default(),
    )
    .unwrap()
    .0;
    let coarse = build_tile(
        &index.capture(key(1, 1, 0), 4096, 65536).unwrap(),
        baseline,
        HeightBuildLimits::default(),
    )
    .unwrap()
    .0;
    for z in (0..17).step_by(2) {
        assert_eq!(fine.heights[16 + 17 * z], coarse.heights[17 * (z / 2)]);
    }
    let (unknown, work) = build_tile(
        &index.capture(key(0, 0, 0), 4096, 65536).unwrap(),
        |x, z| {
            if x == 8 && z == 8 {
                None
            } else {
                baseline(x, z)
            }
        },
        HeightBuildLimits::default(),
    )
    .unwrap();
    assert!(!unknown.sample_known(8, 8));
    assert_eq!(unknown.heights[8 + 17 * 8], 0);
    assert_eq!(unknown.known[36] & !1, 0);
    assert_eq!(work.baseline_columns, 289);
}
#[test]
fn oversized_private_snapshot_and_invalid_material_are_rejected() {
    let mut index = HeightEditIndex::new(IndexLimits::default()).unwrap();
    index
        .apply_committed(
            TxId(1),
            &[CellEdit {
                position: BlockPos {
                    space: SpaceId(0),
                    x: 3,
                    y: 3,
                    z: 3,
                },
                cell: Cell::AIR,
            }],
        )
        .unwrap();
    let snapshot = index.capture(key(0, 0, 0), 4096, 65536).unwrap();
    assert_eq!(
        build_tile(
            &snapshot,
            baseline,
            HeightBuildLimits {
                max_cells: 0,
                ..HeightBuildLimits::default()
            }
        ),
        Err(HeightBuildError::InvalidLimits)
    );
    assert_eq!(
        build_tile(
            &snapshot,
            |_, _| Some(PublicTerrainSurface {
                height: 3,
                ground_y: 2,
                atlas_layer: 256,
                tint: 0,
                protected: false
            }),
            HeightBuildLimits::default()
        ),
        Err(HeightBuildError::InvalidMaterial)
    );
}

#[test]
fn buried_edits_keep_untouched_surface_and_visible_edits_refine_to_full_column() {
    let mut index = HeightEditIndex::new(IndexLimits::default()).unwrap();
    let p = BlockPos {
        space: SpaceId(0),
        x: 7,
        y: 2,
        z: 9,
    };
    index
        .apply_committed(
            TxId(1),
            &[CellEdit {
                position: p,
                cell: Cell::AIR,
            }],
        )
        .unwrap();
    let (buried, _) = build_tile(
        &index.capture(key(0, 0, 0), 4096, 65536).unwrap(),
        baseline,
        HeightBuildLimits::default(),
    )
    .unwrap();
    assert_eq!(buried.replacement, ReplacementRequirement::None);
    assert_eq!(
        buried.protected, [0; 32],
        "buried AIR edit punched a crater through untouched ground"
    );
    index
        .apply_committed(
            TxId(2),
            &[CellEdit {
                position: BlockPos { y: 200, ..p },
                cell: Cell::from_parts(rules::ids::STONE, 0),
            }],
        )
        .unwrap();
    let (coarse, _) = build_tile(
        &index.capture(key(3, 0, 0), 4096, 65536).unwrap(),
        baseline,
        HeightBuildLimits::default(),
    )
    .unwrap();
    assert_eq!(
        coarse.replacement,
        ReplacementRequirement::Refine {
            children: [key(2, 0, 0), key(2, 1, 0), key(2, 0, 1), key(2, 1, 1)]
        }
    );
    let (leaf, _) = build_tile(
        &index.capture(key(0, 0, 0), 4096, 65536).unwrap(),
        baseline,
        HeightBuildLimits::default(),
    )
    .unwrap();
    assert_eq!(
        leaf.replacement,
        ReplacementRequirement::Column {
            source: leaf.source,
            min_page_y: 0,
            max_page_y: 12
        }
    );
    assert_eq!(leaf.protected, [255; 32]);
    index
        .apply_committed(
            TxId(3),
            &[CellEdit {
                position: BlockPos { y: 10000, ..p },
                cell: Cell::from_parts(rules::ids::STONE, 0),
            }],
        )
        .unwrap();
    assert_eq!(
        build_tile(
            &index.capture(key(0, 0, 0), 4096, 65536).unwrap(),
            baseline,
            HeightBuildLimits::default()
        ),
        Err(HeightBuildError::ReplacementTooTall)
    );
}
#[test]
fn structures_between_sparse_samples_require_refinement_and_canonical_materials_are_shared() {
    let index = HeightEditIndex::new(IndexLimits::default()).unwrap();
    let snapshot = index.capture(key(4, 0, 0), 4096, 65536).unwrap();
    let (tile, _) = build_tile_with_regions(
        &snapshot,
        &[[17, 17, 18, 18]],
        baseline,
        HeightBuildLimits::default(),
    )
    .unwrap();
    assert!(tile.cell_protected(1, 1));
    assert!(matches!(
        tile.replacement,
        ReplacementRequirement::Refine { .. }
    ));
    let generator = r#gen::Generator::new(r#gen::TerrainSeed(1337));
    let snapshot = index.capture(key(0, 1024, 0), 4096, 65536).unwrap();
    let (tile, work) =
        build_canonical_tile(&generator, &snapshot, HeightBuildLimits::default()).unwrap();
    assert_eq!(work.canonical_noise_columns, 1445);
    assert_eq!(work.world_reads, 0);
    assert_eq!(tile.replacement, ReplacementRequirement::None);
    for z in 0..17 {
        for x in 0..17 {
            let surface = generator.public_terrain_column(16384 + x, z).unwrap();
            assert_eq!(tile.heights[(x + 17 * z) as usize], surface.surface_y + 1);
            if x < 16 && z < 16 {
                assert_eq!(
                    tile.atlas[(x + 16 * z) as usize],
                    rules::blocks::layer_cell(surface.surface.id(), 0, rules::blocks::mesh_face(1))
                        .0 as u16
                );
            }
        }
    }
}
