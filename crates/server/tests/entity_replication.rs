use core_types::{SpaceId, Tick};
use net::entities::EntityRequest;
use rules::entities::{
    EntityError, EntityId, MobKind,
    item::{ItemEntity, ItemStore},
    mob::{EntityPatch, EntityStore, Mob},
    spatial::QueryBudget,
};
use server::entities::replication::{EntityHistory, Interest};
fn interest() -> Interest {
    Interest {
        space: SpaceId(0),
        center: [0.0, 1.0, 0.0],
        enter: 8.0,
        retain: 10.0,
    }
}
fn shift(store: &mut EntityStore, id: u64, x: f64) {
    let before = *store.get(EntityId(id)).unwrap();
    let mut after = before;
    after.body.position[0] = x;
    store
        .apply_patches(&[EntityPatch {
            before: Some(before),
            after: Some(after),
            before_active: true,
            after_active: true,
        }])
        .unwrap();
}
#[test]
fn spatial_aoi_hysteresis_and_confirmed_base_survive_lost_sends() {
    let mut mobs = EntityStore::new(3).unwrap();
    let items = ItemStore::new(0).unwrap();
    for (id, x) in [(1, 7.0), (2, 9.0), (3, 5000.0)] {
        mobs.insert(
            Mob::new(EntityId(id), MobKind::Pig, [x, 1.0, 0.0]).unwrap(),
            true,
        )
        .unwrap();
    }
    let mut history = EntityHistory::new(3, 3, 3).unwrap();
    let (first, work) = history
        .plan(&mobs, &items, interest(), Tick(1), QueryBudget::default())
        .unwrap();
    let first = first.unwrap();
    assert_eq!(first.upserts.iter().map(|p| p.id).collect::<Vec<_>>(), [1]);
    assert_eq!(work.candidates, 2);
    assert!(history.request(EntityRequest::Ack {
        revision: first.revision
    }));
    shift(&mut mobs, 1, 9.5);
    let second = history
        .plan(&mobs, &items, interest(), Tick(2), QueryBudget::default())
        .unwrap()
        .0
        .unwrap();
    assert_eq!(second.base, Some(first.revision));
    assert_eq!(second.upserts[0].position[0], 9.5);
    // Packet 2 and its ACK are lost. Packet 3 still removes against frame 1.
    shift(&mut mobs, 1, 10.5);
    let third = history
        .plan(&mobs, &items, interest(), Tick(3), QueryBudget::default())
        .unwrap()
        .0
        .unwrap();
    assert_eq!(third.base, Some(first.revision));
    assert_eq!(third.removes, [1]);
    assert!(third.upserts.is_empty());
    assert!(!history.request(EntityRequest::Ack { revision: 999 }));
    assert!(history.request(EntityRequest::Full { have: Some(12345) }));
    let full = history
        .plan(&mobs, &items, interest(), Tick(4), QueryBudget::default())
        .unwrap()
        .0
        .unwrap();
    assert!(full.base.is_none());
    assert!(full.upserts.is_empty());
}
#[test]
fn bounded_history_eviction_falls_back_and_capacity_never_publishes_partial_scene() {
    let mut mobs = EntityStore::new(2).unwrap();
    let items = ItemStore::new(0).unwrap();
    mobs.insert(
        Mob::new(EntityId(1), MobKind::Pig, [1.0, 1.0, 0.0]).unwrap(),
        true,
    )
    .unwrap();
    let mut history = EntityHistory::new(1, 2, 1).unwrap();
    let first = history
        .plan(&mobs, &items, interest(), Tick(1), QueryBudget::default())
        .unwrap()
        .0
        .unwrap();
    assert!(history.request(EntityRequest::Ack {
        revision: first.revision
    }));
    shift(&mut mobs, 1, 2.0);
    let second = history
        .plan(&mobs, &items, interest(), Tick(2), QueryBudget::default())
        .unwrap()
        .0
        .unwrap();
    assert!(second.base.is_some());
    shift(&mut mobs, 1, 3.0);
    let third = history
        .plan(&mobs, &items, interest(), Tick(3), QueryBudget::default())
        .unwrap()
        .0
        .unwrap();
    assert!(third.base.is_none());
    mobs.insert(
        Mob::new(EntityId(2), MobKind::Cow, [1.0, 1.0, 0.0]).unwrap(),
        true,
    )
    .unwrap();
    assert!(matches!(
        history.plan(&mobs, &items, interest(), Tick(4), QueryBudget::default()),
        Err(EntityError::Capacity)
    ));
    mobs.remove(EntityId(2)).unwrap();
    let next = history
        .plan(&mobs, &items, interest(), Tick(5), QueryBudget::default())
        .unwrap()
        .0
        .unwrap();
    assert_eq!(next.revision, third.revision + 1);
}
#[test]
fn public_item_count_death_lifecycle_and_precision_come_from_committed_stores() {
    let mut mobs = EntityStore::new(1).unwrap();
    let mut items = ItemStore::new(1).unwrap();
    let mut mob = Mob::new(EntityId(1), MobKind::Sheep, [1_000_000_000.125, 1.0, 0.0]).unwrap();
    mob.life.damage(
        rules::survival::health::DamageKind::Generic,
        100.0,
        (0, 0.0),
        false,
    );
    mobs.insert(mob, true).unwrap();
    items
        .insert(
            ItemEntity::new(
                EntityId(2),
                SpaceId(0),
                rules::stack::ItemStack::new(rules::ids::DIRT, 5).unwrap(),
                [1_000_000_000.25, 1.0, 0.0],
                1,
                10,
            )
            .unwrap(),
            true,
        )
        .unwrap();
    let mut history = EntityHistory::new(2, 2, 2).unwrap();
    let interest = Interest {
        center: [1_000_000_000.0, 1.0, 0.0],
        ..interest()
    };
    let first = history
        .plan(&mobs, &items, interest, Tick(1), QueryBudget::default())
        .unwrap()
        .0
        .unwrap();
    assert_eq!(first.upserts[0].position[0], 1_000_000_000.125);
    assert_eq!(first.upserts[0].flags & 4, 4);
    assert_eq!(first.upserts[1].count, 5);
    assert_eq!(first.upserts[1].color, u32::from(rules::ids::DIRT));
    assert!(history.request(EntityRequest::Ack {
        revision: first.revision
    }));
    items.remove(EntityId(2)).unwrap();
    mobs.remove(EntityId(1)).unwrap();
    let gone = history
        .plan(&mobs, &items, interest, Tick(2), QueryBudget::default())
        .unwrap();
    assert_eq!(gone.0.unwrap().removes, [1, 2]);
    assert_eq!(gone.1.candidates, 0);
    assert_eq!(gone.1.probes, 0);
}
