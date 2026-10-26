use client::{
    far_water_source::{Context, Error, FarWaterSource, Limits},
    far_water_worker::{BuiltFarWater, MAX_RETAINED_RESULT_BYTES},
};
use core_types::{PageId, SpaceId, Version};
use net::{height::GeometryStamp, water::WaterHalo};
use std::sync::Arc;
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
fn page(x: i32) -> PageId {
    PageId {
        space: SpaceId(0),
        x,
        y: 0,
        z: 0,
    }
}
fn image(x: i32, view: u64, code: u8) -> Arc<WaterHalo> {
    let mut cells = Box::new([1; 5832]);
    for y in -1..=16 {
        for z in -1..=16 {
            for x in -1..=16 {
                cells[(x + 1) as usize + 18 * ((z + 1) as usize + 18 * (y + 1) as usize)] =
                    if y == 0 {
                        code
                    } else if y < 0 {
                        2
                    } else {
                        1
                    };
            }
        }
    }
    Arc::new(WaterHalo {
        page: page(x),
        geometry_source: Version(19),
        geometry_view: 23,
        column_source: 37,
        water_view: view,
        cells,
        tints: [0xf800; 256],
    })
}
fn binding(x: i32) -> (GeometryStamp, u64) {
    (
        GeometryStamp {
            page: page(x),
            source: Version(19),
            view: 23,
        },
        37,
    )
}
fn source() -> FarWaterSource {
    FarWaterSource::new(Limits {
        max_pages: 4,
        ..Limits::default()
    })
    .unwrap()
}
fn finish(owner: &mut FarWaterSource) -> Context {
    let (ctx, task) = owner.take_task().unwrap();
    assert!(
        owner
            .complete(ctx, task.execute(|| false).unwrap())
            .unwrap()
    );
    ctx
}
#[test]
fn leases_reject_forgotten_same_public_stamp_and_old_completions_do_not_consume_new_job() {
    let mut owner = source();
    owner.set_wanted(&[binding(0)]).unwrap();
    owner.admit(image(0, 1, 10)).unwrap();
    let (old, task) = owner.take_task().unwrap();
    let built = task.execute(|| false).unwrap();
    owner.set_wanted(&[]).unwrap();
    owner.set_wanted(&[binding(0)]).unwrap();
    owner.admit(image(0, 1, 10)).unwrap();
    let (new, newtask) = owner.take_task().unwrap();
    assert_ne!(old.epoch, new.epoch);
    assert_eq!(old.stamp, new.stamp);
    assert!(!owner.complete(old, built).unwrap());
    assert_eq!(owner.stats().running, 1);
    let built = newtask.execute(|| false).unwrap();
    let duplicate = BuiltFarWater::decode(&built.encode().unwrap()).unwrap();
    assert!(owner.complete(new, built).unwrap());
    assert!(!owner.complete(new, duplicate).unwrap());
    assert_eq!(owner.stats().running, 0);
    assert_eq!(owner.stats().ready_queue, 1);
    let mut old_owner = source();
    old_owner.set_wanted(&[binding(0)]).unwrap();
    old_owner.admit(image(0, 1, 10)).unwrap();
    let (old_context, old_task) = old_owner.take_task().unwrap();
    drop(old_owner);
    let mut recreated = source();
    recreated.set_wanted(&[binding(0)]).unwrap();
    recreated.admit(image(0, 1, 10)).unwrap();
    let (new_context, _) = recreated.take_task().unwrap();
    assert_ne!(old_context.epoch, new_context.epoch);
    assert!(
        !recreated
            .complete(old_context, old_task.execute(|| false).unwrap())
            .unwrap()
    );
    assert_eq!(recreated.stats().running, 1);
}
#[test]
fn reserve_worst_result_bytes_unknown_parks_until_new_view_and_failures_are_bounded() {
    let mut owner = FarWaterSource::new(Limits {
        max_pages: 4,
        max_running: 4,
        max_ready_bytes: MAX_RETAINED_RESULT_BYTES * 2,
        ..Limits::default()
    })
    .unwrap();
    owner
        .set_wanted(&[binding(0), binding(1), binding(2)])
        .unwrap();
    for x in 0..3 {
        owner.admit(image(x, 1, 10)).unwrap();
    }
    let (a, ta) = owner.take_task().unwrap();
    let (b, tb) = owner.take_task().unwrap();
    assert!(owner.take_task().is_none());
    assert_eq!(owner.stats().reserved_bytes, MAX_RETAINED_RESULT_BYTES * 2);
    assert!(owner.complete(a, ta.execute(|| false).unwrap()).unwrap());
    assert!(
        owner.take_task().is_none(),
        "ready bytes plus full reservation cap future completions"
    );
    assert!(owner.complete(b, tb.execute(|| false).unwrap()).unwrap());
    let (c, _) = owner.take_task().unwrap();
    assert!(owner.fail(c));
    let (d, _) = owner.take_task().unwrap();
    assert!(owner.fail(d));
    let (e, _) = owner.take_task().unwrap();
    assert!(owner.fail(e));
    assert!(owner.take_task().is_none());
    let mut owner = source();
    owner.set_wanted(&[binding(0)]).unwrap();
    owner.admit(image(0, 1, 0)).unwrap();
    let (ctx, task) = owner.take_task().unwrap();
    assert!(
        !owner
            .complete(ctx, task.execute(|| false).unwrap())
            .unwrap()
    );
    assert!(owner.take_task().is_none());
    assert_eq!(owner.stats().unknown_held, 1);
    owner.admit(image(0, 2, 10)).unwrap();
    finish(&mut owner);
    assert_eq!(owner.stats().ready_queue, 1);
}
#[test]
fn coalesced_ready_queue_stays_bounded_and_unchanged_metadata_phases_allocate_nothing() {
    let mut owner = source();
    let wanted = [binding(0)];
    owner.set_wanted(&wanted).unwrap();
    for view in 1..=1000 {
        owner.admit(image(0, view, 10)).unwrap();
        finish(&mut owner);
        assert_eq!(owner.stats().ready_queue, 1);
    }
    let before = owner.stats();
    let scope = telemetry::AllocationScope::begin().unwrap();
    for _ in 0..10000 {
        assert!(!owner.set_wanted(&wanted).unwrap());
        assert!(owner.take_task().is_none());
        assert!(!owner.all_ready());
    }
    let count = scope.finish();
    assert_eq!(count.allocating_calls(), 0);
    assert_eq!(count.dealloc_calls, 0);
    assert_eq!(owner.stats(), before);
    let mut invalid = vec![binding(0), binding(0)];
    assert_eq!(owner.set_wanted(&invalid), Err(Error::Binding));
    invalid = (0..5).map(binding).collect();
    assert_eq!(owner.set_wanted(&invalid), Err(Error::Capacity));
    assert_eq!(owner.stats(), before);
}
