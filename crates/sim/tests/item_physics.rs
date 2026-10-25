use core_types::SpaceId;
use rules::{
    entities::{EntityError, EntityId, item::ItemEntity},
    ids::*,
    stack::ItemStack,
};
use sim::{
    Aabb,
    collision::{Bufs, Solids},
    entities::item,
};
use std::hint::black_box;
use telemetry::{AllocationCounts, AllocationScope, CountingAllocator};
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator::new(std::alloc::System);
struct Floor;
impl Solids for Floor {
    fn shapes_at(&mut self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
        if y == 0 {
            out.push(Aabb {
                lo: [f64::from(x), 0.0, f64::from(z)],
                hi: [f64::from(x) + 1.0, 1.0, f64::from(z) + 1.0],
            });
        }
    }
    fn slip(&mut self, _: i32, _: i32, _: i32) -> f32 {
        0.6
    }
    fn chokes(&mut self, _: i32, _: i32, _: i32) -> bool {
        false
    }
}
fn value(position: [f64; 3], velocity: [f64; 3]) -> ItemEntity {
    let mut item = ItemEntity::new(
        EntityId(0),
        SpaceId(0),
        ItemStack::new(DIRT, 1).unwrap(),
        position,
        1,
        0,
    )
    .unwrap();
    item.velocity = velocity;
    item
}
#[test]
fn donor_block_drop_11_positions_peak_tick_four_landing11_no_bounce() {
    let want = [
        0.535,
        0.6518,
        0.726264,
        0.75923872,
        0.751553946,
        0.704022867,
        0.617442409,
        0.492593561,
        0.330241690,
        0.131136856,
        0.0,
    ];
    let mut item = value([0.5, 1.375, 0.5], [0.0, 0.2, 0.0]);
    let mut bufs = Bufs::default();
    for (tick, height) in want.into_iter().enumerate() {
        item = item::tick_solids(item, &mut Floor, 1, &mut bufs)
            .after
            .unwrap();
        assert!(
            (item.position[1] - 1.0 - height).abs() < 1e-8,
            "tick {tick}: {} vs {height}",
            item.position[1] - 1.0
        );
        assert_eq!(item.on_ground, tick == 10);
    }
    assert!(item.velocity[1].abs() < 1e-12);
}
#[test]
fn donor_q_throw_float_rounding_and_stationary_moves_every_four_ticks() {
    let mut item = value(
        [0.5, 2.32, 0.5],
        [0.0, f64::from(0.1f32), f64::from(0.3f32)],
    );
    let mut bufs = Bufs::default();
    let mut positions = Vec::new();
    for _ in 0..24 {
        item = item::tick_solids(item, &mut Floor, 1, &mut bufs)
            .after
            .unwrap();
        positions.push((item.position[1] - 1.0, item.position[2] - 0.5));
    }
    let near = |a: f64, b: f64| (a - b).abs() < 1e-8;
    assert!(near(positions[0].0, 1.380000001) && near(positions[0].1, 0.300000012));
    assert!(near(positions[4].0, 1.216157937) && near(positions[4].1, 1.441188160));
    assert!(near(positions[10].0, 0.0) && near(positions[10].1, 2.989030137));
    assert!(near(positions[19].1, 3.335924990) && near(positions[22].1, 3.335924990));
    assert!(near(positions[23].1, 3.337136022));
}
#[test]
fn pickup_delays_exact10_and40_active_ticks_and_despawn6000() {
    let mut bufs = Bufs::default();
    for delay in [10, 40] {
        let mut item = value([0.5, 1.0, 0.5], [0.0; 3]);
        item.pickup_delay = delay;
        item.on_ground = true;
        for tick in 1..=delay {
            item = item::tick_solids(item, &mut Floor, 1, &mut bufs)
                .after
                .unwrap();
            assert_eq!(item.pickup_delay, delay - tick);
        }
        assert_eq!(item.pickup_delay, 0);
    }
    let mut item = value([0.5, 1.0, 0.5], [0.0; 3]);
    item.on_ground = true;
    item.item_age = 5998;
    item = item::tick_solids(item, &mut Floor, 1, &mut bufs)
        .after
        .unwrap();
    assert_eq!(item.item_age, 5999);
    assert!(
        item::tick_solids(item, &mut Floor, 1, &mut bufs)
            .after
            .is_none()
    );
}
#[test]
fn unknown_preserves_entire_item_and_warmed_tick_allocates_nothing() {
    let original = value([0.5, 1.0, 0.5], [0.0; 3]);
    let mut bufs = Bufs::default();
    assert_eq!(
        item::plan_item(&original, &|_| world::CellRead::Unknown, 1, &mut bufs),
        Err(EntityError::UnknownInput)
    );
    let mut item = item::tick_solids(original, &mut Floor, 1, &mut bufs)
        .after
        .unwrap();
    let positive = AllocationScope::begin().unwrap();
    let allocation = Vec::<u8>::with_capacity(black_box(13));
    black_box(&allocation);
    assert!(positive.finish().allocating_calls() > 0);
    drop(allocation);
    let scope = AllocationScope::begin().unwrap();
    for _ in 0..1000 {
        item = item::tick_solids(item, &mut Floor, 1, &mut bufs)
            .after
            .unwrap();
        black_box(&item);
    }
    assert_eq!(scope.finish(), AllocationCounts::ZERO);
}
