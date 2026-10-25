#![cfg(not(target_arch = "wasm32"))]
use client::network::entities::{Apply, EntityReplica, ReplicaError};
use core_types::{SpaceId, Tick};
use net::entities::{self, EntityFrame, EntityPose, EntityRequest};
use rules::entities::{
    EntityId, MobKind,
    item::ItemStore,
    mob::{EntityPatch, EntityStore, Instance, Mob},
    spatial::QueryBudget,
};
use server::entities::replication::{EntityHistory, Interest};
use std::hint::black_box;
use telemetry::{AllocationCounts, AllocationScope, CountingAllocator};
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator::new(std::alloc::System);
fn pose(id: u64) -> EntityPose {
    EntityPose {
        id,
        space: SpaceId(0),
        position: [1_000_000_000.125, 1.0, 0.0],
        body_yaw: 359.0,
        head_yaw: 359.0,
        pitch: 0.0,
        limb_phase: 0.0,
        limb_amount: 0.0,
        death_roll: 0.0,
        scale: 1.0,
        extra: 0.0,
        kind: 0,
        flags: 64,
        color: 0,
        count: 0,
        damage: 0,
    }
}
fn frame(
    revision: u64,
    base: Option<u64>,
    tick: u64,
    upserts: Vec<EntityPose>,
    removes: Vec<u64>,
) -> EntityFrame {
    EntityFrame {
        revision,
        base,
        tick: Tick(tick),
        upserts,
        removes,
    }
}
fn wire(frame: EntityFrame) -> EntityFrame {
    entities::decode_frame(&entities::encode_frame(&frame).unwrap()).unwrap()
}

#[test]
fn actual_owner_history_codec_replica_recovers_lost_ack_and_publishes_despawn() {
    let mut mobs = EntityStore::new(1).unwrap();
    let mob = Mob::new(EntityId(1), MobKind::Pig, [0.5, 1.0, 0.5]).unwrap();
    mobs.insert(mob, true).unwrap();
    let items = ItemStore::new(0).unwrap();
    let mut history = EntityHistory::new(2, 2, 4).unwrap();
    let interest = Interest {
        space: SpaceId(0),
        center: [0.0, 1.0, 0.0],
        enter: 8.0,
        retain: 10.0,
    };
    let mut replica = EntityReplica::new(2).unwrap();
    let first = history
        .plan(&mobs, &items, interest, Tick(1), QueryBudget::default())
        .unwrap()
        .0
        .unwrap();
    let Apply::Applied(ack) = replica.apply(wire(first.clone())).unwrap() else {
        panic!("full")
    };
    assert!(
        history.request(entities::decode_request(&entities::encode_request(ack).unwrap()).unwrap())
    );
    let mut previous = mob;
    for (revision, x) in [(2, 1.0), (3, 1.5)] {
        let mut next = previous;
        next.body.position[0] = x;
        mobs.apply_patches(&[EntityPatch {
            before: Some(previous),
            after: Some(next),
            before_active: true,
            after_active: true,
        }])
        .unwrap();
        previous = next;
        let message = history
            .plan(
                &mobs,
                &items,
                interest,
                Tick(revision),
                QueryBudget::default(),
            )
            .unwrap()
            .0
            .unwrap();
        assert_eq!(message.base, Some(1));
        let result = replica.apply(wire(message)).unwrap();
        if revision == 2 {
            assert!(matches!(
                result,
                Apply::Applied(EntityRequest::Ack { revision: 2 })
            ));
            // ACK 2 is intentionally dropped.
        } else {
            let Apply::MissingBase(request) = result else {
                panic!("gap must request full")
            };
            assert_eq!(replica.get(1).unwrap().position[0], 1.0);
            assert!(history.request(request));
        }
    }
    let full = history
        .plan(&mobs, &items, interest, Tick(4), QueryBudget::default())
        .unwrap()
        .0
        .unwrap();
    assert_eq!(full.base, None);
    let Apply::Applied(ack) = replica.apply(wire(full)).unwrap() else {
        panic!("recovery")
    };
    history.request(ack);
    assert_eq!(replica.get(1).unwrap().position[0], 1.5);
    mobs.remove(EntityId(1)).unwrap();
    let gone = history
        .plan(&mobs, &items, interest, Tick(5), QueryBudget::default())
        .unwrap()
        .0
        .unwrap();
    replica.apply(wire(gone)).unwrap();
    assert!(replica.is_empty());
    assert!(matches!(
        replica.apply(first).unwrap(),
        Apply::Stale(EntityRequest::Ack { revision: 5 })
    ));
}
#[test]
fn interpolation_wraps_angles_and_subtracts_origin_before_float_conversion() {
    let mut replica = EntityReplica::new(2).unwrap();
    let a = pose(1);
    replica
        .apply(frame(1, None, 10, vec![a], Vec::new()))
        .unwrap();
    let mut b = a;
    b.position[0] += 0.25;
    b.body_yaw = 1.0;
    b.head_yaw = 1.0;
    replica
        .apply(frame(2, Some(1), 12, vec![b], Vec::new()))
        .unwrap();
    let mut out = [Instance::default(); 2];
    assert_eq!(
        replica.instances(SpaceId(0), 11.0, [1_000_000_000, 0, 0], &mut out),
        Ok(1)
    );
    assert_eq!(out[0].position[0], 0.25);
    assert_eq!(out[0].body_yaw, 360.0);
    assert_eq!(replica.instances(SpaceId(1), 11.0, [0; 3], &mut out), Ok(0));
    // Cross-space teleports reset interpolation instead of crossing a world.
    b.space = SpaceId(1);
    b.position[0] = -100.0;
    replica
        .apply(frame(3, Some(2), 14, vec![b], Vec::new()))
        .unwrap();
    replica
        .instances(SpaceId(1), 13.0, [0; 3], &mut out)
        .unwrap();
    assert_eq!(out[0].position[0], -100.0);
}
#[test]
fn invalid_lifecycle_kind_capacity_and_time_preserve_previous_scene() {
    let mut replica = EntityReplica::new(1).unwrap();
    replica
        .apply(frame(1, None, 10, vec![pose(1)], Vec::new()))
        .unwrap();
    assert_eq!(replica.len(), 1);
    assert_eq!(replica.poses().count(), 1);
    assert_eq!(
        replica.apply(frame(2, Some(1), 11, vec![pose(2)], Vec::new())),
        Err(ReplicaError::Capacity)
    );
    let mut changed = pose(1);
    changed.kind = 1;
    assert_eq!(
        replica.apply(frame(2, Some(1), 11, vec![changed], Vec::new())),
        Err(ReplicaError::Kind)
    );
    assert_eq!(
        replica.apply(frame(2, Some(1), 9, vec![pose(1)], Vec::new())),
        Err(ReplicaError::Time)
    );
    assert_eq!(replica.revision(), Some(1));
    assert_eq!(replica.get(1).unwrap(), pose(1));
}
#[test]
fn thousand_replicated_visible_actors_write_fixed_instances_without_allocations() {
    let mut replica = EntityReplica::new(1000).unwrap();
    replica
        .apply(wire(frame(
            1,
            None,
            1,
            (0..1000).map(pose).collect(),
            Vec::new(),
        )))
        .unwrap();
    let mut out = vec![Instance::default(); 1000];
    let positive = AllocationScope::begin().unwrap();
    let allocation = Vec::<u8>::with_capacity(black_box(31));
    black_box(&allocation);
    assert!(positive.finish().allocating_calls() > 0);
    drop(allocation);
    let scope = AllocationScope::begin().unwrap();
    for _ in 0..100 {
        assert_eq!(
            replica.instances(SpaceId(0), black_box(1.0), [1_000_000_000, 0, 0], &mut out),
            Ok(1000)
        );
        black_box(&out);
    }
    assert_eq!(scope.finish(), AllocationCounts::ZERO);
    assert!(out.iter().all(|instance| instance.position[0] == 0.125));
    assert_eq!(std::mem::size_of_val(out.as_slice()), 64_000);
}
