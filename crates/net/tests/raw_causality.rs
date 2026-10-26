use core_types::{BlockPos, Cell, PAGE_VOLUME, PageId, SpaceId, Version};
use net::{
    ActionId, ActionReply, ActionStatus, ApplyResult, PageDelta, PageSnapshot, PageUpdate,
    PredictionOverlay, Replica, ReplicaError, ReplicaRead, ViewHistory,
};

fn page() -> PageId {
    PageId {
        space: SpaceId(0),
        x: 0,
        y: 0,
        z: 0,
    }
}
fn position() -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x: 3,
        y: 4,
        z: 5,
    }
}
fn cells() -> Box<[Cell; PAGE_VOLUME]> {
    Box::new(std::array::from_fn(|i| Cell::from_raw((i % 4) as u32)))
}
fn accepted(id: u64) -> ActionReply {
    ActionReply {
        id: ActionId(id),
        status: ActionStatus::Accepted,
        transaction: Some(core_types::TxId(9000)),
        source_version: Version(9000),
        position: Some(position()),
        cell: Some(Cell::from_raw(9)),
    }
}
fn snapshot(update: PageUpdate) -> PageSnapshot {
    match update {
        PageUpdate::Snapshot(p) => p,
        _ => panic!("full image required"),
    }
}

#[test]
fn accepted_external_overwrite_retires_in_both_raw_reply_orders_without_comparing_private_versions()
{
    for raw_first in [false, true] {
        let mut history = ViewHistory::new(1, 4);
        let initial = snapshot(
            history
                .build_public(page(), ActionId(0), cells(), false)
                .unwrap()
                .unwrap(),
        );
        let mut replica = Replica::new(1);
        replica
            .apply(PageUpdate::Snapshot(initial.clone()))
            .unwrap();
        let mut overlay = PredictionOverlay::new(4);
        overlay
            .predict(ActionId(11), position(), Cell::from_raw(9))
            .unwrap();
        let mut overwritten = cells();
        overwritten[usize::from(position().local_index().get())] = Cell::from_raw(7);
        let truth = history
            .build_public(page(), ActionId(11), overwritten, true)
            .unwrap()
            .unwrap();
        assert!(truth_source(&truth) < accepted(11).source_version);
        if raw_first {
            replica.apply(truth).unwrap();
            overlay.retire_applied(&replica);
            assert_eq!(
                overlay.len(),
                1,
                "Raw watermark alone cannot turn an unknown action result into accepted"
            );
            overlay.reply(accepted(11));
        } else {
            overlay.reply(accepted(11));
            overlay.retire_applied(&replica);
            assert_eq!(overlay.len(), 1, "an ACK is not an authoritative Raw image");
            replica.apply(truth).unwrap();
        }
        overlay.retire_applied(&replica);
        assert!(overlay.is_empty());
        assert_eq!(
            overlay.read(position(), &replica),
            ReplicaRead::Known(Cell::from_raw(7))
        );
    }
}
fn truth_source(update: &PageUpdate) -> Version {
    match update {
        PageUpdate::Snapshot(p) => p.source_version,
        PageUpdate::Delta(p) => p.source_version,
    }
}

#[test]
fn own_aba_coalescing_publishes_watermark_without_body_changes_and_missing_base_does_not_retire() {
    let mut history = ViewHistory::new(1, 2);
    let first = snapshot(
        history
            .build_public(page(), ActionId(0), cells(), false)
            .unwrap()
            .unwrap(),
    );
    assert!(history.acknowledge(page(), first.version));
    let mut overlay = PredictionOverlay::new(4);
    overlay
        .predict(ActionId(1), position(), Cell::from_raw(9))
        .unwrap();
    overlay.reply(accepted(1));
    overlay
        .predict(ActionId(2), position(), Cell::from_raw(3))
        .unwrap();
    let mut reply = accepted(2);
    reply.cell = Some(Cell::from_raw(3));
    overlay.reply(reply);
    let update = history
        .build_public(page(), ActionId(2), cells(), false)
        .unwrap()
        .unwrap();
    let PageUpdate::Delta(delta) = &update else {
        panic!("metadata-only delta should be smaller than this paletted image")
    };
    assert!(delta.changes.is_empty());
    assert_eq!(delta.applied_action, ActionId(2));
    let mut lost = Replica::new(1);
    assert!(matches!(
        lost.apply(update),
        Err(ReplicaError::MissingBase { .. })
    ));
    overlay.retire_applied(&lost);
    assert_eq!(overlay.len(), 2);
    let repaired = history
        .build_public(page(), ActionId(2), cells(), true)
        .unwrap()
        .unwrap();
    assert_eq!(lost.apply(repaired), Ok(ApplyResult::Applied));
    overlay.retire_applied(&lost);
    assert!(overlay.is_empty());
    assert_eq!(lost.page(page()).unwrap().cells, first.cells);
    // A new session starts public view revisions at1 but retains this identity's
    // durable action prefix. No old global SourceVersion comparison is involved.
    let mut reconnected_history = ViewHistory::new(1, 2);
    let reconnect = snapshot(
        reconnected_history
            .build_public(page(), ActionId(2), cells(), false)
            .unwrap()
            .unwrap(),
    );
    assert_eq!(reconnect.version, 1);
    assert_eq!(reconnect.applied_action, ActionId(2));
    let mut reconnected = Replica::new(1);
    reconnected.apply(PageUpdate::Snapshot(reconnect)).unwrap();
    overlay
        .predict(ActionId(2), position(), Cell::from_raw(9))
        .unwrap();
    overlay.reply(accepted(2));
    overlay.retire_applied(&reconnected);
    assert!(overlay.is_empty());
}

#[test]
fn public_history_preserves_header_on_no_change_and_watermarks_never_regress() {
    let mut history = ViewHistory::new(1, 2);
    let first = snapshot(
        history
            .build_public(page(), ActionId(3), cells(), false)
            .unwrap()
            .unwrap(),
    );
    assert_eq!(first.source_version, Version(first.version));
    history.acknowledge(page(), first.version);
    assert!(
        history
            .build_public(page(), ActionId(3), cells(), false)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        history
            .build_public(page(), ActionId(3), cells(), true)
            .unwrap(),
        Some(PageUpdate::Snapshot(first.clone()))
    );
    assert_eq!(
        history.build_public(page(), ActionId(2), cells(), false),
        Err(ReplicaError::InvalidVersion)
    );
    let mut replica = Replica::new(1);
    replica.apply(PageUpdate::Snapshot(first.clone())).unwrap();
    let mut bad = first.clone();
    bad.version += 1;
    bad.applied_action = ActionId(2);
    assert_eq!(
        replica.apply(PageUpdate::Snapshot(bad)),
        Err(ReplicaError::InvalidVersion)
    );
    assert_eq!(
        replica.apply(PageUpdate::Delta(PageDelta {
            page: page(),
            base: first.version,
            version: first.version + 1,
            source_version: Version(2),
            applied_action: ActionId(2),
            changes: vec![]
        })),
        Err(ReplicaError::InvalidVersion)
    );
    assert_eq!(replica.page(page()), Some(&first));
}

#[test]
fn raw_watermark_layout_has_distinct_tags_and_legacy_raw_is_rejected_without_changing_envelope() {
    let image = PageSnapshot {
        page: page(),
        version: 5,
        source_version: Version(5),
        applied_action: ActionId(17),
        cells: Box::new([Cell::AIR; PAGE_VOLUME]),
    };
    let bytes = net::encode_server(&net::ServerMessage::Page(PageUpdate::Snapshot(
        image.clone(),
    )))
    .unwrap();
    assert_eq!(&bytes[..6], b"CUBI\x01\x00");
    assert_eq!(bytes[18], 28);
    assert_eq!(u64::from_le_bytes(bytes[51..59].try_into().unwrap()), 17);
    assert_eq!(
        net::decode_server(&bytes).unwrap(),
        net::ServerMessage::Page(PageUpdate::Snapshot(image))
    );
    for tag in [3, 4] {
        let body = [tag];
        let checksum = body.iter().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
        });
        let mut frame = b"CUBI\x01\x00".to_vec();
        frame.extend(1u32.to_le_bytes());
        frame.extend(checksum.to_le_bytes());
        frame.extend(body);
        assert_eq!(net::decode_server(&frame), Err(net::CodecError::Version));
    }
}
