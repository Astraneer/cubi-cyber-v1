//! Fixed private canonical AIR/lava pocket used by encoded privacy tests.
use core_types::{BlockPos, Cell, PageId, SpaceId, Tick};
use gen_authority::{PrivateSeed, ServerGenerator};
use std::collections::{BTreeSet, VecDeque};
use world::{Transaction, World};
pub fn pos(x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x,
        y,
        z,
    }
}
pub fn page(y: i32) -> PageId {
    PageId {
        space: SpaceId(0),
        x: 1,
        y,
        z: 0,
    }
}
pub fn worlds() -> (World, World, Vec<BlockPos>) {
    let generator = ServerGenerator::new(r#gen::TerrainSeed(1), PrivateSeed::new([37; 32]));
    let terrain = r#gen::Generator::new(r#gen::TerrainSeed(1));
    let private = generator.generate_column(1, 0).unwrap();
    let public = terrain.generate_column(1, 0).unwrap();
    let read = |p: BlockPos| {
        assert!(
            p.x >= 16 && p.x < 32 && p.z >= 0 && p.z < 16 && (0..128).contains(&p.y),
            "component must be wholly enclosed inside the source column"
        );
        Cell::from_parts(private.block_padded(p.x - 15, p.y, p.z + 1), 0)
    };
    let mut queue = VecDeque::from([pos(21, 35, 3)]);
    let mut seen = BTreeSet::from([pos(21, 35, 3)]);
    let mut component = Vec::new();
    while let Some(p) = queue.pop_front() {
        assert!(!rules::properties(read(p)).opaque);
        component.push(p);
        for (dx, dy, dz) in [
            (-1, 0, 0),
            (1, 0, 0),
            (0, -1, 0),
            (0, 1, 0),
            (0, 0, -1),
            (0, 0, 1),
        ] {
            let q = pos(p.x + dx, p.y + dy, p.z + dz);
            if !rules::properties(read(q)).opaque && seen.insert(q) {
                queue.push_back(q)
            }
        }
    }
    assert_eq!(component.len(), 201);
    assert!(component.iter().any(|&p| read(p).id() == rules::ids::LAVA));
    assert!(component.iter().any(|&p| read(p) == Cell::AIR));
    let mut actual = World::new(rules::properties, 128);
    let mut baseline = World::new(rules::properties, 128);
    let mut a = Transaction::new(Tick(0), SpaceId(0));
    let mut b = Transaction::new(Tick(0), SpaceId(0));
    for y in 0..8 {
        let cells = private.page(page(y), None).unwrap().cells;
        let mut sealed = cells.clone();
        for p in component.iter().filter(|p| p.page() == page(y)) {
            let value = Cell::from_parts(public.block_padded(p.x - 15, p.y, p.z + 1), 0);
            assert!(rules::properties(value).opaque);
            sealed[usize::from(p.local_index().get())] = value;
        }
        let cells: Box<[Cell]> = cells;
        let sealed: Box<[Cell]> = sealed;
        a.create_page(page(y), cells);
        b.create_page(page(y), sealed);
    }
    actual.commit(a).unwrap();
    baseline.commit(b).unwrap();
    (actual, baseline, component)
}
