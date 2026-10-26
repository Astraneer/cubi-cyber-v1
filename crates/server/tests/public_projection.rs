use core_types::Cell;
use server::public_projection::{Error, HEIGHT, PublicProjection, VOLUME, WIDTH};
fn stone() -> Cell {
    Cell::from_parts(rules::ids::STONE, 0)
}
fn baseline(p: [i32; 3]) -> Cell {
    if p[1] < 64 { stone() } else { Cell::AIR }
}
fn pocket(p: [i32; 3]) -> bool {
    (23..=25).contains(&p[0]) && (34..=36).contains(&p[1]) && (23..=25).contains(&p[2])
}
fn authority(p: [i32; 3], portal: bool, unknown: bool) -> Option<Cell> {
    if unknown && p == [24, 50, 24] {
        return None;
    }
    if p == [24, 34, 24] {
        return Some(Cell::from_parts(rules::ids::LAVA, 0));
    }
    if pocket(p) || (portal && p[0] == 24 && p[2] == 24 && (35..=64).contains(&p[1])) {
        return Some(Cell::AIR);
    }
    Some(baseline(p))
}
#[test]
fn closed_private_air_and_lava_match_public_baseline_until_actual_known_portal_opens() {
    let closed =
        PublicProjection::build([0, 0], |p| authority(p, false, false), baseline, || false)
            .unwrap();
    for y in 34..=36 {
        for z in 23..=25 {
            for x in 23..=25 {
                let p = [x, y, z];
                assert_eq!(
                    closed.project(p, authority(p, false, false).unwrap(), baseline(p)),
                    stone()
                );
            }
        }
    }
    assert_eq!(closed.work().closed_transparent_cells, 27);
    let incomplete =
        PublicProjection::build([0, 0], |p| authority(p, true, true), baseline, || false).unwrap();
    assert_eq!(
        incomplete.project([24, 35, 24], Cell::AIR, stone()),
        stone()
    );
    let open =
        PublicProjection::build([0, 0], |p| authority(p, true, false), baseline, || false).unwrap();
    assert_eq!(open.project([24, 35, 24], Cell::AIR, stone()), Cell::AIR);
    assert_eq!(
        open.project([24, 34, 24], Cell::from_parts(rules::ids::LAVA, 0), stone())
            .id(),
        rules::ids::LAVA
    );
    assert_eq!(open.work().closed_transparent_cells, 0);
    for work in [closed.work(), incomplete.work(), open.work()] {
        assert_eq!(work.authority_reads, VOLUME as u64);
        assert!(work.baseline_reads <= VOLUME as u64);
        assert!(work.flood_cells <= VOLUME as u64);
        assert_eq!(work.mask_bytes, VOLUME as u64);
        assert_eq!(work.peak_queue_bytes, (VOLUME * 4) as u64);
    }
}
#[test]
fn public_open_arch_and_actual_solid_edits_survive_while_ores_stay_redacted() {
    let actual = |p: [i32; 3]| {
        // Two columns and a lintel above the public ground. The opening and a
        // roofed public-air room must not be silently filled with invented rock.
        if (p[0] == 20 || p[0] == 24 || p[1] == 75)
            && (20..=24).contains(&p[0])
            && (70..=75).contains(&p[1])
            && p[2] == 24
        {
            Some(stone())
        } else {
            Some(baseline(p))
        }
    };
    let projection = PublicProjection::build([0, 0], actual, baseline, || false).unwrap();
    for y in 70..75 {
        for x in 21..24 {
            assert_eq!(
                projection.project([x, y, 24], Cell::AIR, Cell::AIR),
                Cell::AIR
            );
        }
    }
    assert_eq!(
        projection.project([20, 72, 24], stone(), Cell::AIR),
        stone()
    );
    assert_eq!(
        projection.project(
            [23, 72, 24],
            Cell::from_parts(rules::ids::IRON_ORE, 0),
            Cell::AIR
        ),
        stone()
    );
    assert_eq!(
        projection.project([0, HEIGHT as i32, 0], Cell::AIR, Cell::AIR),
        Cell::AIR
    );
    assert_eq!(
        projection.work().authority_reads,
        (WIDTH * WIDTH * HEIGHT) as u64
    );
}
#[test]
fn bounded_cancellation_and_coordinate_rejection() {
    assert!(matches!(
        PublicProjection::build([i32::MAX, 0], |_| Some(Cell::AIR), |_| Cell::AIR, || false),
        Err(Error::Bounds)
    ));
    assert!(matches!(
        PublicProjection::build([0, 0], |_| Some(Cell::AIR), |_| Cell::AIR, || true),
        Err(Error::Cancelled)
    ));
}
