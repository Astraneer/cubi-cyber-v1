//! Unsent retries share one FIFO position; terminal authority replies retire them.
use client::network::{Connection, NetworkConfig};
use core_types::{Tick, Version};
use net::{
    Action, ActionId, ActionReply, ActionStatus, ClientMessage, PlayerState, RejectReason,
    ServerMessage, Welcome,
};
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
fn reply(c: &mut Connection, id: ActionId, status: ActionStatus) {
    c.receive(
        ServerMessage::Action(ActionReply {
            id,
            status,
            transaction: None,
            source_version: Version(1),
            position: None,
            cell: None,
        }),
        1000,
    )
    .unwrap();
}
fn ids(messages: &[ClientMessage]) -> Vec<ActionId> {
    messages
        .iter()
        .filter_map(|m| match m {
            ClientMessage::Action { id, .. } | ClientMessage::AimedAction { id, .. } => Some(*id),
            _ => None,
        })
        .collect()
}
#[test]
fn repeated_poll_retains_one_unsent_rpc_per_id_and_preserves_action_order_and_aim() {
    let mut c = connection();
    let a = c.action(Action::Chat("first".into()), false, 0).unwrap();
    let b = c
        .aimed_action(Action::Chat("second".into()), false, 0, 45., -10.)
        .unwrap();
    for time in [250, 500, 750] {
        c.poll(time).unwrap();
    }
    let messages = c.take_outgoing(256);
    assert_eq!(ids(&messages), [a, b]);
    assert!(
        matches!(&messages[1],ClientMessage::AimedAction{yaw,pitch,..} if *yaw==45.&&*pitch== -10.)
    );
    // Once handed to transport, retry remains at-least-once until authority ACK.
    c.poll(1000).unwrap();
    c.poll(1250).unwrap();
    assert_eq!(ids(&c.take_outgoing(256)), [a, b]);
}
#[test]
fn terminal_reply_removes_only_its_unsent_retry_and_pending_keeps_delivery_live() {
    let mut c = connection();
    let a = c.action(Action::Chat("first".into()), false, 0).unwrap();
    let b = c.action(Action::Chat("second".into()), false, 0).unwrap();
    c.take_outgoing(256);
    c.poll(250).unwrap();
    reply(&mut c, a, ActionStatus::Pending);
    assert_eq!(c.queue_depths().outgoing, 2);
    assert!(c.action_pending(a));
    reply(&mut c, a, ActionStatus::Accepted);
    assert!(!c.action_pending(a));
    assert!(c.action_pending(b));
    assert_eq!(c.queue_depths().outgoing, 1);
    reply(&mut c, a, ActionStatus::Accepted);
    assert_eq!(c.counters().accepted_actions, 1);
    assert_eq!(ids(&c.take_outgoing(256)), [b]);
    c.poll(500).unwrap();
    reply(&mut c, b, ActionStatus::Rejected(RejectReason::Protected));
    assert!(!c.action_pending(b));
    assert!(c.take_outgoing(256).is_empty());
    assert_eq!(c.counters().rejected_actions, 1);
}
#[test]
fn retry_at_full_outbox_does_not_require_capacity_and_terminal_reply_frees_exactly_one_slot() {
    let mut c = connection();
    let a = c.action(Action::Chat("one".into()), false, 0).unwrap();
    for x in 0..255 {
        c.request_height(
            net::height::HeightKey {
                space: 0,
                level: 0,
                x,
                z: 0,
            },
            0,
        )
        .unwrap();
    }
    assert_eq!(c.queue_depths().outgoing, 256);
    assert!(!c.can_queue_action());
    c.poll(250).unwrap();
    assert_eq!(c.queue_depths().outgoing, 256);
    reply(&mut c, a, ActionStatus::Accepted);
    assert_eq!(c.queue_depths().outgoing, 255);
    assert!(c.can_queue_action());
    let b = c.action(Action::Chat("two".into()), false, 251).unwrap();
    let messages = c.take_outgoing(256);
    assert_eq!(messages.len(), 256);
    assert_eq!(ids(&messages), [b]);
}
