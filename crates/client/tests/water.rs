#[path = "../src/water.rs"]
#[allow(dead_code)]
mod water;
use core_types::{BlockPos, Cell, PageId, SpaceId, Tick};
use water::*;
use world::{Transaction, World};
#[test]
fn known_flat_source_and_flowing_planes_emit16byte_levels_without_internal_faces() {
    for (amount, level) in [(8, 227u32), (1, 28u32)] {
        let halo = WaterHalo::from_fn(|_, y, _| {
            if y == 0 {
                SurfaceCell::Water {
                    amount,
                    falling: false,
                }
            } else if y < 0 {
                SurfaceCell::Solid
            } else {
                SurfaceCell::Air
            }
        });
        let mesh = mesh_water(&halo, &[u32::MAX; 256], 1024, || false).unwrap();
        assert_eq!(mesh.quads.len(), 256);
        assert_eq!(mesh.work.cells_scanned, 4096);
        assert_eq!(mesh.work.halo_samples, 5832);
        assert_eq!(mesh.work.cpu_frame_vertices, 0);
        for q in mesh.quads {
            assert_eq!((q.words[0] >> 15) & 7, 1);
            assert_eq!(q.words[1], level * 0x01010101);
            assert_eq!(q.words[3] >> 16, 0);
        }
    }
}
#[test]
fn unknown_boundary_is_withheld_instead_of_becoming_air() {
    let unknown = WaterHalo::from_fn(|x, y, z| {
        if x == 16 {
            SurfaceCell::Unknown
        } else if y == 0 {
            SurfaceCell::Water {
                amount: 8,
                falling: false,
            }
        } else if y < 0 {
            SurfaceCell::Solid
        } else {
            let _ = z;
            SurfaceCell::Air
        }
    });
    let mesh = mesh_water(&unknown, &[u32::MAX; 256], 1024, || false).unwrap();
    assert_eq!(mesh.quads.len(), 240);
    assert_eq!(mesh.work.unknown_surface_cells, 16);
    assert!(mesh_water(&unknown, &[u32::MAX; 256], 8, || false).is_err());
    assert!(mesh_water(&unknown, &[u32::MAX; 256], 1024, || true).is_err());
}
#[test]
fn shared_page_corners_are_identical_and_state_levels_change_only_descriptors() {
    let make = |offset: i32| {
        WaterHalo::from_fn(|x, y, z| {
            if y == 0 {
                SurfaceCell::Water {
                    amount: if (i32::from(x) + offset + i32::from(z)).rem_euclid(5) == 0 {
                        3
                    } else {
                        8
                    },
                    falling: false,
                }
            } else if y < 0 {
                SurfaceCell::Solid
            } else {
                SurfaceCell::Air
            }
        })
    };
    let a = mesh_water(&make(0), &[u32::MAX; 256], 1024, || false).unwrap();
    let b = mesh_water(&make(16), &[u32::MAX; 256], 1024, || false).unwrap();
    for z in 0..16usize {
        let left = &a.quads[z * 16 + 15];
        let right = &b.quads[z * 16];
        assert_eq!((left.words[1] >> 8) & 255, right.words[1] & 255);
        assert_eq!((left.words[1] >> 24) & 255, (right.words[1] >> 16) & 255);
    }
}
#[test]
fn actual_world_snapshot_guard_uses_fluid_and_exact_halo_regions() {
    let mut world = World::new(rules::properties, 128);
    let page = PageId {
        space: SpaceId(0),
        x: 0,
        y: 0,
        z: 0,
    };
    let neighbor = PageId { x: 1, ..page };
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    tx.create_page(page, vec![Cell::AIR; 4096]);
    tx.create_page(neighbor, vec![Cell::AIR; 4096]);
    tx.set_cell(
        BlockPos {
            space: SpaceId(0),
            x: 8,
            y: 0,
            z: 8,
        },
        Cell::from_parts(rules::ids::WATER, 0),
    );
    world.commit(tx).unwrap();
    let task = WaterTask::capture(&world, page, Box::new([u32::MAX; 256])).unwrap();
    let result = task.execute(1024, || false).unwrap();
    assert!(result.current(&world));
    assert!(!result.mesh.quads.is_empty());
    let mut tx = Transaction::new(Tick(2), SpaceId(0));
    tx.set_cell(
        BlockPos {
            space: SpaceId(0),
            x: 24,
            y: 8,
            z: 8,
        },
        Cell::from_parts(rules::ids::STONE, 0),
    );
    world.commit(tx).unwrap();
    assert!(
        result.current(&world),
        "unread neighbor interior invalidated water snapshot"
    );
    let mut tx = Transaction::new(Tick(3), SpaceId(0));
    tx.set_cell(
        BlockPos {
            space: SpaceId(0),
            x: 16,
            y: 8,
            z: 8,
        },
        Cell::from_parts(rules::ids::STONE, 0),
    );
    world.commit(tx).unwrap();
    assert!(!result.current(&world));
}
