use client::lighting::{LightDirtyTracker, LightUploadScratch, PreparedLight};
use core_types::{
    BlockPos, Cell, CellProperties, ChannelMask, LocalIdx, PageId, SpaceId, Tick, Version,
};
use world::{LightChange, LightSample, Transaction, World};
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
fn page() -> PageId {
    PageId {
        space: SpaceId(0),
        x: -1,
        y: 0,
        z: -1,
    }
}
fn change(index: u16) -> LightChange {
    LightChange {
        page: page(),
        index: LocalIdx::new(index).unwrap(),
        old: LightSample::default(),
        new: LightSample {
            sky: 7,
            block: 15,
            rgb: [15, 0, 0],
        },
    }
}
fn world() -> World {
    let mut world = World::new(|_cell: Cell| CellProperties::AIR, 64);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.create_page(page(), vec![Cell::AIR; 4096].into_boxed_slice());
    world.commit(tx).unwrap();
    world
}
#[test]
fn corner_change_updates_eight_halos_and_interior_exactly_one() {
    let mut tracker = LightDirtyTracker::new(8);
    assert_eq!(tracker.mark_light(&change(0), Version(2)).unwrap(), 8);
    assert_eq!(tracker.pending_pages(), 8);
    let mut scratch = LightUploadScratch::default();
    let world = world();
    let mut prepared = 0;
    while let Some(page) = tracker.first_page() {
        match tracker.prepare(&world, page, &mut scratch).unwrap() {
            PreparedLight::Patch {
                version,
                updates,
                work,
                ..
            } => {
                assert_eq!(updates.len(), 1);
                assert_eq!(work.samples, 1);
                assert_eq!(version, Version(2));
            }
            _ => panic!("single light change must not rescan page"),
        };
        assert!(tracker.acknowledge(page, Version(2)));
        prepared += 1;
    }
    assert_eq!(prepared, 8);
    let mut interior = change(0);
    interior.index = LocalIdx::new(8 + 16 * (8 + 16 * 8)).unwrap();
    assert_eq!(tracker.mark_light(&interior, Version(3)).unwrap(), 1);
}
#[test]
fn backpressure_keeps_existing_dirty_data_and_has_no_partial_admission() {
    let mut tracker = LightDirtyTracker::new(7);
    assert!(tracker.mark_light(&change(0), Version(1)).is_err());
    assert_eq!(tracker.pending_pages(), 0);
    let mut tracker = LightDirtyTracker::new(26);
    assert!(tracker.mark_page_boundary(page(), Version(1)).is_err());
    assert_eq!(tracker.pending_pages(), 0);
    tracker.mark_full(page(), Version(2)).unwrap();
    assert_eq!(tracker.pending_pages(), 1);
    assert!(!tracker.acknowledge(page(), Version(1)));
    assert!(tracker.acknowledge(page(), Version(2)));
}
#[test]
fn dirty_rgb_reads_current_light_without_invalidating_geometry() {
    let mut world = world();
    let before = world.input_version(page(), ChannelMask::SHAPE | ChannelMask::OPACITY);
    let position = BlockPos {
        space: SpaceId(0),
        x: -16,
        y: 0,
        z: -16,
    };
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    tx.set_light(
        position,
        LightSample {
            sky: 0,
            block: 15,
            rgb: [15, 0, 0],
        },
    );
    let change = world.commit(tx).unwrap();
    assert_eq!(
        before,
        world.input_version(page(), ChannelMask::SHAPE | ChannelMask::OPACITY)
    );
    let mut tracker = LightDirtyTracker::new(8);
    tracker.mark_light(&change.lights[0], Version(4)).unwrap();
    let mut scratch = LightUploadScratch::default();
    let mut seen = 0;
    while let Some(page) = tracker.first_page() {
        match tracker.prepare(&world, page, &mut scratch).unwrap() {
            PreparedLight::Patch { updates, work, .. } => {
                assert_eq!(updates.len(), 1);
                assert_eq!(updates[0].value.rgb(), [15, 0, 0]);
                assert_eq!(work.known_reads, 1);
                assert_eq!(work.unknown_reads, 0);
                seen += 1;
            }
            _ => panic!("expected patch"),
        };
        tracker.acknowledge(page, Version(4));
    }
    assert_eq!(seen, 8);
}
#[test]
fn full_snapshot_has_dark_unknown_halo_and_correct_known_samples() {
    let world = world();
    let mut tracker = LightDirtyTracker::new(1);
    tracker.mark_full(page(), Version(2)).unwrap();
    let mut scratch = LightUploadScratch::default();
    match tracker.prepare(&world, page(), &mut scratch).unwrap() {
        PreparedLight::Full { samples, work, .. } => {
            assert_eq!(work.samples, 5832);
            assert_eq!(work.known_reads, 4096);
            assert_eq!(work.unknown_reads, 1736);
            assert!(samples.iter().all(|value| value.raw() == 0));
        }
        _ => panic!("new slots need full snapshot"),
    };
    tracker.mark_full(page(), Version(3)).unwrap();
    assert!(!tracker.acknowledge(page(), Version(2)));
    assert!(tracker.acknowledge(page(), Version(3)));
}
#[test]
fn idle_tracker_has_zero_allocations_with_positive_control() {
    let tracker = LightDirtyTracker::new(64);
    let control = telemetry::AllocationScope::begin().unwrap();
    let data = vec![1u8; std::hint::black_box(32)];
    std::hint::black_box(&data);
    drop(data);
    assert!(control.finish().allocating_calls() > 0);
    let scope = telemetry::AllocationScope::begin().unwrap();
    for _ in 0..10000 {
        assert_eq!(tracker.pending_pages(), 0);
        assert!(tracker.first_page().is_none());
    }
    assert_eq!(scope.finish().allocating_calls(), 0);
}
#[test]
fn newly_dirty_low_coordinates_cannot_starve_older_pages() {
    let mut tracker = LightDirtyTracker::new(4);
    let high = PageId { x: 100, ..page() };
    let low = PageId { x: -100, ..page() };
    tracker.mark_full(high, Version(1)).unwrap();
    tracker.mark_full(low, Version(2)).unwrap();
    tracker.mark_full(low, Version(3)).unwrap();
    assert_eq!(tracker.first_page(), Some(high));
    assert!(tracker.acknowledge(high, Version(1)));
    assert_eq!(tracker.first_page(), Some(low));
}
