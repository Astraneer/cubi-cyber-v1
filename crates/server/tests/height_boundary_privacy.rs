//! Isolated candidate tests; authored without executing Cargo or a GPU.
//!
//! Assumptions under review: canonical foreign-halo relevance uses the PUBLIC
//! terrain envelope, ignores material-only changes between known full opaque
//! cubes, distinguishes public render/opacity/fluid changes, and fails
//! conservatively for unknown materials. Own-body behavior remains unchanged.
//! The current y >= baseline ground predicate intentionally leaves a closed,
//! buried AIR edit occluded. These tests do not certify all externally opened
//! tunnel walls, exterior flood reachability, UI edits, or WAL durability.
use core_types::{BlockPos, Cell, SpaceId, TxId};
use r#gen::{Generator, TerrainSeed};
use server::{
    height_heads::{HeightHeads, Limits as HeadLimits},
    height_index::{CellEdit, HeightEditIndex, IndexLimits, TileKey},
    height_tile::{
        HeightBuildLimits, PublicHeightTile, ReplacementRequirement, build_canonical_tile,
    },
};

const HIGH: TileKey = TileKey {
    space: SpaceId(0),
    level: 0,
    x: 3,
    z: -3,
};
const CONNECTION: u64 = 71;

fn index() -> HeightEditIndex {
    HeightEditIndex::new(IndexLimits::default()).unwrap()
}

fn cell(id: u16) -> Cell {
    Cell::from_parts(id, 0)
}

fn foreign(y: i32, value: Cell) -> CellEdit {
    CellEdit {
        position: BlockPos {
            space: SpaceId(0),
            x: 47,
            y,
            z: -40,
        },
        cell: value,
    }
}

fn build(generator: &Generator, edits: &HeightEditIndex) -> PublicHeightTile {
    let capture = edits.capture(HIGH, 4096, 65536).unwrap();
    let (tile, _) =
        build_canonical_tile(generator, &capture, HeightBuildLimits::default()).unwrap();
    tile
}

fn baseline(generator: &Generator) -> PublicHeightTile {
    let tile = build(generator, &index());
    assert_eq!(tile.key, HIGH);
    assert_eq!(tile.source, 1);
    assert_eq!(tile.heights, [71; 289]);
    assert_eq!(tile.error.unwrap().max_error_q1024, 0);
    assert_eq!(tile.error.unwrap().min_height, 71);
    assert_eq!(tile.error.unwrap().max_height, 71);
    assert_none(&tile);
    assert_eq!(
        generator.public_terrain_column(47, -40).unwrap().ground_y,
        70
    );
    tile
}

fn assert_none(tile: &PublicHeightTile) {
    assert_eq!(tile.protected, [0; 32]);
    assert_eq!(tile.replacement, ReplacementRequirement::None);
}

fn assert_own_profile_column(tile: &PublicHeightTile) {
    assert_eq!(tile.protected, [255; 32]);
    assert_eq!(
        tile.replacement,
        ReplacementRequirement::Column {
            source: tile.source,
            min_page_y: 0,
            max_page_y: 7,
        }
    );
}

fn assert_same_baseline_samples(tile: &PublicHeightTile, original: &PublicHeightTile) {
    assert_eq!(tile.key, original.key);
    assert_eq!(tile.error, original.error);
    assert_eq!(tile.heights, original.heights);
    assert_eq!(tile.known, original.known);
    assert_eq!(tile.atlas, original.atlas);
    assert_eq!(tile.tint, original.tint);
}

// Publish only complete None representations here. Column geometry/manifest
// readiness belongs to the separately exercised exact public geometry path.
fn none_wire(tile: &PublicHeightTile) -> net::height::HeightTile {
    assert_none(tile);
    let wire = net::height::HeightTile {
        key: net::height::HeightKey {
            space: tile.key.space.0,
            level: tile.key.level,
            x: tile.key.x,
            z: tile.key.z,
        },
        source: tile.source,
        error: tile.error,
        heights: tile.heights,
        known: tile.known,
        atlas: tile.atlas,
        tint: tile.tint,
        protected: tile.protected,
        replacement: net::height::Replacement::None,
    };
    wire.validate().unwrap();
    wire
}

fn publish(heads: &mut HeightHeads, tile: &PublicHeightTile) -> (u64, Vec<u8>) {
    let mut wire = none_wire(tile);
    let source = heads.publish(CONNECTION, tile.source, &mut wire).unwrap();
    assert_eq!(
        heads.private_source(CONNECTION, wire.key),
        Some(tile.source)
    );
    assert_eq!(heads.public_source(CONNECTION, wire.key), Some(source));
    (source, net::height::encode(&wire).unwrap())
}

#[test]
fn actual_surface_removal_and_foreign_roof_protect_without_extending_own_profile() {
    let generator = Generator::new(TerrainSeed(1));
    let original = baseline(&generator);
    for edit in [
        foreign(70, Cell::AIR),
        foreign(10_000, cell(rules::ids::STONE)),
    ] {
        let mut edits = index();
        edits.apply_committed(TxId(1), &[edit]).unwrap();
        let captured = edits.capture(HIGH, 4096, 65536).unwrap();
        assert_eq!(
            captured.source, 2,
            "the neighboring height input must be invalidated"
        );
        assert_eq!(captured.column(47, -40).unwrap().cells[0].cell, edit.cell);
        let saved = edits.encode_snapshot().unwrap();
        let restored = HeightEditIndex::decode_snapshot(&saved, IndexLimits::default()).unwrap();
        let tile = build(&generator, &restored);
        assert_eq!(tile.source, captured.source);
        assert_own_profile_column(&tile);
        assert_same_baseline_samples(&tile, &original);
        assert!(tile.error.unwrap().max_height < edit.position.y || edit.position.y == 70);
    }
}

#[test]
fn opaque_foreign_materials_change_private_inputs_but_not_public_height_identity() {
    let generator = Generator::new(TerrainSeed(1));
    let original = baseline(&generator);
    let mut edits = index();
    let mut heads = HeightHeads::new(HeadLimits::default()).unwrap();
    let published_baseline = publish(&mut heads, &original);
    let opaque_ids = [
        rules::ids::STONE,
        rules::ids::DIRT,
        rules::ids::COAL_ORE,
        rules::ids::IRON_ORE,
        rules::ids::OSMIUM_ORE,
        rules::ids::TIN_ORE,
        rules::ids::COPPER_ORE,
        rules::ids::GOLD_ORE,
        rules::ids::REDSTONE_ORE,
    ];
    let mut previous_private = original.source;
    for (i, id) in opaque_ids.into_iter().enumerate() {
        assert!(rules::properties(cell(id)).opaque);
        edits
            .apply_committed(TxId(i as u64 + 1), &[foreign(70, cell(id))])
            .unwrap();
        let tile = build(&generator, &edits);
        assert!(tile.source > previous_private);
        previous_private = tile.source;
        assert_none(&tile);
        assert_same_baseline_samples(&tile, &original);
        assert_eq!(
            publish(&mut heads, &tile),
            published_baseline,
            "public version and encoded HTI2 must hide opaque material ID {id}"
        );
    }
    // The final retained row is exactly the public baseline ground material.
    // Retaining a historical row must not itself become a public shape hint.
    let ground = generator.public_terrain_column(47, -40).unwrap().ground;
    edits
        .apply_committed(TxId(opaque_ids.len() as u64 + 1), &[foreign(70, ground)])
        .unwrap();
    let saved = edits.encode_snapshot().unwrap();
    let restored = HeightEditIndex::decode_snapshot(&saved, IndexLimits::default()).unwrap();
    assert_eq!(
        restored.get(foreign(70, ground).position).unwrap().cell,
        ground
    );
    assert!(
        restored
            .capture(HIGH, 4096, 65536)
            .unwrap()
            .column(47, -40)
            .is_some()
    );
    let tile = build(&generator, &restored);
    assert!(tile.source > previous_private);
    assert_eq!(publish(&mut heads, &tile), published_baseline);
}

#[test]
fn retained_foreign_air_above_baseline_reverts_to_the_same_public_image() {
    let generator = Generator::new(TerrainSeed(1));
    let original = baseline(&generator);
    let mut edits = index();
    let mut heads = HeightHeads::new(HeadLimits::default()).unwrap();
    let published_baseline = publish(&mut heads, &original);
    edits
        .apply_committed(TxId(1), &[foreign(10_000, cell(rules::ids::STONE))])
        .unwrap();
    assert_own_profile_column(&build(&generator, &edits));
    // The intermediate Column was not published. The persisted revert remains
    // indexed, but its AIR equals the public baseline AIR at this altitude.
    edits
        .apply_committed(TxId(2), &[foreign(10_000, Cell::AIR)])
        .unwrap();
    assert_eq!(
        edits.get(foreign(10_000, Cell::AIR).position).unwrap().cell,
        Cell::AIR
    );
    let tile = build(&generator, &edits);
    assert_eq!(tile.source, 3);
    assert_none(&tile);
    assert_same_baseline_samples(&tile, &original);
    assert_eq!(publish(&mut heads, &tile), published_baseline);
}

#[test]
fn unknown_foreign_material_fails_conservatively() {
    let generator = Generator::new(TerrainSeed(1));
    let original = baseline(&generator);
    assert!(rules::block(u16::MAX).is_none());
    let mut edits = index();
    edits
        .apply_committed(TxId(1), &[foreign(70, cell(u16::MAX))])
        .unwrap();
    let tile = build(&generator, &edits);
    assert_own_profile_column(&tile);
    assert_same_baseline_samples(&tile, &original);
}

#[test]
fn closed_buried_air_remains_occluded_under_the_current_surface_predicate() {
    let generator = Generator::new(TerrainSeed(1));
    let original = baseline(&generator);
    // All six neighbors are within solid public baseline terrain. This single
    // removal creates a closed voxel cavity, not an externally opened tunnel.
    for [x, y, z] in [
        [47, 68, -40],
        [47, 70, -40],
        [46, 69, -40],
        [48, 69, -40],
        [47, 69, -41],
        [47, 69, -39],
    ] {
        assert!(generator.public_terrain_column(x, z).unwrap().ground_y >= y);
    }
    let mut edits = index();
    edits
        .apply_committed(TxId(1), &[foreign(69, Cell::AIR)])
        .unwrap();
    let tile = build(&generator, &edits);
    assert_eq!(
        tile.source, 2,
        "the conservative input stamp still covers a retained halo row"
    );
    assert_none(&tile);
    assert_same_baseline_samples(&tile, &original);
    let mut heads = HeightHeads::new(HeadLimits::default()).unwrap();
    assert_eq!(publish(&mut heads, &original), publish(&mut heads, &tile));
}

#[test]
fn actual_cold_public_bodies_hide_ore_and_private_entropy_with_boundary_overrides() {
    use gen_authority::{PrivateSeed, ServerGenerator};
    use server::cold_projection::{ColumnKey, ColumnSnapshot, Limits};
    use world::World;
    let public = Generator::new(TerrainSeed(1));
    let original = baseline(&public);
    let mut heads = HeightHeads::new(HeadLimits::default()).unwrap();
    let published_baseline = publish(&mut heads, &original);
    let empty = World::new(rules::properties, 16);
    let public_column = public.generate_column(2, -3).unwrap();
    let sealed_baseline = Cell::from_parts(public_column.block_padded(16, 69, 9), 0);
    let mut reference: Option<Box<server::cold_projection::BuiltColumn>> = None;
    for (variant, (id, entropy)) in [
        (rules::ids::STONE, 37),
        (rules::ids::IRON_ORE, 37),
        (rules::ids::OSMIUM_ORE, 219),
    ]
    .into_iter()
    .enumerate()
    {
        let mut edits = index();
        // Retained closed AIR is sealed; top opaque private ores redact to the
        // same public host stone. Neither constitutes a foreign public opening.
        edits
            .apply_committed(
                TxId(variant as u64 + 1),
                &[foreign(69, Cell::AIR), foreign(70, cell(id))],
            )
            .unwrap();
        let tile = build(&public, &edits);
        assert_none(&tile);
        assert_eq!(publish(&mut heads, &tile), published_baseline);
        let authority = ServerGenerator::new(TerrainSeed(1), PrivateSeed::new([entropy; 32]));
        let built =
            ColumnSnapshot::capture(&empty, &edits, ColumnKey { x: 2, z: -3 }, Limits::default())
                .unwrap()
                .execute(&authority, || false)
                .unwrap();
        assert!(built.certificate.matches(&empty, &edits));
        let closed = core_types::LocalIdx::new(15 + 16 * (8 + 16 * 5)).unwrap();
        assert_eq!(
            built.pages[4].sealed_cells.as_ref().unwrap()[usize::from(closed.get())],
            sealed_baseline
        );
        let top = core_types::LocalIdx::new(15 + 16 * (8 + 16 * 6)).unwrap();
        assert_eq!(
            built.pages[4].sealed_cells.as_ref().unwrap()[usize::from(top.get())],
            cell(rules::ids::STONE)
        );
        if let Some(expected) = &reference {
            for (a, b) in expected.pages.iter().zip(&built.pages) {
                assert_eq!(a.page, b.page);
                assert_eq!(
                    a.geometry, b.geometry,
                    "private entropy/ore affected public geometry"
                );
                assert_eq!(
                    a.sealed_cells, b.sealed_cells,
                    "private entropy/ore affected public sealed cells"
                );
                assert_eq!(a.water, b.water);
            }
        } else {
            reference = Some(built);
        }
    }
}
