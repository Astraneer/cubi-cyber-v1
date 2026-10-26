//! Actual shared UseInput routing, with bounded retained presses under transport
//! pressure. No health, inventory or World changes are invented by the client.
use client::{
    ClientScene, SceneConfig, network::NetworkConfig, remote::RemoteSession, view::RemoteInput,
};
use core_types::{Cell, PageId, SpaceId, Tick, Version};
use net::{Action, ActionId, ClientMessage, PlayerState, ServerMessage, Welcome};
fn session(item: u16, health: f32) -> (RemoteSession, ClientScene) {
    session_with_id(item, health, 1)
}
fn session_with_id(item: u16, health: f32, session_id: u64) -> (RemoteSession, ClientScene) {
    let mut session = RemoteSession::new(NetworkConfig::default()).unwrap();
    session
        .connection_mut()
        .receive(
            ServerMessage::Welcome(Welcome {
                session: session_id,
                identity: Default::default(),
                new_credential: None,
                role: net::Role::Player,
                tick: Tick(0),
                next_action: ActionId(1),
                public_seed: 1,
                generator_version: terrain_gen::GENERATOR_VERSION,
                rules_version: net::RULES_VERSION,
                player: PlayerState {
                    health,
                    position: [8.5, 1., 8.5],
                    ..Default::default()
                },
            }),
            0,
        )
        .unwrap();
    let mut actual = sim::Player::new([8.5, 1., 8.5]);
    actual.health = health;
    session
        .connection_mut()
        .receive(
            ServerMessage::Prediction {
                tick: Tick(0),
                player: PlayerState {
                    health,
                    position: actual.pos,
                    velocity: actual.vel,
                    yaw: actual.yaw,
                    pitch: actual.pitch,
                    on_ground: actual.on_ground,
                    flying: actual.flying,
                    noclip: actual.noclip,
                    mode: actual.mode as u8,
                    ..Default::default()
                },
                continuation: sim::prediction::PredictionState::capture(&actual)
                    .unwrap()
                    .as_bytes()
                    .to_vec(),
            },
            0,
        )
        .unwrap();
    let mut inventory = rules::survival::Inventory::empty();
    inventory
        .set_trusted(0, rules::stack::ItemStack::new(item, 2).unwrap())
        .unwrap();
    session
        .connection_mut()
        .receive(
            ServerMessage::Inventory {
                data: inventory.encode().to_vec(),
            },
            0,
        )
        .unwrap();
    let mut scene = ClientScene::new(SceneConfig {
        horizontal_radius: 0,
        vertical_radius: 0,
        max_loaded_pages: 27,
        authoritative_remote: true,
        ..SceneConfig::forest()
    })
    .unwrap();
    session
        .pump_scene(&mut scene, RemoteInput::default(), 0)
        .unwrap();
    session.connection_mut().take_outgoing(256);
    (session, scene)
}
fn actions(session: &mut RemoteSession) -> Vec<Action> {
    session
        .connection_mut()
        .take_outgoing(256)
        .into_iter()
        .filter_map(|m| match m {
            ClientMessage::Action { action, .. } | ClientMessage::AimedAction { action, .. } => {
                Some(action)
            }
            _ => None,
        })
        .collect()
}
fn fill(session: &mut RemoteSession) {
    for x in 0..(256 - session.connection().queue_depths().outgoing) {
        session
            .connection_mut()
            .request_height(
                net::height::HeightKey {
                    space: 0,
                    level: 0,
                    x: x as i32,
                    z: 0,
                },
                0,
            )
            .unwrap();
    }
    assert_eq!(session.connection().queue_depths().outgoing, 256);
}
#[test]
fn actual_held_food_sends_one_press_and_release_retries_without_prediction() {
    let (mut s, mut scene) = session(rules::items::I_APPLE, 10.);
    for now in 1..4 {
        s.pump_scene(
            &mut scene,
            RemoteInput {
                placing: true,
                ..Default::default()
            },
            now,
        )
        .unwrap();
    }
    let action = actions(&mut s);
    assert!(
        matches!(action.as_slice(),[Action::UseFood{held:true,inventory}]if inventory.item==rules::items::I_APPLE&&inventory.count==2)
    );
    // Regeneration reaching full HP after use started must not cancel that
    // bite; the input owner also drops attack buttons while still using food.
    let mut actual = sim::Player::new([8.5, 1., 8.5]);
    actual.health = 20.;
    s.connection_mut()
        .receive(
            ServerMessage::Prediction {
                tick: Tick(1),
                player: PlayerState {
                    health: 20.,
                    position: actual.pos,
                    velocity: actual.vel,
                    yaw: actual.yaw,
                    pitch: actual.pitch,
                    mode: actual.mode as u8,
                    on_ground: actual.on_ground,
                    flying: actual.flying,
                    noclip: actual.noclip,
                    ..Default::default()
                },
                continuation: sim::prediction::PredictionState::capture(&actual)
                    .unwrap()
                    .as_bytes()
                    .to_vec(),
            },
            4,
        )
        .unwrap();
    s.pump_scene(
        &mut scene,
        RemoteInput {
            placing: true,
            keys: net::InputFrame::ATTACK,
            ..Default::default()
        },
        4,
    )
    .unwrap();
    assert!(actions(&mut s).is_empty());
    fill(&mut s);
    for now in 4..7 {
        s.pump_scene(&mut scene, RemoteInput::default(), now)
            .unwrap();
        assert_eq!(s.connection().queue_depths().outgoing, 256);
    }
    s.connection_mut().take_outgoing(256);
    s.pump_scene(&mut scene, RemoteInput::default(), 7).unwrap();
    assert!(matches!(
        actions(&mut s).as_slice(),
        [Action::UseFood { held: false, .. }]
    ));
    assert_eq!(s.connection().player().unwrap().health, 20.);
    assert_eq!(
        s.connection().predicted_inventory().unwrap().hand().count(),
        2
    );
    assert_eq!(s.connection().predicted_edits(), 0);
}
#[test]
fn short_bucket_press_survives_full_outbox_and_keyup_with_original_guard() {
    let (mut s, mut scene) = session(rules::items::I_BUCKET, 20.);
    let page = PageId {
        space: SpaceId(0),
        x: 0,
        y: 0,
        z: 0,
    };
    let mut cells = Box::new([Cell::AIR; 4096]);
    cells[..256].fill(Cell::from_parts(rules::ids::STONE, 0));
    cells[8 + 16 * (11 + 16 * 2)] = Cell::from_parts(rules::ids::WATER, 0);
    s.connection_mut()
        .receive(
            ServerMessage::Page(net::PageUpdate::Snapshot(net::PageSnapshot {
                page,
                version: 1,
                source_version: Version(1),
                applied_action: Default::default(),
                cells,
            })),
            0,
        )
        .unwrap();
    fill(&mut s);
    s.pump_scene(
        &mut scene,
        RemoteInput {
            place_pressed: true,
            ..Default::default()
        },
        1,
    )
    .unwrap();
    assert_eq!(s.connection().pending_actions(), 0);
    s.connection_mut().take_outgoing(256);
    s.pump_scene(&mut scene, RemoteInput::default(), 2).unwrap();
    let a = actions(&mut s);
    assert!(
        matches!(a.as_slice(),[Action::UseBucket{position,expected,inventory}]if [position.x,position.y,position.z]==[8,2,11]&&expected.id()==rules::ids::WATER&&inventory.item==rules::items::I_BUCKET&&inventory.count==2),
        "actions={a:?}; player={:?}; read={:?}",
        s.connection().player().map(|p| p.pos),
        s.connection().read(core_types::BlockPos {
            space: SpaceId(0),
            x: 8,
            y: 2,
            z: 11
        })
    );
    s.pump_scene(&mut scene, RemoteInput::default(), 3).unwrap();
    assert!(actions(&mut s).is_empty());
    assert_eq!(
        s.connection().predicted_inventory().unwrap().hand().item(),
        rules::items::I_BUCKET
    );
    assert_eq!(s.connection().predicted_edits(), 0);
}

fn receive_encoded(s: &mut RemoteSession, message: ServerMessage, now_ms: u64) {
    s.receive_frame(&net::encode_server(&message).unwrap(), now_ms)
        .unwrap();
}
fn food_reply(s: &mut RemoteSession, id: ActionId, status: net::ActionStatus, now_ms: u64) {
    receive_encoded(
        s,
        ServerMessage::Action(net::ActionReply {
            id,
            status,
            transaction: None,
            source_version: Version(100),
            position: None,
            cell: None,
        }),
        now_ms,
    );
}
fn action_frames(s: &mut RemoteSession) -> Vec<(ActionId, Action)> {
    let mut actions = Vec::new();
    while let Some(bytes) = s.take_outgoing() {
        match net::decode_client(&bytes).unwrap() {
            ClientMessage::Action { id, action }
            | ClientMessage::AimedAction { id, action, .. } => {
                actions.push((id, action));
            }
            _ => {}
        }
    }
    actions
}
fn hold(s: &mut RemoteSession, scene: &mut ClientScene, held: bool, now_ms: u64) {
    s.pump_scene(
        scene,
        RemoteInput {
            placing: held,
            ..Default::default()
        },
        now_ms,
    )
    .unwrap();
}
fn ground(s: &mut RemoteSession) {
    let mut cells = Box::new([Cell::AIR; 4096]);
    cells[..256].fill(Cell::from_parts(rules::ids::STONE, 0));
    receive_encoded(
        s,
        ServerMessage::Page(net::PageUpdate::Snapshot(net::PageSnapshot {
            page: PageId {
                space: SpaceId(0),
                x: 0,
                y: 0,
                z: 0,
            },
            version: 1,
            source_version: Version(1),
            applied_action: Default::default(),
            cells,
        })),
        0,
    );
}
fn forward_factor(s: &mut RemoteSession) -> f32 {
    let next = Tick(s.connection().predicted_tick().0 + 1);
    s.connection_mut()
        .schedule_input(next, net::InputFrame::FORWARD, 0., 0.)
        .unwrap();
    assert!(s.connection_mut().advance_to(next).unwrap());
    s.connection().player().unwrap().fwd_in
}
#[test]
fn encoded_rejected_food_start_clears_slowdown_and_does_not_retry_held_stale_guard() {
    let (mut s, mut scene) = session(rules::items::I_APPLE, 10.);
    ground(&mut s);
    hold(&mut s, &mut scene, true, 1);
    let start = action_frames(&mut s);
    let [
        (
            id,
            Action::UseFood {
                held: true,
                inventory: old_guard,
            },
        ),
    ] = start.as_slice()
    else {
        panic!("expected food start: {start:?}");
    };
    // A real inventory update can invalidate the guard while start is in
    // flight. The server's rejection must undo only the local use prediction.
    let mut inventory = *s.connection().inventory().unwrap();
    inventory
        .set_trusted(
            0,
            rules::stack::ItemStack::new(rules::items::I_APPLE, 3).unwrap(),
        )
        .unwrap();
    assert_ne!(inventory.revision(), old_guard.revision);
    receive_encoded(
        &mut s,
        ServerMessage::Inventory {
            data: inventory.encode().to_vec(),
        },
        2,
    );
    food_reply(
        &mut s,
        *id,
        net::ActionStatus::Rejected(net::RejectReason::Stale),
        2,
    );
    for now in 3..7 {
        hold(&mut s, &mut scene, true, now);
    }
    assert!(
        action_frames(&mut s).is_empty(),
        "no held retry or spurious release"
    );
    assert_eq!(s.connection().pending_actions(), 0);
    assert_eq!(
        forward_factor(&mut s),
        1.,
        "rejection restores normal movement"
    );
    assert_eq!(s.connection().player().unwrap().health, 10.);
    assert_eq!(s.connection().inventory().unwrap().hand().count(), 3);
    hold(&mut s, &mut scene, false, 7);
    hold(&mut s, &mut scene, true, 8);
    let next = action_frames(&mut s);
    assert!(
        matches!(next.as_slice(), [(new, Action::UseFood { held: true, inventory })]
        if *new > *id && inventory.count == 3 && inventory.revision != old_guard.revision)
    );
    assert_eq!(
        forward_factor(&mut s),
        0.2,
        "fresh physical press starts prediction"
    );
}
#[test]
fn old_encoded_rejections_and_duplicate_welcome_cannot_cancel_a_newer_food_start() {
    let (mut s, mut scene) = session(rules::items::I_APPLE, 10.);
    ground(&mut s);
    hold(&mut s, &mut scene, true, 1);
    let old = action_frames(&mut s)[0].0;
    hold(&mut s, &mut scene, false, 2);
    let release = action_frames(&mut s)[0].0;
    hold(&mut s, &mut scene, true, 3);
    let new = action_frames(&mut s)[0].0;
    assert!(old < release && release < new);
    food_reply(
        &mut s,
        old,
        net::ActionStatus::Rejected(net::RejectReason::Stale),
        4,
    );
    food_reply(
        &mut s,
        release,
        net::ActionStatus::Rejected(net::RejectReason::Stale),
        5,
    );
    let welcome = s.connection().welcome().unwrap().clone();
    receive_encoded(&mut s, ServerMessage::Welcome(welcome), 6);
    hold(&mut s, &mut scene, true, 7);
    assert!(action_frames(&mut s).is_empty());
    assert_eq!(forward_factor(&mut s), 0.2);
    food_reply(&mut s, new, net::ActionStatus::Accepted, 8);
    // A contradictory terminal replay has no pending start left to reject.
    food_reply(
        &mut s,
        new,
        net::ActionStatus::Rejected(net::RejectReason::Stale),
        9,
    );
    hold(&mut s, &mut scene, true, 10);
    assert!(action_frames(&mut s).is_empty());
    assert_eq!(forward_factor(&mut s), 0.2);
}
#[test]
fn new_session_resets_food_state_and_welcome_cannot_replace_a_live_connection() {
    let (mut old, mut scene) = session(rules::items::I_APPLE, 10.);
    ground(&mut old);
    hold(&mut old, &mut scene, true, 1);
    assert_eq!(action_frames(&mut old)[0].0, ActionId(1));
    let mut other = old.connection().welcome().unwrap().clone();
    other.session = 2;
    assert!(
        old.receive_frame(
            &net::encode_server(&ServerMessage::Welcome(other)).unwrap(),
            2
        )
        .is_err()
    );
    assert_eq!(old.connection().welcome().unwrap().session, 1);
    assert_eq!(forward_factor(&mut old), 0.2);
    food_reply(
        &mut old,
        ActionId(1),
        net::ActionStatus::Rejected(net::RejectReason::Stale),
        3,
    );
    hold(&mut old, &mut scene, true, 4);
    assert!(action_frames(&mut old).is_empty());
    // Both active-use prediction and the rejected-press latch are session-local.
    // Both socket adapters reconnect by constructing a fresh RemoteSession;
    // they never transplant Welcome or old callbacks into the new owner.
    drop(old);
    let (mut fresh, mut fresh_scene) = session_with_id(rules::items::I_APPLE, 10., 2);
    ground(&mut fresh);
    assert_eq!(fresh.connection().pending_actions(), 0);
    assert!(action_frames(&mut fresh).is_empty());
    assert_eq!(forward_factor(&mut fresh), 1.);
    hold(&mut fresh, &mut fresh_scene, true, 1);
    assert!(matches!(
        action_frames(&mut fresh).as_slice(),
        [(ActionId(1), Action::UseFood { held: true, .. })]
    ));
}
#[test]
fn confirmed_consumable_observation_never_labels_local_prediction_as_authority() {
    let (mut s, _) = session(rules::items::I_APPLE, 10.);
    ground(&mut s);
    let original = s.connection().confirmed_motion().unwrap();
    assert_eq!(original.0, Tick(0));
    assert_eq!(forward_factor(&mut s), 1.);
    assert_ne!(s.connection().player().unwrap().pos, original.1.position);
    assert_eq!(s.connection().confirmed_motion(), Some(original));
    let mut actual = sim::Player::new([8.5, 1., 9.5]);
    actual.health = 9.;
    let prediction = |tick| ServerMessage::Prediction {
        tick,
        player: PlayerState {
            health: actual.health,
            position: actual.pos,
            velocity: actual.vel,
            yaw: actual.yaw,
            pitch: actual.pitch,
            mode: actual.mode as u8,
            on_ground: actual.on_ground,
            flying: actual.flying,
            noclip: actual.noclip,
            ..Default::default()
        },
        continuation: sim::prediction::PredictionState::capture(&actual)
            .unwrap()
            .as_bytes()
            .to_vec(),
    };
    receive_encoded(&mut s, prediction(Tick(0)), 10);
    assert_eq!(s.connection().confirmed_motion(), Some(original));
    receive_encoded(&mut s, prediction(Tick(8)), 11);
    let confirmed = s.connection().confirmed_motion().unwrap();
    assert_eq!(confirmed.0, Tick(8));
    assert_eq!(confirmed.1.health, 9.);
    assert_eq!(confirmed.1.position, actual.pos);
    // The new authority is pending reconciliation; observation already names
    // the decoded checkpoint, not the previous locally advanced Player.
    assert_eq!(s.connection().player().unwrap().health, 10.);
}
