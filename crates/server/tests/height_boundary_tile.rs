//! Whole body plus one-cell rim: symmetric protected-owner authority checks.
use core_types::{BlockPos, Cell, SpaceId, TxId};
use server::{
    height_index::{CellEdit, HeightEditIndex, IndexLimits, TileKey},
    height_tile::{
        HeightBuildLimits, PublicTerrainSurface, ReplacementRequirement, build_canonical_tile,
        build_tile,
    },
};
fn key(level: u8) -> TileKey {
    TileKey {
        space: SpaceId(0),
        level,
        x: -2,
        z: 3,
    }
}
fn baseline(_: i32, _: i32) -> Option<PublicTerrainSurface> {
    Some(PublicTerrainSurface {
        height: 71,
        ground_y: 70,
        atlas_layer: 3,
        tint: u32::MAX,
        protected: false,
    })
}
#[test]
fn all_four_edges_and_corners_protect_the_nearest_actual_owner_at_every_level() {
    for level in 0..=10 {
        let k = key(level);
        let b = k.bounds().unwrap();
        let centre = [b[0] + (b[2] - b[0]) / 2, b[1] + (b[3] - b[1]) / 2];
        for (x, z, cx, cz) in [
            (b[0] - 1, centre[1], 0, 8),
            (b[2], centre[1], 15, 8),
            (centre[0], b[1] - 1, 8, 0),
            (centre[0], b[3], 8, 15),
            (b[0] - 1, b[1] - 1, 0, 0),
            (b[0] - 1, b[3], 0, 15),
            (b[2], b[1] - 1, 15, 0),
            (b[2], b[3], 15, 15),
        ] {
            let mut index = HeightEditIndex::new(IndexLimits::default()).unwrap();
            index
                .apply_committed(
                    TxId(1),
                    &[CellEdit {
                        position: BlockPos {
                            space: SpaceId(0),
                            x: x as i32,
                            y: 70,
                            z: z as i32,
                        },
                        cell: Cell::AIR,
                    }],
                )
                .unwrap();
            let captured = index.capture(k, 16, 16).unwrap();
            assert_eq!(captured.source, 2);
            assert_eq!(captured.columns.len(), 1);
            let (tile, work) =
                build_tile(&captured, baseline, HeightBuildLimits::default()).unwrap();
            assert!(
                tile.cell_protected(cx, cz),
                "edge/corner level{level} ({x},{z}) did not protect({cx},{cz})"
            );
            assert_eq!(work.edited_cells, 1);
            assert_eq!(work.world_reads, 0);
            if level == 0 {
                assert_eq!(
                    tile.replacement,
                    ReplacementRequirement::Column {
                        source: 2,
                        min_page_y: 0,
                        max_page_y: 7
                    }
                );
            } else {
                assert!(matches!(
                    tile.replacement,
                    ReplacementRequirement::Refine { .. }
                ));
            }
        }
    }
}
#[test]
fn foreign_rows_cannot_extend_owner_vertical_range_or_bypass_input_halo_bounds() {
    let k = key(0);
    let b = k.bounds().unwrap();
    let mut index = HeightEditIndex::new(IndexLimits::default()).unwrap();
    index
        .apply_committed(
            TxId(1),
            &[CellEdit {
                position: BlockPos {
                    space: SpaceId(0),
                    x: b[0] as i32 - 1,
                    y: 10_000,
                    z: b[1] as i32 + 7,
                },
                cell: Cell::from_parts(rules::ids::STONE, 0),
            }],
        )
        .unwrap();
    let mut captured = index.capture(k, 16, 16).unwrap();
    let (tile, _) = build_tile(&captured, baseline, HeightBuildLimits::default()).unwrap();
    assert_eq!(
        tile.replacement,
        ReplacementRequirement::Column {
            source: 2,
            min_page_y: 0,
            max_page_y: 7
        }
    );
    captured.columns[0].x -= 1;
    assert_eq!(
        build_tile(&captured, baseline, HeightBuildLimits::default()),
        Err(server::height_tile::HeightBuildError::InvalidInput)
    );
}
#[test]
fn canonical_empty_old_world_keeps_sample_error_and_no_replacement() {
    let generator = r#gen::Generator::new(r#gen::TerrainSeed(1));
    let index = HeightEditIndex::new(IndexLimits::default()).unwrap();
    let k = TileKey {
        space: SpaceId(0),
        level: 0,
        x: 3,
        z: -3,
    };
    let (tile, work) = build_canonical_tile(
        &generator,
        &index.capture(k, 4096, 65536).unwrap(),
        HeightBuildLimits::default(),
    )
    .unwrap();
    assert_eq!(tile.heights, [71; 289]);
    assert_eq!(tile.error.unwrap().max_error_q1024, 0);
    assert_eq!(tile.replacement, ReplacementRequirement::None);
    assert_eq!(work.baseline_columns, 289);
    assert_eq!(work.canonical_noise_columns, 1445);
}
