//! A public Raw watermark repairs accepted edits without dropping a usable base.
use client::network::{Connection, NetworkConfig};
use core_types::{BlockPos, Cell, PAGE_VOLUME, PageId, SpaceId, Tick, Version};
use net::{
    Action, ActionId, ActionReply, ActionStatus, ClientMessage, PageDelta, PageSnapshot,
    PageUpdate, PlayerState, ReplicaRead, ServerMessage, Welcome,
};

#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);

fn position(x: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x: x * 16 + 3,
        y: 4,
        z: 5,
    }
}
fn connection() -> Connection {
    let mut c = Connection::new(NetworkConfig::default()).unwrap();
    c.receive(
        ServerMessage::Welcome(Welcome {
            session: 1,
            identity: Default::default(),
            new_credential: None,
            role: net::Role::Player,
            tick: Tick(0),
            next_action: ActionId(1),
            public_seed: 1,
            generator_version: terrain_gen::GENERATOR_VERSION,
            rules_version: net::RULES_VERSION,
            player: PlayerState {
                health: 20.,
                position: [8.5, 1., 8.5],
                ..Default::default()
            },
        }),
        0,
    )
    .unwrap();
    c
}
fn raw(page: PageId, version: u64, applied_action: ActionId, cell: Cell) -> ServerMessage {
    ServerMessage::Page(PageUpdate::Snapshot(PageSnapshot {
        page,
        version,
        source_version: Version(version),
        applied_action,
        cells: Box::new([cell; PAGE_VOLUME]),
    }))
}
fn predict(c: &mut Connection, x: i32) -> (BlockPos, ActionId) {
    let p = position(x);
    c.request_page(p.page(), 0).unwrap();
    c.take_outgoing(256);
    c.receive(raw(p.page(), 1, ActionId(0), Cell::AIR), 1)
        .unwrap();
    c.take_outgoing(256);
    let id = c
        .action(
            Action::Place {
                position: p,
                expected: Cell::AIR,
                cell: Cell::from_parts(rules::ids::STONE, 0),
            },
            true,
            2,
        )
        .unwrap();
    (p, id)
}
fn accept(c: &mut Connection, position: BlockPos, id: ActionId) {
    c.receive(
        ServerMessage::Action(ActionReply {
            id,
            status: ActionStatus::Accepted,
            transaction: Some(core_types::TxId(9000)),
            // This private transaction value cannot acknowledge a public image.
            source_version: Version(9000),
            position: Some(position),
            cell: Some(Cell::from_parts(rules::ids::STONE, 0)),
        }),
        3,
    )
    .unwrap();
}
fn requests(messages: &[ClientMessage]) -> Vec<(PageId, Option<u64>)> {
    messages
        .iter()
        .filter_map(|message| match message {
            ClientMessage::RequestPage { page, acknowledged } => Some((*page, *acknowledged)),
            _ => None,
        })
        .collect()
}

#[test]
fn accepted_edit_polls_at_500ms_with_retained_base_then_retires_on_watermark() {
    let mut c = connection();
    let (p, id) = predict(&mut c, 0);
    accept(&mut c, p, id);
    assert_eq!(c.predicted_edits(), 1);
    assert!(!c.action_pending(id));
    assert!(c.take_outgoing(256).is_empty());
    c.poll(499).unwrap();
    assert!(c.take_outgoing(256).is_empty());
    c.poll(500).unwrap();
    assert_eq!(requests(&c.take_outgoing(256)), [(p.page(), Some(1))]);
    assert_eq!(c.replica().page(p.page()).unwrap().version, 1);
    assert_eq!(
        c.replica().page(p.page()).unwrap().applied_action,
        ActionId(0)
    );
    c.poll(999).unwrap();
    assert!(c.take_outgoing(256).is_empty());
    // An accepted place may have been overwritten before any Raw publication.
    c.receive(raw(p.page(), 2, id, Cell::AIR), 1000).unwrap();
    assert_eq!(c.predicted_edits(), 0);
    assert_eq!(c.read(p), ReplicaRead::Known(Cell::AIR));
    c.take_outgoing(256);
    c.poll(1500).unwrap();
    assert!(c.take_outgoing(256).is_empty());
}

#[test]
fn causal_poll_caps_four_distinct_pages_and_defers_a_full_outbox_without_mutation() {
    let mut c = connection();
    let mut pages = Vec::new();
    for x in 0..6 {
        let (p, id) = predict(&mut c, x);
        accept(&mut c, p, id);
        pages.push(p.page());
    }
    c.take_outgoing(256);
    c.poll(500).unwrap();
    let first = requests(&c.take_outgoing(256));
    assert_eq!(first.len(), 4);
    assert!(first.iter().all(|(_, base)| *base == Some(1)));
    c.poll(500).unwrap();
    let second = requests(&c.take_outgoing(256));
    assert_eq!(second.len(), 2);
    assert!(
        second
            .iter()
            .all(|(page, _)| !first.iter().any(|(p, _)| p == page))
    );
    for x in 0..256 {
        c.request_height(
            net::height::HeightKey {
                space: 0,
                level: 0,
                x,
                z: 0,
            },
            1000,
        )
        .unwrap();
    }
    assert_eq!(c.queue_depths().outgoing, 256);
    let scope = telemetry::AllocationScope::begin().unwrap();
    c.poll(1000).unwrap();
    let counts = scope.finish();
    assert_eq!(counts.allocating_calls(), 0);
    assert_eq!(counts.dealloc_calls, 0);
    assert_eq!(c.queue_depths().outgoing, 256);
    assert_eq!(c.predicted_edits(), 6);
    for page in &pages {
        assert_eq!(c.replica().page(*page).unwrap().version, 1);
    }
    let full = c.take_outgoing(256);
    assert!(requests(&full).is_empty());
    // A failed admission cannot advance the retry clock: draining enables the
    // same timestamp to request four images immediately.
    c.poll(1000).unwrap();
    assert_eq!(requests(&c.take_outgoing(256)).len(), 4);
}

#[test]
fn queued_full_repair_takes_priority_over_retained_base_causal_poll() {
    let mut c = connection();
    let (p, id) = predict(&mut c, 0);
    accept(&mut c, p, id);
    c.take_outgoing(256);
    c.receive(
        ServerMessage::Page(PageUpdate::Delta(PageDelta {
            page: p.page(),
            base: 9,
            version: 10,
            source_version: Version(10),
            applied_action: ActionId(0),
            changes: vec![],
        })),
        4,
    )
    .unwrap();
    c.poll(500).unwrap();
    assert_eq!(requests(&c.take_outgoing(256)), [(p.page(), None)]);
    assert_eq!(c.replica().page(p.page()).unwrap().version, 1);
    assert_eq!(c.predicted_edits(), 1);
}

#[test]
fn raw_and_reply_in_either_order_retire_only_after_both_public_watermark_and_acceptance() {
    for raw_first in [false, true] {
        let mut c = connection();
        let (p, id) = predict(&mut c, 0);
        let actual = Cell::from_parts(rules::ids::DIRT, 0);
        if raw_first {
            c.receive(raw(p.page(), 2, id, actual), 3).unwrap();
            assert_eq!(c.predicted_edits(), 1);
            accept(&mut c, p, id);
        } else {
            accept(&mut c, p, id);
            assert_eq!(c.predicted_edits(), 1);
            c.receive(raw(p.page(), 2, id, actual), 4).unwrap();
        }
        assert_eq!(c.predicted_edits(), 0);
        assert!(!c.action_pending(id));
        assert_eq!(c.read(p), ReplicaRead::Known(actual));
        c.take_outgoing(256);
        c.poll(1000).unwrap();
        assert!(requests(&c.take_outgoing(256)).is_empty());
    }
}
