#[path = "../src/natural.rs"]
mod natural;
use core_types::SpaceId;
use natural::*;
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
fn column(x: i32) -> Column {
    Column {
        space: SpaceId(0),
        x,
        z: -2,
    }
}
#[test]
fn canonical_metadata_bytes_and_strict_replay_keep_population_rng_and_ids_atomic() {
    let mut state = State::new(4, 3).unwrap();
    let mut expected = b"NAT1".to_vec();
    for n in [0u64, 4, 3, 1] {
        expected.extend_from_slice(&n.to_le_bytes());
    }
    expected.extend_from_slice(&0u32.to_le_bytes());
    assert_eq!(state.encode(), expected);
    let delta = state.plan(3, 9, 4, Some(column(7))).unwrap().unwrap();
    let wire = delta.encode();
    assert_eq!(wire.len(), 57);
    assert_eq!(&wire[..4], b"NAD1");
    assert_eq!(&wire[45..], &[0, 0, 0, 0, 7, 0, 0, 0, 254, 255, 255, 255]);
    state.apply(Delta::decode(&wire).unwrap()).unwrap();
    assert_eq!(state.revision(), 1);
    assert_eq!(state.rng(), 9);
    assert_eq!(state.next_id(), 4);
    assert_eq!(state.map_seed(), 4);
    assert_eq!(state.len(), 1);
    assert!(!state.is_empty());
    assert!(state.populated(column(7)));
    let before = state.encode();
    assert_eq!(state.apply(delta), Err(Error::Stale));
    assert_eq!(state.encode(), before);
    let restored = State::decode(&before).unwrap();
    assert_eq!(restored.encode(), before);
    for end in 0..before.len() {
        assert!(State::decode(&before[..end]).is_err());
    }
    for end in 0..wire.len() {
        assert!(Delta::decode(&wire[..end]).is_err());
    }
    let mut duplicate = before.clone();
    duplicate[36..40].copy_from_slice(&2u32.to_le_bytes());
    duplicate.extend_from_slice(&before[40..]);
    assert!(State::decode(&duplicate).is_err());
    assert!(matches!(state.plan(3, 10, 5, None), Err(Error::Stale)));
}
#[test]
fn cold_population_admission_is_bounded_and_idle_planning_allocates_nothing() {
    let mut state = State::new(11, 7).unwrap();
    for x in 0..MAX_COLUMNS {
        let d = state
            .plan(7, 7, 1, Some(column(x as i32)))
            .unwrap()
            .unwrap();
        state.apply(d).unwrap();
    }
    let scope = telemetry::AllocationScope::begin().unwrap();
    for _ in 0..10_000 {
        assert!(state.plan(7, 7, 1, None).unwrap().is_none());
        assert!(state.populated(column(50_000)));
    }
    let stats = scope.finish();
    assert_eq!(stats.allocating_calls(), 0);
    assert_eq!(stats.dealloc_calls, 0);
    let before = state.encode();
    assert!(matches!(
        state.plan(7, 9, 2, Some(column(70_000))),
        Err(Error::Capacity)
    ));
    assert_eq!(state.encode(), before);
    assert!(state.plan(7, 0, 1, None).is_err());
    assert!(state.plan(7, 9, 0, None).is_err());
    assert!(state.plan(7, 9, 2000, None).is_err());
}
