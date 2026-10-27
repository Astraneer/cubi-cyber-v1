//! Pose history is committed presentation state, independent of upload cadence.
use presented_motion::*;
use render::{block_models, entity_gpu, presented_motion};

#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);

fn frame() -> Frame {
    Frame {
        space: 0,
        geometry_epoch: 1,
        source_epoch: 1,
        extent: [128, 64],
        origin: [0; 3],
        unjittered_view_projection: [
            [1., 0., 0., 0.],
            [0., 1., 0., 0.],
            [0., 0., 1., 0.],
            [0., 0., 0., 1.],
        ],
        reset: false,
    }
}
fn token(i: u64) -> Token {
    Token {
        frame: i,
        serial: i,
    }
}
#[test]
fn resolver_and_pose_banks_share_the_exact_presentation_transaction_after_discard() {
    use render::taa::{PresentedHistory, ResetReasons, TaaFrame};
    let mut history = PresentedHistory::default();
    let mut poses = PresentedMotion::new(1, 16.).unwrap();
    let row = actor(ObjectKey::native(1, 0, 99).unwrap(), 0.);
    let state = |frame| TaaFrame {
        frame,
        space: 0,
        scene_revision: 1,
        geometry_epoch: 1,
        source_epoch: 1,
        jitter_pixels: [0.; 2],
        reset: ResetReasons::NONE,
    };
    let first = history.begin(state(901)).unwrap();
    poses.stage(first.into(), frame(), &[row]).unwrap();
    history.mark_encoded(first).unwrap();
    history.commit_presented(first, 8).unwrap();
    poses.commit_presented(first.into()).unwrap();
    let discarded = history.begin(state(902)).unwrap();
    let mut moved = row;
    moved.pose.position[0] = 3.;
    poses.stage(discarded.into(), frame(), &[moved]).unwrap();
    history.mark_encoded(discarded).unwrap();
    history.discard(discarded).unwrap();
    poses.discard(discarded.into()).unwrap();
    assert_eq!(poses.presented_poses(), &[row.pose]);
    let replacement = history.begin(state(902)).unwrap();
    assert_eq!(replacement.frame(), discarded.frame());
    assert_ne!(replacement.serial(), discarded.serial());
    poses.stage(replacement.into(), frame(), &[moved]).unwrap();
    assert!(poses.matches(discarded.into()).is_err());
    assert_eq!(poses.matches(replacement.into()).unwrap()[0].valid, 1);
    assert_eq!(poses.presented_poses(), &[row.pose]);
    history.mark_encoded(replacement).unwrap();
    history.commit_presented(replacement, 8).unwrap();
    poses.commit_presented(replacement.into()).unwrap();
    assert_eq!(poses.presented_poses(), &[moved.pose]);
    assert_eq!(history.front().unwrap().state.frame, 902);
}
fn actor(key: ObjectKey, x: f32) -> ObjectPose<entity_gpu::EntityInstance> {
    ObjectPose {
        key,
        topology: 1,
        pose: entity_gpu::EntityInstance {
            position: [x, 0., 0.],
            body_yaw: 20.,
            head_yaw: 30.,
            pitch: 15.,
            limb_phase: x,
            limb_amount: 1.,
            kind: 5,
            flags: 0,
            color: 0,
            id_low: 0,
            id_high: 0,
            death_roll: 0.,
            scale: 1.,
            reserved: 0.,
        },
        visible: true,
        discontinuity: false,
    }
}
fn uid(record: GpuMatch) -> u64 {
    u64::from(record.surface[1]) | (u64::from(record.surface[2]) << 32)
}

#[test]
fn full_identity_and_presented_bones_survive_reorder_and_discarded_uploads() {
    let mut cache = PresentedMotion::new(8, 32.).unwrap();
    let a = ObjectKey::native(1, 0, 0xfedc_ba98_7654_3210).unwrap();
    let b = ObjectKey::named(2, 0, [7, 11]).unwrap();
    let c = ObjectKey::named(2, 0, [7, 12]).unwrap();
    let first = [actor(a, 1.), actor(b, 2.), actor(c, 3.)];
    cache.stage(token(1), frame(), &first).unwrap();
    let ids = cache.matches(token(1)).unwrap().to_vec();
    assert_eq!(uid(ids[0]), 0xfedc_ba98_7654_3210);
    assert_ne!(
        uid(ids[1]),
        uid(ids[2]),
        "full UUID upper64 cannot be discarded"
    );
    assert!(
        ids.iter()
            .all(|v| v.valid == 0 && v.previous_index == INVALID_INDEX)
    );
    assert!(cache.presented_poses().is_empty());
    cache.commit_presented(token(1)).unwrap();
    assert_eq!(cache.presented_frame(), Some((token(1), frame())));
    let moved = [actor(c, 6.), actor(a, 4.), actor(b, 5.)];
    cache.stage(token(2), frame(), &moved).unwrap();
    let metadata = cache.matches(token(2)).unwrap();
    assert_eq!(
        metadata
            .iter()
            .map(|v| v.previous_index)
            .collect::<Vec<_>>(),
        [2, 0, 1]
    );
    for (meta, old) in metadata.iter().zip([ids[2], ids[0], ids[1]]) {
        assert_eq!(meta.surface, old.surface);
        assert_eq!(meta.valid, 1);
    }
    assert_eq!(cache.candidate_poses(token(2)).unwrap()[0].limb_phase, 6.);
    assert_eq!(cache.presented_poses()[2].limb_phase, 3.);
    assert_eq!(cache.stage(token(3), frame(), &first), Err(Error::Busy));
    assert_eq!(cache.commit_presented(token(3)), Err(Error::WrongToken));
    cache.discard(token(2)).unwrap();
    assert_eq!(
        cache.stage(token(2), frame(), &first),
        Err(Error::InvalidFrame)
    );
    cache.stage(token(3), frame(), &moved).unwrap();
    assert_eq!(cache.matches(token(3)).unwrap()[0].previous_index, 2);
    assert_eq!(
        cache.presented_poses()[2].limb_phase,
        3.,
        "discard must not change presented bones"
    );
    cache.commit_presented(token(3)).unwrap();
    assert_eq!(cache.presented_poses()[0].limb_phase, 6.);
}

#[test]
fn spawn_despawn_topology_hidden_teleport_and_explicit_epochs_invalidate_history() {
    let mut cache = PresentedMotion::new(4, 16.).unwrap();
    let key = ObjectKey::native(1, 0, 9).unwrap();
    let object = actor(key, 0.);
    cache.stage(token(1), frame(), &[object]).unwrap();
    let original = cache.matches(token(1)).unwrap()[0].surface;
    cache.commit_presented(token(1)).unwrap();
    for (i, candidate) in [
        ObjectPose {
            topology: 2,
            ..object
        },
        ObjectPose {
            visible: false,
            ..object
        },
        ObjectPose {
            discontinuity: true,
            ..object
        },
        actor(key, 17.),
    ]
    .into_iter()
    .enumerate()
    {
        let t = token(i as u64 + 2);
        cache.stage(t, frame(), &[candidate]).unwrap();
        assert_eq!(cache.matches(t).unwrap()[0].valid, 0);
        if candidate.topology != object.topology {
            assert_ne!(cache.matches(t).unwrap()[0].surface[3], original[3]);
        }
        cache.discard(t).unwrap();
    }
    for (i, changed) in [
        Frame {
            geometry_epoch: 2,
            ..frame()
        },
        Frame {
            source_epoch: 2,
            ..frame()
        },
        Frame {
            space: 1,
            ..frame()
        },
        Frame {
            extent: [64, 64],
            ..frame()
        },
        Frame {
            reset: true,
            ..frame()
        },
    ]
    .into_iter()
    .enumerate()
    {
        let t = token(6 + i as u64);
        cache.stage(t, changed, &[object]).unwrap();
        assert_eq!(cache.matches(t).unwrap()[0].valid, 0);
        cache.discard(t).unwrap();
    }
    cache.stage(token(11), frame(), &[]).unwrap();
    cache.commit_presented(token(11)).unwrap();
    cache.stage(token(12), frame(), &[object]).unwrap();
    let reborn = cache.matches(token(12)).unwrap()[0];
    assert_eq!(reborn.valid, 0);
    assert_eq!(uid(reborn), 9);
    assert_ne!(reborn.surface[3], original[3]);
}

#[test]
fn rebase_preserves_world_motion_and_rigid_transform_is_previous_presented_pose() {
    let mut cache = PresentedMotion::new(2, 16.).unwrap();
    let key = ObjectKey::named(3, 5, [u64::MAX, 123]).unwrap();
    let pose = |x| RigidPose {
        model: [
            [0., 0., -1., 0.],
            [0., 1., 0., 0.],
            [1., 0., 0., 0.],
            [x, 3., 4., 1.],
        ],
    };
    let row = |x| ObjectPose {
        key,
        topology: 17,
        pose: pose(x),
        visible: true,
        discontinuity: false,
    };
    let before = Frame {
        origin: [1 << 40, 0, 0],
        ..frame()
    };
    cache.stage(token(1), before, &[row(4.)]).unwrap();
    cache.commit_presented(token(1)).unwrap();
    let after = Frame {
        origin: [(1 << 40) + 64, 0, 0],
        ..frame()
    };
    cache.stage(token(2), after, &[row(-60.)]).unwrap();
    assert_eq!(
        cache.matches(token(2)).unwrap()[0].valid,
        1,
        "integer rebase is not a teleport"
    );
    assert_eq!(cache.presented_poses()[0].model, pose(4.).model);
    assert_eq!(
        cache.candidate_poses(token(2)).unwrap()[0].model,
        pose(-60.).model
    );
    cache.discard(token(2)).unwrap();
    cache.stage(token(3), before, &[row(5.)]).unwrap();
    assert_eq!(cache.matches(token(3)).unwrap()[0].valid, 1);
}

#[test]
fn invalid_candidates_leave_the_presented_front_and_frame_token_usable() {
    assert!(matches!(
        PresentedMotion::<RigidPose>::new(0, 16.),
        Err(Error::InvalidLimits)
    ));
    assert_eq!(ObjectKey::native(0, 0, 0), Err(Error::InvalidIdentity));
    let mut cache = PresentedMotion::new(1, 16.).unwrap();
    let key = ObjectKey::native(1, 0, 8).unwrap();
    let object = actor(key, 0.);
    cache.stage(token(1), frame(), &[object]).unwrap();
    cache.commit_presented(token(1)).unwrap();
    assert_eq!(
        cache.stage(token(2), frame(), &[object, object]),
        Err(Error::Capacity)
    );
    let invalid = actor(key, f32::NAN);
    assert_eq!(
        cache.stage(token(2), frame(), &[invalid]),
        Err(Error::InvalidPose)
    );
    assert_eq!(
        cache.stage(
            token(2),
            Frame {
                extent: [0, 64],
                ..frame()
            },
            &[object]
        ),
        Err(Error::InvalidFrame)
    );
    assert_eq!(cache.matches(token(2)), Err(Error::WrongToken));
    assert_eq!(cache.presented_poses()[0].position, [0.; 3]);
    cache.stage(token(2), frame(), &[object]).unwrap();
    assert_eq!(cache.matches(token(2)).unwrap()[0].valid, 1);
    let mut larger = PresentedMotion::new(2, 16.).unwrap();
    assert_eq!(
        larger.stage(token(1), frame(), &[object, object]),
        Err(Error::Duplicate)
    );
    assert!(larger.presented_poses().is_empty());
}

#[test]
fn thousand_static_chest_and_rotor_snapshots_use_no_frame_allocation() {
    let mut cache = PresentedMotion::new(4, 16.).unwrap();
    let rows = [
        ObjectPose {
            key: ObjectKey::named(4, 0, [1, 2]).unwrap(),
            topology: 3,
            pose: block_models::BlockInstance {
                phase: 0.3,
                ..Default::default()
            },
            visible: true,
            discontinuity: false,
        },
        ObjectPose {
            key: ObjectKey::named(4, 0, [1, 3]).unwrap(),
            topology: 9,
            pose: block_models::BlockInstance {
                kind: 9,
                phase: 1.2,
                ..Default::default()
            },
            visible: true,
            discontinuity: false,
        },
    ];
    assert!(cache.reserved_bytes() > 4 * 2 * 64);
    cache.stage(token(1), frame(), &rows).unwrap();
    cache.commit_presented(token(1)).unwrap();
    let scope = telemetry::AllocationScope::begin().unwrap();
    for i in 2..1002 {
        cache.stage(token(i), frame(), &rows).unwrap();
        assert!(
            cache
                .matches(token(i))
                .unwrap()
                .iter()
                .all(|m| m.valid == 1)
        );
        assert_eq!(cache.work().new_surfaces, 0);
        assert_eq!(cache.work().matched, 2);
        assert!(cache.work().identity_comparisons <= 8);
        cache.commit_presented(token(i)).unwrap();
    }
    let counts = scope.finish();
    assert_eq!(counts.allocating_calls(), 0);
    assert_eq!(counts.dealloc_calls, 0);
}

#[test]
fn actual_model_kind_connection_and_primary_visibility_are_automatic_guards() {
    let mut entities = PresentedMotion::new(1, 16.).unwrap();
    let key = ObjectKey::native(1, 0, 9).unwrap();
    let row = actor(key, 0.);
    entities.stage(token(1), frame(), &[row]).unwrap();
    let surface = entities.matches(token(1)).unwrap()[0].surface;
    entities.commit_presented(token(1)).unwrap();
    let mut changed = row;
    changed.pose.kind = 1;
    entities.stage(token(2), frame(), &[changed]).unwrap();
    assert_eq!(entities.matches(token(2)).unwrap()[0].valid, 0);
    assert_ne!(
        entities.matches(token(2)).unwrap()[0].surface[3],
        surface[3]
    );
    entities.discard(token(2)).unwrap();
    changed = row;
    changed.pose.flags = entity_gpu::PRIMARY_HIDDEN;
    entities.stage(token(3), frame(), &[changed]).unwrap();
    assert_eq!(entities.matches(token(3)).unwrap()[0].valid, 0);
    let mut props = PresentedMotion::new(1, 16.).unwrap();
    let row = ObjectPose {
        key: ObjectKey::named(2, 0, [0, 1]).unwrap(),
        topology: 1,
        pose: block_models::BlockInstance::default(),
        visible: true,
        discontinuity: false,
    };
    props.stage(token(1), frame(), &[row]).unwrap();
    props.commit_presented(token(1)).unwrap();
    let mut changed = row;
    changed.pose.connections = 1;
    props.stage(token(2), frame(), &[changed]).unwrap();
    assert_eq!(props.matches(token(2)).unwrap()[0].valid, 0);
    props.discard(token(2)).unwrap();
    changed = row;
    changed.pose.flags = block_models::HIDDEN;
    props.stage(token(3), frame(), &[changed]).unwrap();
    assert_eq!(props.matches(token(3)).unwrap()[0].valid, 0);
}
