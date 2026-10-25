//! Per-observer wire views. Private ores are never sent before a concrete,
//! bounded line-of-sight query reaches them; Unknown closes the query.
use core_types::{Cell, PAGE_VOLUME, PageId, SpaceId, Version};
use net::FarSummary;
use sim::{Player, RayResult, WorldQuery};
use world::World;

#[derive(Default)]
pub(crate) struct ViewWork {
    pub cells: u64,
    pub segments: u64,
}

fn private_material(id: u16) -> bool {
    use rules::ids::*;
    matches!(
        id,
        COAL_ORE | IRON_ORE | OSMIUM_ORE | TIN_ORE | COPPER_ORE | GOLD_ORE | REDSTONE_ORE
    )
}
pub(crate) fn sanitize(
    world: &World,
    page: PageId,
    mut cells: Box<[Cell; PAGE_VOLUME]>,
    observer: &Player,
    space: SpaceId,
) -> (Box<[Cell; PAGE_VOLUME]>, ViewWork) {
    let eye = observer.eye(1.0);
    let mut work = ViewWork {
        cells: PAGE_VOLUME as u64,
        segments: 0,
    };
    for (index, cell) in cells.iter_mut().enumerate() {
        if !private_material(cell.id()) {
            continue;
        }
        let target = [
            page.x * 16 + (index % 16) as i32,
            page.y * 16 + (index / 256) as i32,
            page.z * 16 + (index / 16 % 16) as i32,
        ];
        let offset =
            std::array::from_fn::<_, 3, _>(|axis| f64::from(target[axis]) + 0.5 - eye[axis]);
        let distance = offset.iter().map(|v| v * v).sum::<f64>().sqrt();
        let visible = page.space == space
            && distance <= 64.0
            && distance > 0.0
            && visible_ray(
                &WorldQuery::new(world, space),
                eye,
                offset.map(|v| v / distance),
                distance + 0.01,
                target,
                &mut work,
            );
        if !visible {
            *cell = Cell::from_parts(rules::ids::STONE, 0);
        }
    }
    (cells, work)
}
fn visible_ray(
    query: &WorldQuery<'_>,
    eye: [f64; 3],
    direction: [f64; 3],
    reach: f64,
    target: [i32; 3],
    work: &mut ViewWork,
) -> bool {
    // The interaction kernel intentionally limits one ray to16 blocks. Chaining
    // at mostfive consecutive, nonoverlapping segments preserves its shape and
    // Unknown behavior while covering the network visibility distance.
    let mut traveled = 0.0;
    for _ in 0..5 {
        let segment = (reach - traveled).min(16.0);
        if segment <= 0.0 {
            return false;
        }
        let start = std::array::from_fn(|axis| eye[axis] + direction[axis] * traveled);
        work.segments += 1;
        match sim::raycast(query, start, direction, segment) {
            RayResult::Hit(hit) => return hit.cell == target,
            RayResult::Unknown(_) => return false,
            RayResult::Miss => traveled += segment,
        }
    }
    false
}
pub(crate) fn summary(
    page: PageId,
    source_version: Version,
    cells: &[Cell; PAGE_VOLUME],
) -> FarSummary {
    let mut occupied = 0u64;
    let mut materials = [0; 64];
    for sy in 0..4 {
        for sz in 0..4 {
            for sx in 0..4 {
                let slot = sx + sz * 4 + sy * 16;
                let mut counts = std::collections::BTreeMap::<u16, u16>::new();
                for y in sy * 4..sy * 4 + 4 {
                    for z in sz * 4..sz * 4 + 4 {
                        for x in sx * 4..sx * 4 + 4 {
                            let id = cells[x + z * 16 + y * 256].id();
                            if id != rules::ids::AIR {
                                *counts.entry(id).or_default() += 1;
                            }
                        }
                    }
                }
                if let Some((&id, _)) = counts
                    .iter()
                    .max_by_key(|(id, count)| (**count, std::cmp::Reverse(**id)))
                {
                    occupied |= 1 << slot;
                    materials[slot] = id;
                }
            }
        }
    }
    FarSummary {
        page,
        source_version,
        occupied,
        materials,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_types::{BlockPos, Tick};
    use world::Transaction;

    #[test]
    fn exposure_beyond_one_interaction_segment_still_requires_every_known_segment() {
        let mut world = World::new(rules::properties, 32);
        let mut tx = Transaction::new(Tick(1), SpaceId(0));
        let target = BlockPos {
            space: SpaceId(0),
            x: 40,
            y: 2,
            z: 8,
        };
        for x in 0..3 {
            let page = PageId {
                space: SpaceId(0),
                x,
                y: 0,
                z: 0,
            };
            let mut cells = vec![Cell::AIR; PAGE_VOLUME];
            if page == target.page() {
                cells[usize::from(target.local_index().get())] =
                    Cell::from_parts(rules::ids::IRON_ORE, 0);
            }
            tx.create_page(page, cells);
        }
        world.commit(tx).unwrap();
        let player = Player::new([8.5, 1.0, 8.5]);
        let data = world.page(target.page()).unwrap();
        let (cells, work) = sanitize(&world, target.page(), data.cells(), &player, SpaceId(0));
        assert_eq!(
            cells[usize::from(target.local_index().get())].id(),
            rules::ids::IRON_ORE
        );
        assert!(work.segments >= 2);
        let mut tx = Transaction::new(Tick(2), SpaceId(0));
        tx.remove_page(PageId {
            space: SpaceId(0),
            x: 1,
            y: 0,
            z: 0,
        });
        world.commit(tx).unwrap();
        let data = world.page(target.page()).unwrap();
        let (cells, _) = sanitize(&world, target.page(), data.cells(), &player, SpaceId(0));
        assert_eq!(
            cells[usize::from(target.local_index().get())].id(),
            rules::ids::STONE
        );
    }
}
