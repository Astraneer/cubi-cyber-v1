use core_types::{BlockPos, Cell, PAGE_VOLUME, PageId, SpaceId, Tick, Version};
use net::*;

fn page(x: i32) -> PageId {
    PageId {
        space: SpaceId(0),
        x,
        y: 0,
        z: 0,
    }
}
fn position() -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x: 1,
        y: 2,
        z: 3,
    }
}

#[test]
fn all_client_messages_roundtrip_and_secrets_are_redacted() {
    let token = SecretToken::new([93; 32]);
    let messages = [
        ClientMessage::Hello(Hello {
            identity: PlayerId([4; 16]),
            name: "Игрок".into(),
            credential: Some(token),
        }),
        ClientMessage::Inputs(vec![InputFrame {
            sequence: 1,
            target_tick: Tick(17),
            keys: 255,
            yaw: 45.0,
            pitch: -20.0,
        }]),
        ClientMessage::Action {
            id: ActionId(1),
            action: Action::Mine {
                position: position(),
                expected: Cell::from_parts(3, 0),
                tool: 271,
            },
        },
        ClientMessage::Action {
            id: ActionId(2),
            action: Action::Place {
                position: position(),
                expected: Cell::AIR,
                cell: Cell::from_parts(9, 2),
            },
        },
        ClientMessage::Action {
            id: ActionId(3),
            action: Action::Flight(true),
        },
        ClientMessage::Action {
            id: ActionId(4),
            action: Action::DebugNoclip(false),
        },
        ClientMessage::Action {
            id: ActionId(5),
            action: Action::Command("/gamemode creative".into()),
        },
        ClientMessage::Action {
            id: ActionId(6),
            action: Action::SelectSlot(8),
        },
        ClientMessage::Action {
            id: ActionId(7),
            action: Action::PlaceOwned {
                position: position(),
                expected: Cell::AIR,
                cell: Cell::from_parts(3, 0),
                inventory: InventoryGuard {
                    revision: 4,
                    selected: 0,
                    item: 3,
                    count: 2,
                    damage: 0,
                    payload: 0,
                },
            },
        },
        ClientMessage::AimedAction {
            id: ActionId(8),
            action: Action::StopMining,
            yaw: 271.5,
            pitch: -30.0,
        },
        ClientMessage::RequestPage {
            page: page(0),
            acknowledged: Some(4),
        },
        ClientMessage::AckPage {
            page: page(0),
            version: 4,
        },
        ClientMessage::RequestSummary { page: page(9) },
        ClientMessage::Ping(44),
    ];
    for message in messages {
        assert_eq!(
            decode_client(&encode_client(&message).unwrap()).unwrap(),
            message
        );
    }
    assert_eq!(format!("{token:?}"), "SecretToken([redacted])");
}

#[test]
fn owner_inventory_and_public_prediction_frames_are_bounded() {
    let mut data = vec![0; 260];
    data[..4].copy_from_slice(b"INV1");
    let message = ServerMessage::Inventory { data };
    assert_eq!(
        decode_server(&encode_server(&message).unwrap()).unwrap(),
        message
    );
    assert!(
        encode_server(&ServerMessage::Inventory {
            data: vec![0; 1025]
        })
        .is_err()
    );
    let message = ServerMessage::Prediction {
        tick: Tick(7),
        player: PlayerState::default(),
        continuation: b"PRDT\x01\0".to_vec(),
    };
    assert_eq!(
        decode_server(&encode_server(&message).unwrap()).unwrap(),
        message
    );
    assert!(
        encode_server(&ServerMessage::Prediction {
            tick: Tick(7),
            player: PlayerState::default(),
            continuation: vec![0; MAX_PREDICTION_BYTES + 1]
        })
        .is_err()
    );
    assert!(
        encode_client(&ClientMessage::Action {
            id: ActionId(1),
            action: Action::SelectSlot(9)
        })
        .is_err()
    );
}

#[test]
fn snapshots_roundtrip_uniform_paletted_and_dense_storage() {
    for distinct in [1, 2, 3, 16, 256, 257, 4096] {
        let cells = Box::new(std::array::from_fn(|index| {
            Cell::from_raw((index % distinct) as u32)
        }));
        let message = ServerMessage::Page(PageUpdate::Snapshot(PageSnapshot {
            page: page(-3),
            version: 1,
            source_version: Version(5),
            cells,
        }));
        let bytes = encode_server(&message).unwrap();
        assert_eq!(decode_server(&bytes).unwrap(), message);
        if distinct == 1 {
            assert!(bytes.len() < 64);
        }
        if distinct == 2 {
            assert!(bytes.len() < 600);
        }
    }
}

#[test]
fn malformed_and_hostile_frames_reject_without_panicking() {
    let good = encode_client(&ClientMessage::Ping(3)).unwrap();
    for len in 0..good.len() {
        assert!(decode_client(&good[..len]).is_err());
    }
    let mut corrupt = good.clone();
    corrupt[20] ^= 1;
    assert_eq!(decode_client(&corrupt), Err(CodecError::Checksum));
    assert!(
        encode_client(&ClientMessage::Inputs(vec![InputFrame {
            sequence: 1,
            yaw: f32::NAN,
            ..Default::default()
        }]))
        .is_err()
    );
    assert!(
        encode_client(&ClientMessage::Inputs(vec![
            InputFrame {
                sequence: 2,
                ..Default::default()
            },
            InputFrame {
                sequence: 1,
                ..Default::default()
            }
        ]))
        .is_err()
    );
    let delta = PageDelta {
        page: page(0),
        base: 1,
        version: 2,
        source_version: Version(2),
        changes: vec![(3, Cell::AIR), (3, Cell::AIR)],
    };
    assert!(encode_server(&ServerMessage::Page(PageUpdate::Delta(delta))).is_err());
    let mut rng = core_types::DeterministicRng::from_seed(41);
    for len in 0..512 {
        let bytes: Vec<_> = (0..len).map(|_| rng.next_u64() as u8).collect();
        let _ = decode_client(&bytes);
        let _ = decode_server(&bytes);
    }
}

#[test]
fn replication_uses_confirmed_base_and_falls_back_when_history_is_gone() {
    let mut history = ViewHistory::new(4, 2);
    let mut replica = Replica::new(4);
    let first = history
        .build(
            page(0),
            Version(1),
            Box::new([Cell::from_raw(3); PAGE_VOLUME]),
            false,
        )
        .unwrap()
        .unwrap();
    let PageUpdate::Snapshot(snapshot) = &first else {
        panic!("initial snapshot")
    };
    let base = snapshot.version;
    replica.apply(first).unwrap();
    assert!(history.acknowledge(page(0), base));
    let mut cells = Box::new([Cell::from_raw(3); PAGE_VOLUME]);
    cells[7] = Cell::AIR;
    let delta = history
        .build(page(0), Version(2), cells.clone(), false)
        .unwrap()
        .unwrap();
    let PageUpdate::Delta(delta_data) = &delta else {
        panic!("one-cell delta")
    };
    assert_eq!(delta_data.base, base);
    assert_eq!(delta_data.changes.len(), 1);
    replica.apply(delta).unwrap();
    // The peer did not acknowledge the second image. Evicting its confirmed base
    // must produce a full image instead of a delta against the unconfirmed send.
    cells[8] = Cell::AIR;
    let fallback = history
        .build(page(0), Version(3), cells.clone(), false)
        .unwrap()
        .unwrap();
    assert!(matches!(fallback, PageUpdate::Snapshot(_)));
    replica.apply(fallback).unwrap();
    assert_eq!(replica.page(page(0)).unwrap().cells, cells);
    let bad = PageUpdate::Delta(PageDelta {
        page: page(0),
        base: 999,
        version: 1000,
        source_version: Version(7),
        changes: vec![(1, Cell::AIR)],
    });
    let old = replica.page(page(0)).unwrap().clone();
    assert!(matches!(
        replica.apply(bad),
        Err(ReplicaError::MissingBase { .. })
    ));
    assert_eq!(replica.page(page(0)), Some(&old));
}

#[test]
fn prediction_waits_for_base_confirmation_and_reject_rolls_back() {
    let mut replica = Replica::new(1);
    replica
        .apply(PageUpdate::Snapshot(PageSnapshot {
            page: page(0),
            version: 1,
            source_version: Version(1),
            cells: Box::new([Cell::from_raw(3); PAGE_VOLUME]),
        }))
        .unwrap();
    let p = position();
    let mut overlay = PredictionOverlay::new(4);
    overlay.predict(ActionId(1), p, Cell::AIR).unwrap();
    assert_eq!(overlay.read(p, &replica), ReplicaRead::Known(Cell::AIR));
    overlay.reply(ActionReply {
        id: ActionId(1),
        status: ActionStatus::Rejected(RejectReason::Protected),
        transaction: None,
        source_version: Version(1),
        position: Some(p),
        cell: None,
    });
    assert_eq!(
        overlay.read(p, &replica),
        ReplicaRead::Known(Cell::from_raw(3))
    );
    overlay.predict(ActionId(2), p, Cell::AIR).unwrap();
    overlay.reply(ActionReply {
        id: ActionId(2),
        status: ActionStatus::Accepted,
        transaction: Some(core_types::TxId(2)),
        source_version: Version(2),
        position: Some(p),
        cell: Some(Cell::AIR),
    });
    overlay.retire_applied(&replica);
    assert_eq!(overlay.len(), 1);
    replica
        .apply(PageUpdate::Delta(PageDelta {
            page: page(0),
            base: 1,
            version: 2,
            source_version: Version(2),
            changes: vec![(p.local_index().get(), Cell::AIR)],
        }))
        .unwrap();
    overlay.retire_applied(&replica);
    assert!(overlay.is_empty());
}

#[test]
fn actual_view_reveal_and_aoi_hysteresis_do_not_require_regeneration() {
    let mut history = ViewHistory::new(2, 4);
    let first = history
        .build(
            page(0),
            Version(9),
            Box::new([Cell::from_raw(3); PAGE_VOLUME]),
            false,
        )
        .unwrap()
        .unwrap();
    let PageUpdate::Snapshot(first) = first else {
        unreachable!()
    };
    history.acknowledge(page(0), first.version);
    let mut cells = first.cells;
    cells[1] = Cell::from_raw(46);
    let update = history
        .build(page(0), Version(9), cells, false)
        .unwrap()
        .unwrap();
    let PageUpdate::Delta(delta) = update else {
        panic!("visibility delta")
    };
    assert_eq!(delta.source_version, Version(9));
    assert!(delta.version > first.version);
    assert_eq!(delta.changes, [(1, Cell::from_raw(46))]);
    assert!(!aoi_contains(page(0), page(3), 2, 4, false));
    assert!(aoi_contains(page(0), page(3), 2, 4, true));
    assert!(!aoi_contains(page(0), page(5), 2, 4, true));
}

#[test]
fn authenticated_udp_recovers_loss_jitter_reorder_and_suppresses_duplicates() {
    use net::{emulator::*, udp::*};
    let token = SecretToken::new([17; 32]);
    let mut client = Channel::new(7, token, Side::Client, Limits::default());
    let mut server = Channel::new(7, token, Side::Server, Limits::default());
    let mut outbound =
        SimulatedLink::new(NetworkProfile::default(), 1234, 4096, 4 * MAX_FRAME_BYTES);
    let mut inbound =
        SimulatedLink::new(NetworkProfile::default(), 5678, 4096, 4 * MAX_FRAME_BYTES);
    let frame: Vec<_> = (0..100_000).map(|i| (i * 17) as u8).collect();
    for packet in client.send(frame.clone(), true, 0).unwrap() {
        assert!(packet.len() <= MAX_DATAGRAM);
        outbound.send(0, packet.len(), packet).unwrap();
    }
    let mut received = Vec::new();
    for now in (0..4000).step_by(5) {
        for packet in client.tick(now).unwrap() {
            outbound.send(now, packet.len(), packet).unwrap();
        }
        while let Some(packet) = outbound.receive(now) {
            let result = server.receive(&packet, now).unwrap();
            received.extend(result.frames);
            for reply in result.replies {
                inbound.send(now, reply.len(), reply).unwrap();
            }
        }
        while let Some(packet) = inbound.receive(now) {
            assert!(client.receive(&packet, now).unwrap().frames.is_empty());
        }
        if client.pending_messages() == 0 {
            break;
        }
    }
    assert_eq!(received, [frame]);
    assert_eq!(client.pending_messages(), 0);
    assert!(outbound.counters().lost > 0);
    assert!(client.counters().retransmitted_bytes > 0);
    let packet = client.send(vec![1, 2, 3], false, 5000).unwrap().remove(0);
    assert_eq!(
        client.receive(&packet, 5001).err(),
        Some(UdpError::Authentication)
    );
    let mut corrupt = packet;
    corrupt[40] ^= 1;
    assert_eq!(
        server.receive(&corrupt, 5001).err(),
        Some(UdpError::Authentication)
    );
}

#[test]
fn simulated_link_and_pending_histories_apply_backpressure() {
    use net::emulator::*;
    let mut link = SimulatedLink::new(
        NetworkProfile {
            delay_ms: 10,
            jitter_ms: 0,
            loss_percent: 0,
        },
        1,
        1,
        8,
    );
    link.send(0, 8, 1).unwrap();
    assert!(link.send(0, 1, 2).is_err());
    assert_eq!(link.receive(9), None);
    assert_eq!(link.receive(10), Some(1));
    let mut history = InputHistory::new(2);
    history
        .push(InputFrame {
            sequence: 1,
            ..Default::default()
        })
        .unwrap();
    history
        .push(InputFrame {
            sequence: 2,
            ..Default::default()
        })
        .unwrap();
    assert!(
        history
            .push(InputFrame {
                sequence: 3,
                ..Default::default()
            })
            .is_err()
    );
    history.acknowledge(1);
    history
        .push(InputFrame {
            sequence: 3,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        history
            .pending()
            .map(|frame| frame.sequence)
            .collect::<Vec<_>>(),
        [2, 3]
    );
    let _ = Tick(0);
}

#[test]
fn aimed_actions_reject_nonfinite_or_out_of_range_angles() {
    for (yaw, pitch) in [
        (f32::NAN, 0.0),
        (f32::INFINITY, 0.0),
        (0.0, 91.0),
        (0.0, f32::NEG_INFINITY),
    ] {
        assert!(
            encode_client(&ClientMessage::AimedAction {
                id: ActionId(1),
                action: Action::StopMining,
                yaw,
                pitch
            })
            .is_err()
        );
    }
}

#[test]
fn unchanged_public_cells_hide_private_revisions_and_repair_the_same_header() {
    let mut history = ViewHistory::new(1, 4);
    let cells = Box::new([Cell::from_raw(3); PAGE_VOLUME]);
    let first = history
        .build(page(0), Version(9), cells.clone(), false)
        .unwrap()
        .unwrap();
    let PageUpdate::Snapshot(first) = first else {
        panic!("initial image")
    };
    assert!(history.acknowledge(page(0), first.version));
    assert!(
        history
            .build(page(0), Version(500), cells.clone(), false)
            .unwrap()
            .is_none()
    );
    let repair = history
        .build(page(0), Version(500), cells.clone(), true)
        .unwrap()
        .unwrap();
    assert_eq!(repair, PageUpdate::Snapshot(first.clone()));
    assert_eq!(history.latest_page(page(0)), Some(&first));
    let mut visible = cells;
    visible[7] = Cell::AIR;
    let changed = history
        .build(page(0), Version(501), visible, false)
        .unwrap()
        .unwrap();
    let PageUpdate::Delta(changed) = changed else {
        panic!("visible delta")
    };
    assert_eq!(changed.base, first.version);
    assert_eq!(changed.source_version, Version(501));
    assert_eq!(changed.changes, [(7, Cell::AIR)]);
}
