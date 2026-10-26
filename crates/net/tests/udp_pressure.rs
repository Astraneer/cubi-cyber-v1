//! Loss of fragmented snapshots cannot occupy every reliable receive slot.
use net::{
    SecretToken,
    udp::{Channel, Limits, Side, UdpError},
};
fn pair() -> (Channel, Channel) {
    let key = SecretToken::new([19; 32]);
    (
        Channel::new(1, key, Side::Server, Limits::default()),
        Channel::new(1, key, Side::Client, Limits::default()),
    )
}
#[test]
fn sixteen_lost_unreliable_tails_cannot_starve_durable_action_or_change_limits() {
    let (mut server, mut client) = pair();
    for i in 0..16 {
        let parts = server.send(vec![i; 2200], false, 0).unwrap();
        assert_eq!(parts.len(), 2);
        assert!(client.receive(&parts[0], 0).unwrap().frames.is_empty());
        // Actual missing tail; no retry exists for this unreliable snapshot.
    }
    let action = b"durable action 2".to_vec();
    let packet = server.send(action.clone(), true, 1).unwrap().pop().unwrap();
    let received = client.receive(&packet, 1).unwrap();
    assert_eq!(received.frames, vec![action]);
    assert_eq!(received.replies.len(), 1);
    for ack in received.replies {
        server.receive(&ack, 1).unwrap();
    }
    assert!(server.tick(3000).unwrap().is_empty());
    assert_eq!(client.counters().discarded_unreliable_assemblies, 1);
    assert!(client.counters().peak_reassembly_bytes <= Limits::default().maximum_bytes);
}
#[test]
fn reliable_partial_is_never_evicted_and_all_reliable_pressure_stays_explicit() {
    let (mut server, mut client) = pair();
    let mut tails = Vec::new();
    for i in 0..16 {
        let mut parts = server.send(vec![i; 2200], true, 0).unwrap();
        tails.push(parts.pop().unwrap());
        client.receive(&parts[0], 0).unwrap();
    }
    // An unreliable newer image cannot discard the reliable guarded actions.
    let fresh = server.send(vec![99; 8], false, 1).unwrap();
    assert_eq!(client.receive(&fresh[0], 1).err(), Some(UdpError::Capacity));
    assert_eq!(client.counters().discarded_unreliable_assemblies, 0);
    for (i, tail) in tails.iter().enumerate() {
        let received = client.receive(tail, 2).unwrap();
        assert_eq!(received.frames, vec![vec![i as u8; 2200]]);
        for ack in received.replies {
            server.receive(&ack, 2).unwrap();
        }
    }
    assert!(server.tick(3000).unwrap().is_empty());
    assert_eq!(
        client.receive(&fresh[0], 3).unwrap().frames,
        vec![vec![99; 8]]
    );
}

#[test]
fn byte_pressure_reclaims_only_authenticated_unreliable_partials() {
    let (mut server, mut client) = pair();
    for _ in 0..2 {
        let mut parts = server
            .send(vec![3; net::MAX_FRAME_BYTES], false, 0)
            .unwrap();
        parts.pop();
        for part in parts {
            assert!(client.receive(&part, 0).unwrap().frames.is_empty());
        }
    }
    let frame = vec![9; 1000];
    let packet = server.send(frame.clone(), true, 1).unwrap().pop().unwrap();
    let mut corrupt = packet.clone();
    *corrupt.last_mut().unwrap() ^= 1;
    assert_eq!(
        client.receive(&corrupt, 1).err(),
        Some(UdpError::Authentication)
    );
    assert_eq!(client.counters().discarded_unreliable_assemblies, 0);
    let received = client.receive(&packet, 1).unwrap();
    assert_eq!(received.frames, vec![frame]);
    assert_eq!(client.counters().discarded_unreliable_assemblies, 1);
    assert!(client.counters().peak_reassembly_bytes <= Limits::default().maximum_bytes);
}
