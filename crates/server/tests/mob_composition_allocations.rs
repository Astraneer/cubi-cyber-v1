//! Empty authoritative owner extensions retain the existing validated patch bank.
use core_types::SpaceId;
use rules::entities::{EntityId, MobKind, mob::Mob};
use server::entities::{Config, MobRuntime};
use telemetry::{AllocationScope, CountingAllocator};
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator::new(std::alloc::System);
#[test]
fn empty_owner_composition_keeps_1024_changes_without_rebuilding_large_inline_tree() {
    let mut owner = MobRuntime::new(Config {
        maximum_entities: 1024,
        ..Config::default()
    })
    .unwrap();
    for id in 1..=1024 {
        owner
            .insert_initial(
                Mob::spawned(
                    EntityId(id),
                    MobKind::Pig,
                    SpaceId(0),
                    [id as f64, 1., 1.],
                    id,
                )
                .unwrap(),
                true,
            )
            .unwrap();
    }
    let activity: Vec<_> = (1..=1024).map(|id| (EntityId(id), false)).collect();
    let before = owner.plan_activity(&activity).unwrap().encode().unwrap();
    let reversed: Vec<_> = activity.iter().rev().copied().collect();
    let delta = owner.plan_activity(&reversed).unwrap();
    let storage = delta.patches.as_ptr();
    let scope = AllocationScope::begin().unwrap();
    let delta = owner
        .extend_owned(delta, std::iter::empty(), &[], &[])
        .unwrap();
    let counts = scope.finish();
    assert_eq!(delta.patches.as_ptr(), storage);
    assert!(counts.allocated_bytes < 64 * 1024, "{counts:?}");
    assert_eq!(delta.encode().unwrap(), before);
    assert_eq!(
        owner.entities().active_len(),
        1024,
        "detached plan does not publish"
    );
    owner.acknowledge(delta).unwrap();
    assert_eq!(owner.entities().len(), 1024);
    assert_eq!(owner.entities().active_len(), 0);
    let noop = owner.plan_activity(&activity).unwrap();
    let noop = owner
        .extend_owned(noop, std::iter::empty(), &[], &[])
        .unwrap();
    assert!(noop.is_empty());
}
