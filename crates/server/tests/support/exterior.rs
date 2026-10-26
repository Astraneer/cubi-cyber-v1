//! Independent union-find reference for the worker's six-neighbour exposure.
use core_types::{Cell, LocalIdx, PageId, SpaceId, Tick};
use world::{CellRead, Transaction, World};
const W: usize = 48;
const H: usize = 128;
const N: usize = W * W * H;
fn root(parent: &mut [usize], mut i: usize) -> usize {
    while parent[i] != i {
        parent[i] = parent[parent[i]];
        i = parent[i];
    }
    i
}
fn ore(c: Cell) -> Cell {
    use rules::ids::*;
    if matches!(
        c.id(),
        COAL_ORE | IRON_ORE | OSMIUM_ORE | TIN_ORE | COPPER_ORE | GOLD_ORE | REDSTONE_ORE
    ) {
        Cell::from_parts(STONE, 0)
    } else {
        c
    }
}
pub fn sealed_world(actual: &World, terrain: &r#gen::Generator, centre: PageId) -> World {
    let origin = [centre.x * 16 - 16, centre.z * 16 - 16];
    let public: Vec<_> = (0..9)
        .map(|i| {
            terrain
                .generate_column(centre.x + i % 3 - 1, centre.z + i / 3 - 1)
                .unwrap()
        })
        .collect();
    let mut cells = vec![None; N];
    let mut base = vec![Cell::AIR; N];
    let mut parent: Vec<_> = (0..N).collect();
    let mut exposed = vec![false; N];
    for y in 0..H {
        for z in 0..W {
            for x in 0..W {
                let i = x + W * (z + W * y);
                let p = core_types::BlockPos {
                    space: SpaceId(0),
                    x: origin[0] + x as i32,
                    y: y as i32,
                    z: origin[1] + z as i32,
                };
                cells[i] = match actual.read(p) {
                    CellRead::Known(c) => Some(c),
                    CellRead::Unknown => None,
                };
                base[i] = Cell::from_parts(
                    public[x / 16 + 3 * (z / 16)].block_padded(
                        (x % 16 + 1) as i32,
                        y as i32,
                        (z % 16 + 1) as i32,
                    ),
                    0,
                );
                exposed[i] = cells[i].is_some_and(|c| !rules::properties(c).opaque)
                    && !rules::properties(base[i]).opaque;
            }
        }
    }
    for y in 0..H {
        for z in 0..W {
            for x in 0..W {
                let i = x + W * (z + W * y);
                if cells[i].is_none_or(|c| rules::properties(c).opaque) {
                    continue;
                }
                for j in [
                    (x + 1 < W).then_some(i + 1),
                    (z + 1 < W).then_some(i + W),
                    (y + 1 < H).then_some(i + W * W),
                ]
                .into_iter()
                .flatten()
                {
                    if cells[j].is_some_and(|c| !rules::properties(c).opaque) {
                        let a = root(&mut parent, i);
                        let b = root(&mut parent, j);
                        if a != b {
                            parent[b] = a;
                            exposed[a] |= exposed[b];
                        }
                    }
                }
            }
        }
    }
    let mut out = World::new(rules::properties, 256);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    for z in 0..3 {
        for x in 0..3 {
            for y in 0..8 {
                let page = PageId {
                    space: SpaceId(0),
                    x: centre.x + x - 1,
                    y,
                    z: centre.z + z - 1,
                };
                if actual.page(page).is_none() {
                    continue;
                }
                let mut values = vec![Cell::AIR; 4096];
                for (local, c) in values.iter_mut().enumerate() {
                    let (lx, ly, lz) = LocalIdx::new(local as u16).unwrap().xyz();
                    let i = (x as usize * 16 + usize::from(lx))
                        + W * ((z as usize * 16 + usize::from(lz))
                            + W * (y as usize * 16 + usize::from(ly)));
                    let value = cells[i].unwrap();
                    let group = root(&mut parent, i);
                    *c = ore(if !rules::properties(value).opaque && !exposed[group] {
                        base[i]
                    } else {
                        value
                    });
                }
                tx.create_page(page, values);
            }
        }
    }
    out.commit(tx).unwrap();
    out
}
