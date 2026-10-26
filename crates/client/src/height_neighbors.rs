//! Dyadic height-grid neighbours shared by worker selection and owner validation.
use render::height_tile::{HeightTileKey, MAX_LEVEL};

/// Candidate across West, East, North or South (edge0..3), at the same or a
/// coarser level. In a disjoint cover, visiting every fine cell finds all edge
/// pairs without descending into finer neighbours. Reject ancestor membership
/// separately when validating that a proposed cover contains no overlaps.
pub fn ancestor_neighbour(key: HeightTileKey, edge: usize, level: u8) -> Option<HeightTileKey> {
    if key.level > MAX_LEVEL || level > MAX_LEVEL || level < key.level {
        return None;
    }
    let span = 1_i64 << (key.level + 1);
    let x = i64::from(key.x) * span;
    let z = i64::from(key.z) * span;
    // Doubled page coordinates retain a leaf midpoint without fractions.
    let [px, pz] = match edge {
        0 => [x - 1, z + span / 2],
        1 => [x + span, z + span / 2],
        2 => [x + span / 2, z - 1],
        3 => [x + span / 2, z + span],
        _ => return None,
    };
    let parent_span = 1_i64 << (level + 1);
    let candidate = HeightTileKey {
        space: key.space,
        level,
        x: i32::try_from(px.div_euclid(parent_span)).ok()?,
        z: i32::try_from(pz.div_euclid(parent_span)).ok()?,
    };
    candidate.bounds()?;
    Some(candidate)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_edge_candidates_cover_both_sides_of_negative_dyadic_boundaries() {
        for level in 0..=MAX_LEVEL {
            for x in -9..=9 {
                for z in -9..=9 {
                    let key = HeightTileKey {
                        space: 3,
                        level,
                        x,
                        z,
                    };
                    for (edge, [dx, dz]) in
                        [[-1, 0], [1, 0], [0, -1], [0, 1]].into_iter().enumerate()
                    {
                        assert_eq!(
                            ancestor_neighbour(key, edge, level),
                            Some(HeightTileKey {
                                x: x + dx,
                                z: z + dz,
                                ..key
                            })
                        );
                    }
                }
            }
        }
        let key = HeightTileKey {
            space: 0,
            level: 0,
            x: -1,
            z: -1,
        };
        assert_eq!(ancestor_neighbour(key, 0, 2).unwrap().x, -1);
        assert_eq!(ancestor_neighbour(key, 1, 2).unwrap().x, 0);
        assert!(ancestor_neighbour(key, 4, 0).is_none());
        assert!(ancestor_neighbour(key, 0, MAX_LEVEL + 1).is_none());
    }
}
