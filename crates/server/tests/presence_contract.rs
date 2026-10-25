use net::{ActionId, PlayerId, Role, presence};
#[path = "../../client/src/network/presence.rs"]
mod cache;
use server::presence as service;

use cache::PresenceCache;
use core_types::Tick;
use presence::{ChatLine, PresenceClientEvent, PresenceServerEvent, RosterDelta};
use service::{Presence, PresenceError};
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);

fn id(value: u8) -> PlayerId {
    PlayerId([value; 16])
}
fn update(service: &mut Presence, cache: &mut PresenceCache) {
    if let Some(event) = service.roster_since(cache.revision()) {
        assert_eq!(cache.receive(event).unwrap(), None);
    }
}

#[test]
fn public_roster_changes_and_missing_base_fail_closed_until_reset() {
    let mut service = Presence::new(7);
    assert!(service.is_empty());
    let mut cache = PresenceCache::new();
    cache.reset(id(1));
    assert!(service.join(id(1), "Олег", Role::Owner, 0).unwrap());
    assert!(!service.join(id(1), "Олег", Role::Owner, 0).unwrap());
    update(&mut service, &mut cache);
    assert_eq!(cache.own_role(), Some(Role::Owner));
    assert_eq!(cache.roster().count(), 1);
    assert_eq!(service.len(), 1);
    assert!(!service.set_role(id(1), Role::Owner).unwrap());
    service.join(id(2), "Друг", Role::Player, 0).unwrap();
    service.set_role(id(1), Role::Player).unwrap();
    let missing = PresenceServerEvent::RosterDelta(RosterDelta {
        base: service.revision() - 1,
        revision: service.revision(),
        reset: false,
        upserts: vec![service.entry(id(1)).unwrap().clone()],
        removes: vec![],
    });
    assert_eq!(
        cache.receive(missing.clone()).unwrap(),
        Some(PresenceClientEvent::RosterRequest { known_revision: 1 })
    );
    assert_eq!(cache.own_role(), None);
    assert_eq!(cache.receive(missing).unwrap(), None);
    update(&mut service, &mut cache);
    assert_eq!(cache.own_role(), Some(Role::Player));
    assert_eq!(cache.entry(id(2)).unwrap().name, "Друг");
    service.remove(id(2)).unwrap();
    update(&mut service, &mut cache);
    assert_eq!(cache.roster().count(), 1);
    for n in 0..70 {
        service
            .set_role(
                id(1),
                if n % 2 == 0 {
                    Role::Admin
                } else {
                    Role::Player
                },
            )
            .unwrap();
    }
    let full = service.roster_since(0).unwrap();
    assert!(
        matches!(&full, PresenceServerEvent::RosterDelta(delta) if delta.reset && delta.upserts.len() == 1)
    );
    cache.receive(full).unwrap();
    assert_eq!(cache.own_role(), Some(Role::Player));
    assert!(service.roster_since(cache.revision()).is_none());
    cache
        .receive(PresenceServerEvent::RosterDelta(RosterDelta {
            base: 0,
            revision: 1,
            reset: true,
            upserts: vec![presence::RosterEntry {
                identity: id(1),
                name: "Олег".into(),
                role: Role::Owner,
                ping_ms: None,
            }],
            removes: vec![],
        }))
        .unwrap();
    assert_eq!(cache.own_role(), Some(Role::Player));
}

#[test]
fn chat_publication_follows_consumed_durable_permit_and_rate_survives_reconnect() {
    let mut service = Presence::new(3);
    let mut cache = PresenceCache::new();
    service.join(id(1), "Олег", Role::Player, 1000).unwrap();
    let permit = service
        .reserve_chat(id(1), ActionId(1), "Привет", 1000)
        .unwrap()
        .unwrap();
    assert_eq!(cache.pending_chats(), 0);
    service.cancel_chat(permit);
    let permit = service
        .reserve_chat(id(1), ActionId(1), "Привет", 1000)
        .unwrap()
        .unwrap();
    let event = service.publish_chat(permit, Tick(20));
    cache.receive(event.clone()).unwrap();
    cache.receive(event).unwrap();
    assert_eq!(cache.pending_chats(), 1);
    assert_eq!(cache.chats().next().unwrap().text, "Привет");
    assert_eq!(cache.pop_chat().unwrap().text, "Привет");
    assert!(
        service
            .reserve_chat(id(1), ActionId(1), "дубль", 1000)
            .unwrap()
            .is_none()
    );
    for action in 2..=4 {
        let permit = service
            .reserve_chat(id(1), ActionId(action), "строка", 1000)
            .unwrap()
            .unwrap();
        service.publish_chat(permit, Tick(20));
    }
    assert!(matches!(
        service.reserve_chat(id(1), ActionId(5), "лимит", 1000),
        Err(PresenceError::RateLimited)
    ));
    service.remove(id(1)).unwrap();
    service.join(id(1), "Олег", Role::Player, 1000).unwrap();
    assert!(matches!(
        service.reserve_chat(id(1), ActionId(5), "лимит", 1000),
        Err(PresenceError::RateLimited)
    ));
    assert!(
        service
            .reserve_chat(id(1), ActionId(5), "после паузы", 1500)
            .unwrap()
            .is_some()
    );
    assert!(matches!(
        service.reserve_chat(id(1), ActionId(6), "назад", 1499),
        Err(PresenceError::Clock)
    ));
    assert!(matches!(
        service.reserve_chat(id(1), ActionId(6), "bad\n", 1500),
        Err(PresenceError::Invalid)
    ));
}

#[test]
fn ping_is_challenged_server_measured_quantized_and_at_most_once_per_second() {
    let mut service = Presence::new(9);
    let mut cache = PresenceCache::new();
    service.join(id(1), "Игрок", Role::Player, 100).unwrap();
    let event = service.ping(id(1), 100).unwrap().unwrap();
    let Some(PresenceClientEvent::Pong { nonce }) = cache.receive(event).unwrap() else {
        panic!("pong");
    };
    assert!(service.ping(id(1), 1099).unwrap().is_none());
    assert_eq!(
        service.pong(id(1), nonce.wrapping_add(1), 180),
        Err(PresenceError::Invalid)
    );
    assert!(service.pong(id(1), nonce, 180).unwrap());
    assert_eq!(service.entry(id(1)).unwrap().ping_ms, Some(80));
    assert!(!service.pong(id(1), nonce, 180).unwrap());
    let PresenceServerEvent::Ping { nonce, .. } = service.ping(id(1), 1100).unwrap().unwrap()
    else {
        panic!("ping");
    };
    assert!(!service.pong(id(1), nonce, 1180).unwrap());
    assert_eq!(service.take_work().pings, 2);
}

#[test]
fn client_chat_memory_is_bounded_and_old_replays_do_not_displace_new_messages() {
    let mut cache = PresenceCache::new();
    for sequence in 1..=300 {
        cache
            .receive(PresenceServerEvent::Chat(ChatLine {
                sequence,
                tick: Tick(sequence),
                identity: id(1),
                name: "A".into(),
                text: "test".into(),
            }))
            .unwrap();
    }
    assert_eq!(cache.pending_chats(), 200);
    assert_eq!(cache.counters().dropped_chats, 100);
    for sequence in [1, 44, 300] {
        cache
            .receive(PresenceServerEvent::Chat(ChatLine {
                sequence,
                tick: Tick(sequence),
                identity: id(1),
                name: "A".into(),
                text: "replay".into(),
            }))
            .unwrap();
    }
    assert_eq!(cache.pending_chats(), 200);
    assert_eq!(cache.counters().duplicate_chats, 3);
    assert_eq!(cache.pop_chat().unwrap().sequence, 101);
}

#[test]
fn unchanged_presence_performs_no_allocation_for_ten_thousand_ticks() {
    let mut service = Presence::new(4);
    service.join(id(1), "A", Role::Player, 0).unwrap();
    service.ping(id(1), 0).unwrap();
    let revision = service.revision();
    let scope = telemetry::AllocationScope::begin().unwrap();
    for _ in 0..10_000 {
        assert!(service.roster_since(revision).is_none());
        assert!(service.ping(id(1), 999).unwrap().is_none());
    }
    let counts = scope.finish();
    assert_eq!(counts.allocating_calls(), 0);
    assert_eq!(counts.dealloc_calls, 0);
    let scope = telemetry::AllocationScope::begin().unwrap();
    std::hint::black_box(vec![42u8; 50]);
    assert!(scope.finish().allocating_calls() > 0);
}
