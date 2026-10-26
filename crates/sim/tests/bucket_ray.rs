use core_types::Cell;
use world::CellRead;
#[test]
fn bucket_ray_skips_flowing_but_never_unknown_or_opaque_occlusion() {
    let source = Cell::from_parts(rules::ids::WATER, 0);
    let stone = Cell::from_parts(rules::ids::STONE, 0);
    let flowing = Cell::from_parts(rules::ids::WATER, 3);
    let query = |p: [i32; 3]| {
        CellRead::Known(match p {
            [0, 0, 1] => flowing,
            [0, 0, 2] => source,
            [0, 0, 3] => stone,
            _ => Cell::AIR,
        })
    };
    let sim::RayResult::Hit(hit) = sim::raycast_fluid_sources(&query, [0.5; 3], [0., 0., 1.], 4.5)
    else {
        panic!()
    };
    assert_eq!(hit.cell, [0, 0, 2]);
    assert_eq!(hit.block, source);
    assert_eq!(hit.face, 2);
    assert!((hit.distance - 1.5).abs() < 1e-10);
    let sim::RayResult::Hit(hit) = sim::raycast(&query, [0.5; 3], [0., 0., 1.], 4.5) else {
        panic!()
    };
    assert_eq!(hit.cell, [0, 0, 3]);
    let unknown = |p| {
        if p == [0, 0, 1] {
            CellRead::Unknown
        } else {
            query(p)
        }
    };
    assert_eq!(
        sim::raycast_fluid_sources(&unknown, [0.5; 3], [0., 0., 1.], 4.5),
        sim::RayResult::Unknown([0, 0, 1])
    );
    let blocked = |p| {
        if p == [0, 0, 1] {
            CellRead::Known(stone)
        } else {
            query(p)
        }
    };
    let sim::RayResult::Hit(hit) =
        sim::raycast_fluid_sources(&blocked, [0.5; 3], [0., 0., 1.], 4.5)
    else {
        panic!()
    };
    assert_eq!(hit.cell, [0, 0, 1]);
    let sim::RayResult::Hit(hit) =
        sim::raycast_fluid_sources(&query, [0.5, 0.95, 0.5], [0., 0., 1.], 4.5)
    else {
        panic!()
    };
    assert_eq!(
        hit.cell,
        [0, 0, 3],
        "8/9 source must not claim the empty top band"
    );
    let covered = |p| {
        if p == [0, 1, 2] {
            CellRead::Known(flowing)
        } else {
            query(p)
        }
    };
    let sim::RayResult::Hit(hit) =
        sim::raycast_fluid_sources(&covered, [0.5, 0.95, 0.5], [0., 0., 1.], 4.5)
    else {
        panic!()
    };
    assert_eq!(
        hit.cell,
        [0, 0, 2],
        "same liquid above raises source outline to1"
    );
}
