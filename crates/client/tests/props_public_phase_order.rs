//! A public frame arriving after the only CPU props phase cannot be drained by
//! GPU-record flush. This is the actual public owner with no GPU dependency.
use client::props_source::{PropsLimits, PropsSource};
use core_types::{Cell, PageId, SpaceId, Tick, Version};
use std::sync::Arc;
use world::{Transaction, World};

fn fixture() -> (World, PageId) {
    let page = PageId {
        space: SpaceId(0),
        x: 0,
        y: 5,
        z: 0,
    };
    let mut world = World::new(rules::properties, 1024);
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    tx.create_page(page, vec![Cell::AIR; 4096]);
    world.commit(tx).unwrap();
    (world, page)
}
fn owner(world: &mut World) -> PropsSource {
    let mut source = PropsSource::attach(
        world,
        PropsLimits {
            max_pages: 1,
            max_instances: 1,
            max_running: 1,
            max_ready: 1,
            max_age_ticks: 40,
        },
    )
    .unwrap();
    while let Some(job) = source.take_job(world, |_| Box::new([u32::MAX; 256])) {
        source
            .complete(world, job.execute(|| false).unwrap())
            .map_err(|_| ())
            .unwrap();
        source.pump_ready(world, 256).unwrap();
    }
    assert!(source.stats(world).settled);
    source
}
fn frame(page: PageId, view: u64) -> Arc<net::props::Frame> {
    Arc::new(net::props::Frame {
        page,
        cell_view: 1,
        view,
        source: Version(view),
        tick: Tick(view),
        hints: Vec::new(),
    })
}
#[test]
fn latest_public_frame_after_cpu_phase_never_settles_but_import_before_phase_does() {
    let (mut world, page) = fixture();
    let mut source = owner(&mut world);
    for view in 1..=100 {
        // The prior browser order: process old public work, then socket import.
        source.pump_public(Tick(view), 0., 256);
        assert!(source.stats(&world).settled);
        assert!(source.publish_public_frame(frame(page, view), 1).unwrap());
        let pending = source.stats(&world);
        assert_eq!(pending.queued, 0);
        assert_eq!(pending.light_bindings, 0);
        assert_eq!(pending.public_pages, 1);
        assert!(
            !pending.settled,
            "new public work cannot be consumed by an earlier phase"
        );
        // GPU flush only drains dirty/light-bindings, not public_jobs. Even this
        // empty public page therefore blocks all-queues-empty input readiness.
    }
    source.detach(&mut world);
    let mut source = owner(&mut world);
    for view in 1..=100 {
        // Native and candidate browser order: import first, then one CPU phase.
        assert!(source.publish_public_frame(frame(page, view), 1).unwrap());
        assert_eq!(source.pump_public(Tick(view), 0., 256), 1);
        assert!(source.stats(&world).settled);
    }
    source.detach(&mut world);
}
