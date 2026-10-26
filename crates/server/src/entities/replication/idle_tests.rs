use super::*;
use crate::entities::{Config, MobRuntime, items::ItemRuntime};
use rules::entities::{MobKind, mob::Mob};
fn sources() -> (MobRuntime, ItemRuntime) {
    let mut mobs = MobRuntime::new(Config::default()).unwrap();
    mobs.insert_initial(
        Mob::spawned(EntityId(1), MobKind::Pig, SpaceId(0), [0., 1., 0.], 17).unwrap(),
        false,
    )
    .unwrap();
    (mobs, ItemRuntime::new(8192, 17).unwrap())
}
fn interest() -> Interest {
    Interest {
        space: SpaceId(0),
        center: [0., 1., 0.],
        enter: 64.,
        retain: 80.,
    }
}
fn budget() -> QueryBudget {
    QueryBudget {
        buckets: 24_000,
        probes: 65_536,
        candidates: 4096,
    }
}
fn confirm(history: &mut EntityHistory, mobs: &MobRuntime, items: &ItemRuntime) {
    let frame = history
        .plan_owner(mobs, items, interest(), Tick(1), budget())
        .unwrap()
        .0
        .unwrap();
    assert!(history.request(EntityRequest::Ack {
        revision: frame.revision
    }));
    assert!(
        history
            .plan_owner(mobs, items, interest(), Tick(2), budget())
            .unwrap()
            .0
            .is_none()
    );
}
#[test]
fn unchanged_sources_skip_work_but_full_repair_and_viewer_changes_do_not() {
    let (mobs, items) = sources();
    let mut history = EntityHistory::new(2048, 4096, 8).unwrap();
    confirm(&mut history, &mobs, &items);
    for t in 3..1003 {
        assert_eq!(
            history
                .plan_owner(&mobs, &items, interest(), Tick(t), budget())
                .unwrap()
                .1,
            Work::default()
        );
    }
    history.request(EntityRequest::Full {
        have: history.confirmed(),
    });
    let frame = history
        .plan_owner(&mobs, &items, interest(), Tick(1003), budget())
        .unwrap()
        .0
        .unwrap();
    assert!(frame.base.is_none());
    assert!(history.request(EntityRequest::Ack {
        revision: frame.revision
    }));
    history
        .plan_owner(&mobs, &items, interest(), Tick(1004), budget())
        .unwrap();
    let mut far = interest();
    far.center = [1000., 1., 1000.];
    let frame = history
        .plan_owner(&mobs, &items, far, Tick(1005), budget())
        .unwrap()
        .0
        .unwrap();
    assert_eq!(frame.removes, vec![1]);
}
#[test]
fn replacement_same_revision_length_and_startup_inserts_invalidate() {
    let (mut mobs, items) = sources();
    let mut history = EntityHistory::new(2048, 4096, 8).unwrap();
    confirm(&mut history, &mobs, &items);
    // Same variable address and persisted revision/length; replacement epoch differs.
    let mut replacement = MobRuntime::new(Config::default()).unwrap();
    replacement
        .insert_initial(
            Mob::spawned(EntityId(1), MobKind::Cow, SpaceId(0), [2., 1., 0.], 17).unwrap(),
            false,
        )
        .unwrap();
    mobs = replacement;
    let frame = history
        .plan_owner(&mobs, &items, interest(), Tick(3), budget())
        .unwrap()
        .0
        .unwrap();
    assert_eq!(frame.upserts.len(), 1);
    assert_eq!(frame.upserts[0].position, [2., 1., 0.]);
    assert!(history.request(EntityRequest::Ack {
        revision: frame.revision
    }));
    history
        .plan_owner(&mobs, &items, interest(), Tick(4), budget())
        .unwrap();
    mobs.insert_initial(
        Mob::spawned(EntityId(2), MobKind::Pig, SpaceId(0), [4., 1., 0.], 19).unwrap(),
        false,
    )
    .unwrap();
    let frame = history
        .plan_owner(&mobs, &items, interest(), Tick(5), budget())
        .unwrap()
        .0
        .unwrap();
    assert_eq!(frame.upserts.len(), 1);
    assert_eq!(frame.upserts[0].id, 2);
}
#[test]
fn capacity_hold_retries_on_full_request_budget_or_interest_change() {
    let (mut mobs, items) = sources();
    mobs.insert_initial(
        Mob::spawned(EntityId(2), MobKind::Pig, SpaceId(0), [3., 1., 0.], 19).unwrap(),
        false,
    )
    .unwrap();
    let mut history = EntityHistory::new(1, 4096, 8).unwrap();
    assert_eq!(
        history
            .plan_owner(&mobs, &items, interest(), Tick(1), budget())
            .err(),
        Some(EntityError::Capacity)
    );
    assert!(history.cached_owner.unwrap().blocked);
    history.request(EntityRequest::Full { have: None });
    assert!(history.cached_owner.is_none());
    let mut narrow = interest();
    narrow.enter = 1.;
    narrow.retain = 1.;
    let frame = history
        .plan_owner(&mobs, &items, narrow, Tick(2), budget())
        .unwrap()
        .0
        .unwrap();
    assert_eq!(frame.upserts.len(), 1);
}

#[test]
fn committed_revision_item_changes_peer_reset_and_larger_budget_invalidate() {
    let (mut mobs, mut items) = sources();
    let mut history = EntityHistory::new(2048, 4096, 8).unwrap();
    confirm(&mut history, &mobs, &items);
    let delta = mobs.plan_activity(&[(EntityId(1), true)]).unwrap();
    mobs.acknowledge(delta).unwrap();
    let result = history
        .plan_owner(&mobs, &items, interest(), Tick(3), budget())
        .unwrap();
    assert!(result.0.is_none());
    assert_eq!(
        result.1.poses, 1,
        "equal-length committed source changes are observed"
    );
    let item = rules::entities::item::ItemEntity::new(
        EntityId(100),
        SpaceId(0),
        rules::stack::ItemStack::new(rules::ids::STONE, 2).unwrap(),
        [2., 1., 0.],
        17,
        10,
    )
    .unwrap();
    items.insert_initial(item, false).unwrap();
    let frame = history
        .plan_owner(&mobs, &items, interest(), Tick(4), budget())
        .unwrap()
        .0
        .unwrap();
    assert_eq!(frame.upserts.len(), 1);
    assert_eq!(frame.upserts[0].id, 100);
    assert_eq!(frame.upserts[0].count, 2);
    assert!(history.request(EntityRequest::Ack {
        revision: frame.revision
    }));
    history
        .plan_owner(&mobs, &items, interest(), Tick(5), budget())
        .unwrap();
    items
        .acknowledge(items.plan_activity(&[(EntityId(100), true)]).unwrap())
        .unwrap();
    assert_eq!(
        history
            .plan_owner(&mobs, &items, interest(), Tick(6), budget())
            .unwrap()
            .1
            .poses,
        2
    );
    let mut peer = EntityHistory::new(2048, 4096, 8).unwrap();
    let tiny = QueryBudget {
        buckets: 1,
        ..budget()
    };
    assert_eq!(
        peer.plan_owner(&mobs, &items, interest(), Tick(7), tiny)
            .err(),
        Some(EntityError::Capacity)
    );
    let frame = peer
        .plan_owner(&mobs, &items, interest(), Tick(8), budget())
        .unwrap()
        .0
        .unwrap();
    assert!(frame.base.is_none());
    assert_eq!(frame.upserts.len(), 2);
}
