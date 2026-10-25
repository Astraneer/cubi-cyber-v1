use rules::{
    entities::{
        EntityError, EntityId, MobKind,
        mob::{Control, Mob},
    },
    ids::*,
};
use sim::{
    Aabb,
    collision::{Bufs, Solids},
    entities,
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
fn walk(kind: MobKind, multiplier: f32, ticks: usize) -> f64 {
    let mut mob = Mob::new(EntityId(1), kind, [0.5, 1.0, 0.5]).unwrap();
    mob.body.on_ground = true;
    let mut bufs = Bufs::default();
    let mut delta = 0.0;
    for _ in 0..ticks {
        let before = mob.body.position[2];
        entities::tick_solids(
            &mut mob,
            &mut Floor,
            Control {
                destination: Some([0.5, 1.0, 10000.0]),
                speed_multiplier: multiplier,
                ..Control::default()
            },
            &mut bufs,
        );
        delta = mob.body.position[2] - before;
    }
    delta
}
#[test]
fn actual_mob_physics_matches_donor_eight_terminal_speeds() {
    for (kind, multiplier, want) in [
        (MobKind::Pig, 1.0, 0.134912),
        (MobKind::Pig, 1.25, 0.210800),
        (MobKind::Cow, 1.0, 0.086344),
        (MobKind::Cow, 2.0, 0.345375),
        (MobKind::Chicken, 1.4, 0.264427),
        (MobKind::Sheep, 1.0, 0.114189),
        (MobKind::Sheep, 1.25, 0.178421),
        (MobKind::Zombie, 1.0, 0.114189),
    ] {
        assert!(
            (walk(kind, multiplier, 120) - want).abs() < 2e-6,
            "{kind:?} ×{multiplier}"
        );
    }
    assert!(walk(MobKind::Pig, 1.0, 4) > 0.9 * 0.134912);
}
#[test]
fn movement_control_turns_180_in_two_ticks_and_jump_apex_matches() {
    let mut mob = Mob::new(EntityId(1), MobKind::Pig, [0.5, 1.0, 0.5]).unwrap();
    mob.body.on_ground = true;
    let mut bufs = Bufs::default();
    for _ in 0..2 {
        entities::tick_solids(
            &mut mob,
            &mut Floor,
            Control {
                destination: Some([0.5, 1.0, -50.0]),
                speed_multiplier: 1.0,
                ..Control::default()
            },
            &mut bufs,
        );
    }
    assert!((mob.body.yaw - 180.0).abs() < 1e-3);
    let mut mob = Mob::new(EntityId(1), MobKind::Pig, [0.5, 1.0, 0.5]).unwrap();
    mob.body.on_ground = true;
    let mut apex = 0.0f64;
    let mut landed = false;
    for tick in 0..50 {
        let outcome = entities::tick_solids(
            &mut mob,
            &mut Floor,
            Control {
                jump: tick == 0,
                ..Control::default()
            },
            &mut bufs,
        );
        apex = apex.max(mob.body.position[1] - 1.0);
        landed |= outcome.landed.is_some();
    }
    assert!((apex - 1.2522).abs() < 0.0001);
    assert!(landed);
    assert_eq!(mob.body.position[1], 1.0);
}
#[test]
fn chicken_slow_fall_immune_actual_generic_body() {
    let mut chicken = Mob::new(EntityId(1), MobKind::Chicken, [0.5, 20.0, 0.5]).unwrap();
    let mut pig = Mob::new(EntityId(2), MobKind::Pig, [0.5, 20.0, 0.5]).unwrap();
    let mut bufs = Bufs::default();
    for _ in 0..20 {
        entities::tick_solids(&mut chicken, &mut Floor, Control::default(), &mut bufs);
        entities::tick_solids(&mut pig, &mut Floor, Control::default(), &mut bufs);
    }
    assert!(chicken.body.position[1] > pig.body.position[1] + 5.0);
    let mut damage = 0;
    for _ in 0..300 {
        damage += entities::tick_solids(&mut chicken, &mut Floor, Control::default(), &mut bufs)
            .fall_damage;
    }
    assert_eq!(damage, 0);
    assert_eq!(chicken.body.position[1], 1.0);
}
#[test]
fn unknown_defers_entire_body_and_has_no_generation_hook() {
    let mob = Mob::new(EntityId(1), MobKind::Pig, [0.5, 1.0, 0.5]).unwrap();
    let mut bufs = Bufs::default();
    let result = entities::plan_body(
        &mob,
        &|_| world::CellRead::Unknown,
        Control::default(),
        &mut bufs,
    );
    assert_eq!(result, Err(EntityError::UnknownInput));
    assert_eq!(mob.body.position, [0.5, 1.0, 0.5]);
    let query = |p: [i32; 3]| {
        world::CellRead::Known(core_types::Cell::from_parts(
            if p[1] <= 0 { STONE } else { AIR },
            0,
        ))
    };
    assert!(entities::plan_body(&mob, &query, Control::default(), &mut bufs).is_ok());
}
#[test]
fn warmed_mob_body_tick_allocates_nothing_positive_control() {
    let mut mob = Mob::new(EntityId(1), MobKind::Cow, [0.5, 1.0, 0.5]).unwrap();
    mob.body.on_ground = true;
    let mut bufs = Bufs::default();
    let control = Control {
        destination: Some([0.5, 1.0, 10000.0]),
        speed_multiplier: 1.0,
        ..Control::default()
    };
    entities::tick_solids(&mut mob, &mut Floor, control, &mut bufs);
    let positive = AllocationScope::begin().unwrap();
    let allocation = Vec::<u8>::with_capacity(black_box(17));
    black_box(&allocation);
    assert!(positive.finish().allocating_calls() > 0);
    drop(allocation);
    let scope = AllocationScope::begin().unwrap();
    for _ in 0..1000 {
        black_box(entities::tick_solids(
            &mut mob, &mut Floor, control, &mut bufs,
        ));
    }
    assert_eq!(scope.finish(), AllocationCounts::ZERO);
}
