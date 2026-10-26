use client::network::machine_packets as packets;
use core_types::{LocalIdx, PageId, SpaceId, Tick, Version};
use net::machine_packets::{ACTIVE, Frame, Pose, Request};
use packets::{Apply, PIPE_INSTANCE_FLAG, PacketPage};
use rules::{entities::mob::Instance, ids::STONE, items::I_ENERGY_TABLET};
use telemetry::{AllocationCounts, AllocationScope, CountingAllocator};
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator::new(std::alloc::System);
fn page() -> PageId {
    PageId {
        space: SpaceId(0),
        x: 1_000_000,
        y: 0,
        z: 0,
    }
}
fn pose(id: u64) -> Pose {
    Pose {
        id,
        index: LocalIdx::new(546).unwrap(),
        item: STONE,
        count: 1,
        color: 0,
        entry: 5,
        exit: 3,
        progress: 5,
        flags: ACTIVE,
        epoch: Tick(1),
    }
}
fn full() -> Frame {
    Frame {
        page: page(),
        view_version: Version(7),
        revision: 1,
        base: None,
        tick: Tick(1),
        upserts: vec![pose(1)],
        removes: vec![],
    }
}
#[test]
fn certified_packet_lifecycle_missing_base_and_late_invalid_row_are_atomic() {
    let mut replica = PacketPage::new(page(), 2).unwrap();
    let request = replica.apply(full(), Version(7)).unwrap();
    assert_eq!(
        request,
        Apply::Applied(Request::Ack {
            page: page(),
            view_version: Version(7),
            revision: 1
        })
    );
    let mut missing = full();
    missing.revision = 3;
    missing.base = Some(2);
    assert!(matches!(
        replica.apply(missing, Version(7)).unwrap(),
        Apply::MissingBase(_)
    ));
    assert_eq!(replica.revision(), Some(1));
    let mut wrong = full();
    wrong.revision = 2;
    wrong.view_version = Version(8);
    assert!(matches!(
        replica.apply(wrong, Version(7)).unwrap(),
        Apply::WrongView(_)
    ));
    let before: Vec<_> = replica.poses().collect();
    let mut invalid = full();
    invalid.revision = 2;
    invalid.upserts.push(Pose {
        id: 2,
        item: u16::MAX,
        ..pose(2)
    });
    assert!(replica.apply(invalid, Version(7)).is_err());
    assert_eq!(replica.poses().collect::<Vec<_>>(), before);
    let mut delta = full();
    delta.revision = 2;
    delta.base = Some(1);
    delta.tick = Tick(2);
    delta.upserts.clear();
    delta.removes = vec![1];
    replica.apply(delta, Version(7)).unwrap();
    assert!(replica.is_empty());
    assert!(matches!(
        replica.apply(full(), Version(7)).unwrap(),
        Apply::Stale(_)
    ));
    assert!(replica.is_empty());
    let mut changed = full();
    changed.revision = 4;
    changed.view_version = Version(8);
    changed.tick = Tick(3);
    replica.apply(changed, Version(8)).unwrap();
    assert_eq!(replica.view_version(), Some(Version(8)));
    assert_eq!(replica.tick(), Tick(3));
    assert!(replica.get(1).is_some());
    let mut fresh = full();
    fresh.view_version = Version(9);
    fresh.tick = Tick(4);
    fresh.revision = 1;
    assert!(matches!(
        replica.apply(fresh, Version(9)).unwrap(),
        Apply::Applied(_)
    ));
    assert_eq!(replica.revision(), Some(1));
}
#[test]
fn pipe_gpu_bridge_preserves_fraction_after_integer_origin_and_has_no_drop_bob_flag() {
    let mut replica = PacketPage::new(page(), 2).unwrap();
    let mut frame = full();
    frame.upserts.push(Pose {
        id: 2,
        item: I_ENERGY_TABLET,
        ..pose(2)
    });
    replica.apply(frame, Version(7)).unwrap();
    let mut out = [Instance::default(); 2];
    assert_eq!(
        replica
            .instances(Tick(1), 0., [16_000_000, 0, 0], &mut out)
            .unwrap(),
        2
    );
    assert!((out[0].position[0] - 2.05).abs() < 1e-6);
    assert_eq!(out[0].position[1], 2.25);
    assert_eq!(out[0].kind, 6);
    assert_eq!(out[1].kind, 7);
    assert_eq!(out[0].flags, 32 | PIPE_INSTANCE_FLAG);
    assert_eq!(out[0].limb_phase, 0.);
    assert_eq!(out[0].reserved, 0.);
    assert!(
        replica
            .instances(Tick(1), 0., [0; 3], &mut out[..1])
            .is_err()
    );
    assert!(
        replica
            .instances(Tick(1), f64::NAN, [0; 3], &mut out)
            .is_err()
    );
}
#[test]
fn five_hundred_twelve_visible_packets_rebuild_no_vertices_and_allocate_nothing_each_frame() {
    let mut replica = PacketPage::new(page(), 512).unwrap();
    let mut frame = full();
    frame.upserts = (1..=512).map(pose).collect();
    replica.apply(frame, Version(7)).unwrap();
    let mut out = vec![Instance::default(); 512];
    let scope = AllocationScope::begin().unwrap();
    for tick in 1..=1000 {
        assert_eq!(
            replica
                .instances(Tick(tick), 0.5, [16_000_000, 0, 0], &mut out)
                .unwrap(),
            512
        );
    }
    assert_eq!(scope.finish(), AllocationCounts::ZERO);
    assert_eq!(std::mem::size_of::<Instance>(), 64);
}

#[test]
fn cross_page_move_and_delivery_tombstone_prevent_duplicate_or_old_page_resurrection() {
    let a = page();
    let b = PageId { x: a.x + 1, ..a };
    let mut scene = packets::PacketScene::new(8, 1).unwrap();
    let mut first = full();
    first.tick = Tick(10);
    scene.apply(first, Version(7)).unwrap();
    let mut moved = full();
    moved.page = b;
    moved.tick = Tick(20);
    moved.upserts[0].epoch = Tick(20);
    scene.apply(moved, Version(7)).unwrap();
    assert_eq!(scene.len(), 1);
    assert_eq!(scene.get(1).unwrap().0, b);
    assert_eq!(scene.protocol_rows(), 2);
    assert_eq!(scene.page_count(), 2);
    // Delivery in B arrives before A's delayed old-page removal.
    let delivered = Frame {
        page: b,
        view_version: Version(7),
        revision: 2,
        base: Some(1),
        tick: Tick(21),
        upserts: vec![],
        removes: vec![1],
    };
    scene.apply(delivered, Version(7)).unwrap();
    assert!(scene.is_empty());
    assert_eq!(scene.retained_ids(), 1);
    assert_eq!(scene.protocol_rows(), 1);
    // A newer *protocol revision* can still describe an older logical instant.
    // It must not resurrect the delivered ID from its old page.
    let mut stale_pose = full();
    stale_pose.revision = 2;
    stale_pose.base = Some(1);
    stale_pose.tick = Tick(19);
    scene.apply(stale_pose, Version(7)).unwrap();
    assert!(scene.is_empty());
    let removed = Frame {
        page: a,
        view_version: Version(7),
        revision: 3,
        base: Some(2),
        tick: Tick(20),
        upserts: vec![],
        removes: vec![1],
    };
    scene.apply(removed, Version(7)).unwrap();
    assert_eq!(scene.retained_ids(), 0);
    assert_eq!(scene.protocol_rows(), 0);
    assert!(scene.page(a).unwrap().is_empty());
    assert!(scene.remove_page(a));
    assert!(!scene.remove_page(a));
    assert_eq!(scene.page_count(), 1);
}
#[test]
fn multipage_row_capacity_rejects_atomically_and_render_output_allocates_nothing() {
    let mut scene = packets::PacketScene::new(4, 2).unwrap();
    scene.apply(full(), Version(7)).unwrap();
    let mut second = full();
    second.page = PageId {
        x: page().x + 1,
        ..page()
    };
    second.upserts[0].id = 2;
    scene.apply(second.clone(), Version(7)).unwrap();
    let mut overflow = second.clone();
    overflow.page.x += 1;
    overflow.upserts[0].id = 3;
    assert!(scene.apply(overflow, Version(7)).is_err());
    assert_eq!(scene.len(), 2);
    assert_eq!(scene.retained_ids(), 2);
    assert_eq!(scene.page_count(), 2);
    let mut out = [Instance::default(); 2];
    let scope = AllocationScope::begin().unwrap();
    for tick in 1..=1000 {
        assert_eq!(
            scene
                .instances(Tick(tick), 0.5, [16_000_000, 0, 0], &mut out)
                .unwrap(),
            2
        );
    }
    assert_eq!(scope.finish(), AllocationCounts::ZERO);
    assert!(scene.instances(Tick(2), 0., [0; 3], &mut out[..1]).is_err());
    assert!(
        scene
            .instances(Tick(2), f64::NAN, [0; 3], &mut out)
            .is_err()
    );
    assert!(scene.remove_page(second.page));
    assert_eq!(scene.len(), 1);
    assert_eq!(scene.retained_ids(), 1);
}

use client::entities;
use client::packet_source::{PacketGpuSource, View};
struct Icons;
impl entities::ItemVisuals for Icons {
    fn visual(&self, item: u16) -> Option<entities::ItemVisual> {
        Some(entities::ItemVisual {
            kind: if item == rules::ids::STONE { 6 } else { 7 },
            atlas_layer: 1,
        })
    }
}
fn scene(count: u64) -> packets::PacketScene {
    let mut scene = packets::PacketScene::new(4, 2048).unwrap();
    scene
        .apply(
            Frame {
                page: page(),
                view_version: Version(7),
                revision: 1,
                base: None,
                tick: Tick(1),
                upserts: (1..=count)
                    .map(|id| Pose {
                        id,
                        index: LocalIdx::new(546).unwrap(),
                        item: rules::ids::STONE,
                        count: 1,
                        color: 0,
                        entry: 5,
                        exit: 3,
                        progress: 5,
                        flags: ACTIVE,
                        epoch: Tick(1),
                    })
                    .collect(),
                removes: vec![],
            },
            Version(7),
        )
        .unwrap();
    scene
}
fn view() -> View {
    View {
        space: SpaceId(0),
        tick: Tick(1),
        alpha: 0.,
        origin: [16_000_000, 0, 0],
    }
}
#[test]
fn packet_source_checks_current_raw_certificate_and_unknown_light_before_gpu_copy() {
    let scene = scene(1);
    let mut source = PacketGpuSource::new(8).unwrap();
    let work = source
        .prepare(
            &scene,
            view(),
            &|p, v, i| p == page() && v == Version(7) && i.get() == 546,
            &Icons,
            |_| None,
        )
        .unwrap();
    assert_eq!(work.instances, 1);
    assert_eq!(work.unknown_light_reads, 1);
    assert_eq!(work.cpu_vertices, 0);
    assert!((source.instances()[0].position[0] - 2.05).abs() < 1e-6);
    assert_eq!(source.instances()[0].flags, 32 | 4096);
    assert_eq!(source.lights().len(), 1);
    let work = source
        .prepare(&scene, view(), &|_, _, _| false, &Icons, |_| {
            panic!("withheld rows cannot read light")
        })
        .unwrap();
    assert_eq!(work.withheld, 1);
    assert_eq!(work.instances, 0);
    assert!(source.instances().is_empty());
}
#[test]
fn five_hundred_twelve_packets_copy_no_vertices_and_allocate_no_frame_memory() {
    let scene = scene(512);
    let mut source = PacketGpuSource::new(512).unwrap();
    let light = |_| {
        Some(world::LightSample {
            sky: 15,
            block: 0,
            rgb: [0; 3],
        })
    };
    source
        .prepare(&scene, view(), &|_, _, _| true, &Icons, light)
        .unwrap();
    let scope = AllocationScope::begin().unwrap();
    for tick in 1..=1000 {
        let work = source
            .prepare(
                &scene,
                View {
                    tick: Tick(tick),
                    ..view()
                },
                &|_, _, _| true,
                &Icons,
                light,
            )
            .unwrap();
        assert_eq!(work.instances, 512);
        assert_eq!(work.cpu_vertices, 0);
    }
    assert_eq!(scope.finish(), AllocationCounts::ZERO);
}

fn connected_raw_page() -> client::network::Connection {
    let mut connection = client::network::Connection::new(client::network::NetworkConfig {
        maximum_pages: 256,
        ..client::network::NetworkConfig::default()
    })
    .unwrap();
    connection.request_page(page(), 0).unwrap();
    connection
        .receive(
            net::ServerMessage::Page(net::PageUpdate::Snapshot(net::PageSnapshot {
                page: page(),
                version: 7,
                source_version: Version(11),
                applied_action: Default::default(),
                cells: Box::new(
                    [core_types::Cell::from_parts(rules::ids::LOGISTICAL_TRANSPORTER, 0); 4096],
                ),
            })),
            0,
        )
        .unwrap();
    connection.take_outgoing(256);
    connection
}
#[test]
fn real_connection_coalesces_a_thousand_packet_acknowledgements_and_requests_missing_base() {
    let mut connection = connected_raw_page();
    for revision in 1..=1000 {
        let mut frame = full();
        frame.revision = revision;
        frame.tick = Tick(revision);
        let bytes =
            net::encode_server(&net::ServerMessage::MachinePackets(Box::new(frame))).unwrap();
        connection
            .receive(net::decode_server(&bytes).unwrap(), revision)
            .unwrap();
    }
    assert_eq!(connection.packet_scene().len(), 1);
    assert_eq!(connection.queue_depths().outgoing, 1);
    assert!(matches!(
        connection.take_outgoing(256).as_slice(),
        [net::ClientMessage::MachinePackets(Request::Ack {
            revision: 1000,
            ..
        })]
    ));
    let mut missing = full();
    missing.revision = 1002;
    missing.base = Some(1001);
    missing.tick = Tick(1002);
    connection
        .receive(net::ServerMessage::MachinePackets(Box::new(missing)), 1002)
        .unwrap();
    assert_eq!(
        connection.packet_scene().page(page()).unwrap().revision(),
        Some(1000)
    );
    assert!(matches!(
        connection.take_outgoing(256).as_slice(),
        [net::ClientMessage::MachinePackets(Request::Full {
            have: Some(1000),
            ..
        })]
    ));
}
#[test]
fn real_connection_rejects_packet_before_mutation_when_ack_capacity_is_exhausted() {
    let mut connection = connected_raw_page();
    connection
        .receive(net::ServerMessage::MachinePackets(Box::new(full())), 1)
        .unwrap();
    connection.take_outgoing(256);
    for x in 0..255 {
        connection.request_page(PageId { x, ..page() }, 2).unwrap();
    }
    connection.request_geometry(page(), 2).unwrap();
    let mut frame = full();
    frame.revision = 2;
    frame.tick = Tick(2);
    frame.upserts[0].count = 2;
    assert_eq!(
        connection.receive(
            net::ServerMessage::MachinePackets(Box::new(frame.clone())),
            2
        ),
        Err(client::network::NetworkError::Capacity)
    );
    assert_eq!(
        connection.packet_scene().page(page()).unwrap().revision(),
        Some(1)
    );
    assert_eq!(connection.packet_scene().get(1).unwrap().1.count, 1);
    connection.take_outgoing(256);
    connection
        .receive(net::ServerMessage::MachinePackets(Box::new(frame)), 2)
        .unwrap();
    assert_eq!(connection.packet_scene().get(1).unwrap().1.count, 2);
}
