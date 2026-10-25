use core_types::{Cell, LocalIdx, PageId, SpaceId, Tick};
use server::lod::{PublicSurfaceKind, geometry_from_page};
use world::{Transaction, World};
fn world(cells: Vec<Cell>) -> World {
    let mut world = World::new(rules::properties, 8);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.create_page(
        PageId {
            space: SpaceId(0),
            x: 0,
            y: 0,
            z: 0,
        },
        cells.into_boxed_slice(),
    );
    world.commit(tx).unwrap();
    world
}
fn data(world: &World) -> server::lod::PublicGeometryData {
    geometry_from_page(world.pages().next().unwrap().1, None)
}
#[test]
fn all_seven_ores_have_byte_identical_far_geometry_to_host_stone() {
    let stone = Cell::from_parts(rules::ids::STONE, 0);
    let baseline = world(vec![stone; 4096]);
    let mut cells = vec![stone; 4096];
    for (index, id) in [
        rules::ids::COAL_ORE,
        rules::ids::IRON_ORE,
        rules::ids::OSMIUM_ORE,
        rules::ids::TIN_ORE,
        rules::ids::COPPER_ORE,
        rules::ids::GOLD_ORE,
        rules::ids::REDSTONE_ORE,
    ]
    .into_iter()
    .enumerate()
    {
        cells[index] = Cell::from_parts(id, 65535);
    }
    let ore_world = world(cells);
    assert_eq!(data(&ore_world), data(&baseline));
    assert_eq!(data(&ore_world).palette.len(), 1);
}
#[test]
fn unknown_ids_keep_distinct_mask_and_never_alias_air() {
    let mut cells = vec![Cell::AIR; 4096];
    cells[0] = Cell::from_raw(u32::MAX);
    cells[1] = Cell::from_parts(rules::ids::STONE, 0);
    let world = world(cells);
    let data = data(&world);
    assert_eq!(data.known[0] & 1, 0);
    assert_ne!(data.known[0] & 2, 0);
    assert_eq!(data.occupied[0] & 1, 0);
    assert_ne!(data.occupied[0] & 2, 0);
    assert_eq!(data.occupied[0] & 4, 0);
    assert_eq!(data.scanned_cells, 4096);
    assert_eq!(data.palette[0].kind, PublicSurfaceKind::Opaque);
}
#[test]
fn packed_palette_indices_cover_every_cell_without_raw_block_states() {
    let ids = [
        rules::ids::STONE,
        rules::ids::DIRT,
        rules::ids::SAND,
        rules::ids::GLASS,
        rules::ids::WATER,
    ];
    let cells = (0..4096)
        .map(|index| Cell::from_parts(ids[index % ids.len()], 0))
        .collect();
    let world = world(cells);
    let data = data(&world);
    assert!(data.palette.len() >= 3);
    assert!(data.index_bits >= 2);
    for index in 0..4096 {
        let at = LocalIdx::new(index).unwrap();
        let cell = world.pages().next().unwrap().1.cell(at);
        let entry = data.palette[usize::from(data.palette_index(at))];
        assert_eq!(
            entry.atlas_layer,
            rules::blocks::layer_cell(cell.id(), cell.state(), rules::blocks::mesh_face(1)).0
                as u16
        );
    }
    assert!(data.wire_payload_bytes() < 40000);
}
