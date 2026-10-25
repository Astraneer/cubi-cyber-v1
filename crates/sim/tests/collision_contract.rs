use rules::shapes::Aabb;
use sim::collision::{
    Bufs, MAX_QUERY_CELLS, Solids, collide, collisions, edge_guard, space_below_empty, space_empty,
    sweep,
};

#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);

fn bounds(low: [f64; 3], high: [f64; 3]) -> Aabb {
    Aabb { lo: low, hi: high }
}
fn body(center: [f64; 3]) -> Aabb {
    bounds(
        [center[0] - 0.3, center[1], center[2] - 0.3],
        [center[0] + 0.3, center[1] + 1.8, center[2] + 0.3],
    )
}
fn floor() -> Aabb {
    bounds([-100.0, -64.0, -100.0], [100.0, 0.0, 100.0])
}
#[derive(Default)]
struct Boxes {
    shapes: Vec<Aabb>,
    queries: usize,
}
impl Boxes {
    fn new(shapes: Vec<Aabb>) -> Self {
        Self { shapes, queries: 0 }
    }
}
impl Solids for Boxes {
    fn shapes_at(&mut self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
        self.queries += 1;
        let cell = bounds(
            [f64::from(x), f64::from(y), f64::from(z)],
            [f64::from(x) + 1.0, f64::from(y) + 1.0, f64::from(z) + 1.0],
        );
        for shape in &self.shapes {
            let clipped = bounds(
                std::array::from_fn(|i| shape.lo[i].max(cell.lo[i])),
                std::array::from_fn(|i| shape.hi[i].min(cell.hi[i])),
            );
            if (0..3).all(|i| clipped.lo[i] < clipped.hi[i]) {
                out.push(clipped);
            }
        }
    }
    fn slip(&mut self, _: i32, _: i32, _: i32) -> f32 {
        0.6
    }
    fn chokes(&mut self, _: i32, _: i32, _: i32) -> bool {
        false
    }
}
fn near(actual: f64, expected: f64) {
    assert!((actual - expected).abs() < 1e-9, "{actual} != {expected}");
}

#[test]
fn larger_horizontal_axis_goes_first_and_tie_uses_x() {
    let obstacle = bounds([0.5, 0.0, 0.5], [1.0, 2.0, 1.0]);
    let bx = body([0.0; 3]);
    let x_first = sweep([0.6, 0.0, 0.4], bx, &[obstacle]);
    near(x_first[0], 0.6);
    near(x_first[2], 0.2);
    let z_first = sweep([0.4, 0.0, 0.6], bx, &[obstacle]);
    near(z_first[0], 0.2);
    near(z_first[2], 0.6);
    let tie = sweep([0.6, 0.0, 0.6], bx, &[obstacle]);
    near(tie[0], 0.6);
    near(tie[2], 0.2);
}

#[test]
fn vertical_sweep_precedes_horizontal_motion() {
    let floor = bounds([-5.0, -1.0, -5.0], [5.0, 0.0, 5.0]);
    let obstacle = bounds([0.5, 0.0, -5.0], [1.5, 0.5, 5.0]);
    let result = sweep([0.6, -0.4, 0.0], body([0.0, 0.3, 0.0]), &[floor, obstacle]);
    near(result[1], -0.3);
    near(result[0], 0.2);
}

#[test]
fn step_boundary_half_block_succeeds_five_eighths_does_not() {
    for (height, expected_x, expected_y) in [(0.5, 0.6, 0.5), (0.625, 0.2, 0.0)] {
        let mut world = Boxes::new(vec![
            floor(),
            bounds([0.5, 0.0, -10.0], [5.0, height, 10.0]),
        ]);
        let result = collide(
            &mut world,
            &body([0.0; 3]),
            [0.6, -0.0784, 0.0],
            true,
            0.6,
            &mut Bufs::default(),
        );
        near(result[0], expected_x);
        near(result[1], expected_y);
    }
}

#[test]
fn step_can_begin_on_the_landing_tick() {
    let mut world = Boxes::new(vec![floor(), bounds([0.5, 0.0, -10.0], [5.0, 0.5, 10.0])]);
    let result = collide(
        &mut world,
        &body([0.0, 0.3, 0.0]),
        [0.6, -0.4, 0.0],
        false,
        0.6,
        &mut Bufs::default(),
    );
    near(result[0], 0.6);
    near(result[1], 0.2);
}

#[test]
fn low_ceiling_prevents_a_step_without_penetration() {
    let mut world = Boxes::new(vec![
        floor(),
        bounds([0.5, 0.0, -10.0], [5.0, 0.5, 10.0]),
        bounds([-5.0, 2.0, -10.0], [5.0, 3.0, 10.0]),
    ]);
    let result = collide(
        &mut world,
        &body([0.0; 3]),
        [0.6, -0.0784, 0.0],
        true,
        0.6,
        &mut Bufs::default(),
    );
    near(result[0], 0.2);
    near(result[1], 0.0);
}

#[test]
fn sneak_keeps_support_at_the_edge_and_preserves_parallel_motion() {
    let mut world = Boxes::new(vec![bounds([-100.0, -1.0, -100.0], [100.0, 0.0, 0.0])]);
    let bx = body([0.0, 0.0, -0.1]);
    let mut buffer = Vec::with_capacity(64);
    let result = edge_guard(&mut world, &bx, [0.5, -0.0784, 0.8], 0.6, &mut buffer);
    near(result[0], 0.5);
    assert!((0.3..=0.4000000001).contains(&result[2]));
    assert!(!space_below_empty(
        &mut world,
        &bx,
        result[0],
        result[2],
        0.6,
        &mut buffer,
    ));
    assert!(space_below_empty(
        &mut world,
        &bx,
        0.5,
        0.8,
        0.6,
        &mut buffer
    ));
}

#[test]
fn malformed_or_over_budget_motion_fails_closed_before_querying_the_world() {
    let mut world = Boxes::default();
    let bx = body([0.0; 3]);
    let mut buffers = Bufs::default();
    for movement in [
        [f64::NAN, 0.0, 0.0],
        [65.0, 0.0, 0.0],
        [f64::INFINITY, 0.0, 0.0],
    ] {
        assert_eq!(
            collide(&mut world, &bx, movement, true, 0.6, &mut buffers),
            [0.0; 3]
        );
    }
    let huge = bounds([-10_000.0; 3], [10_000.0; 3]);
    assert!(!space_empty(&mut world, &huge, &mut buffers.a));
    assert_eq!(world.queries, 0);
    let moderate = bounds([0.0; 3], [5.0; 3]);
    assert!(collisions(&mut world, &moderate, &mut buffers.a));
    assert!(world.queries <= MAX_QUERY_CELLS);
}

#[test]
fn collision_has_no_fixed_world_height() {
    for ground in [-512.0, 0.0, 4096.0] {
        let mut world = Boxes::new(vec![bounds([-5.0, ground - 1.0, -5.0], [5.0, ground, 5.0])]);
        let result = collide(
            &mut world,
            &body([0.0, ground, 0.0]),
            [0.25, -0.0784, 0.0],
            true,
            0.6,
            &mut Bufs::default(),
        );
        near(result[0], 0.25);
        near(result[1], 0.0);
    }
}

#[test]
fn invalid_supplied_shapes_do_not_make_the_world_empty() {
    struct InvalidShape;
    impl Solids for InvalidShape {
        fn shapes_at(&mut self, _: i32, _: i32, _: i32, out: &mut Vec<Aabb>) {
            out.push(bounds([f64::NAN, 0.0, 0.0], [1.0; 3]));
        }
        fn slip(&mut self, _: i32, _: i32, _: i32) -> f32 {
            0.6
        }
        fn chokes(&mut self, _: i32, _: i32, _: i32) -> bool {
            false
        }
    }
    let mut world = InvalidShape;
    let mut buffers = Bufs::default();
    let bx = body([0.0; 3]);
    assert!(!space_empty(&mut world, &bx, &mut buffers.a));
    assert_eq!(
        collide(
            &mut world,
            &bx,
            [0.1, -0.0784, 0.0],
            true,
            0.6,
            &mut buffers
        ),
        [0.0; 3],
    );
}

#[test]
fn warmed_step_collision_and_sneak_queries_allocate_nothing() {
    let control = telemetry::AllocationScope::begin().unwrap();
    let allocation = std::hint::black_box(Box::new([17u8; 128]));
    std::hint::black_box(&allocation);
    drop(allocation);
    assert!(control.finish().alloc_calls > 0);

    let mut world = Boxes::new(vec![floor(), bounds([0.5, 0.0, -10.0], [5.0, 0.5, 10.0])]);
    let bx = body([0.0; 3]);
    let mut buffers = Bufs::default();
    for _ in 0..2 {
        std::hint::black_box(collide(
            &mut world,
            &bx,
            [0.6, -0.0784, 0.0],
            true,
            0.6,
            &mut buffers,
        ));
    }
    let measured = telemetry::AllocationScope::begin().unwrap();
    for _ in 0..10_000 {
        std::hint::black_box(collide(
            &mut world,
            &bx,
            [0.6, -0.0784, 0.0],
            true,
            0.6,
            &mut buffers,
        ));
        std::hint::black_box(edge_guard(
            &mut world,
            &bx,
            [0.2, -0.0784, 0.2],
            0.6,
            &mut buffers.a,
        ));
    }
    let counts = measured.finish();
    assert_eq!(counts.alloc_calls, 0);
    assert_eq!(counts.realloc_calls, 0);
    assert_eq!(counts.zeroed_alloc_calls, 0);
}
