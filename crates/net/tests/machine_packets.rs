use core_types::{LocalIdx, PageId, SpaceId, Tick, Version};
use net::machine_packets::*;
fn page() -> PageId {
    PageId {
        space: SpaceId(0),
        x: -1,
        y: 2,
        z: 3,
    }
}
fn pose() -> Pose {
    Pose {
        id: 0x0102030405060708,
        index: LocalIdx::new(33).unwrap(),
        item: 0x1234,
        count: 1,
        color: 3,
        entry: 5,
        exit: 1,
        progress: 25,
        flags: ACTIVE,
        epoch: Tick(11),
    }
}
fn frame() -> Frame {
    Frame {
        page: page(),
        view_version: Version(7),
        revision: 9,
        base: Some(8),
        tick: Tick(12),
        upserts: vec![pose()],
        removes: vec![],
    }
}
#[test]
fn packet_public_wire_matches_independent_little_endian_golden() {
    let expected = "4d504b31 00000000 ffffffff 02000000 03000000 0700000000000000 0900000000000000 0800000000000000 0c00000000000000 0100 0000 0807060504030201 2100 3412 0100 03 05 01 19 01 00 0b00000000000000";
    let bytes: Vec<_> = expected
        .split_whitespace()
        .flat_map(|s| {
            (0..s.len())
                .step_by(2)
                .map(move |i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        })
        .collect();
    assert_eq!(bytes.len(), HEADER_BYTES + ROW_BYTES);
    assert_eq!(frame().encode().unwrap(), bytes);
    assert_eq!(Frame::decode(&bytes).unwrap(), frame());
}
#[test]
fn bounded_counts_padding_invalid_pose_lifecycle_and_every_truncated_prefix_reject() {
    let bytes = frame().encode().unwrap();
    for end in 0..bytes.len() {
        assert!(Frame::decode(&bytes[..end]).is_err(), "prefix{end}");
    }
    let mut bad = bytes.clone();
    bad[54..56].copy_from_slice(&u16::MAX.to_le_bytes());
    assert!(Frame::decode(&bad).is_err());
    let mut bad = bytes.clone();
    bad[HEADER_BYTES + 19] = 1;
    assert!(Frame::decode(&bad).is_err());
    let mut f = frame();
    f.view_version = Version(0);
    assert!(f.encode().is_err());
    let mut f = frame();
    f.upserts[0].progress = 100;
    assert!(f.encode().is_err());
    let mut f = frame();
    f.upserts[0].epoch = Tick(13);
    assert!(f.encode().is_err());
    let mut f = frame();
    f.upserts.push(pose());
    assert!(f.encode().is_err());
    let mut f = frame();
    f.removes.push(pose().id);
    assert!(f.encode().is_err());
    let mut f = frame();
    f.upserts.clear();
    f.removes.push(99);
    assert!(resulting_ids(&f, [pose().id], MAX_PACKETS).is_err());
    let mut f = frame();
    f.upserts = (1..=MAX_PACKETS as u64)
        .map(|id| Pose { id, ..pose() })
        .collect();
    assert_eq!(Frame::decode(&f.encode().unwrap()).unwrap(), f);
    f.upserts.push(Pose {
        id: MAX_PACKETS as u64 + 1,
        ..pose()
    });
    assert!(f.encode().is_err());
}
#[test]
fn request_ack_and_missing_base_are_strict_and_preserve_view_certificate() {
    assert!(
        Request::Full {
            page: page(),
            view_version: Version(0),
            have: None
        }
        .encode()
        .is_err()
    );
    for request in [
        Request::Ack {
            page: page(),
            view_version: Version(7),
            revision: 9,
        },
        Request::Full {
            page: page(),
            view_version: Version(7),
            have: None,
        },
        Request::Full {
            page: page(),
            view_version: Version(7),
            have: Some(9),
        },
    ] {
        let bytes = request.encode().unwrap();
        assert_eq!(Request::decode(&bytes).unwrap(), request);
        for end in 0..bytes.len() {
            assert!(Request::decode(&bytes[..end]).is_err());
        }
    }
    assert!(
        Request::Ack {
            page: page(),
            view_version: Version(7),
            revision: 0
        }
        .encode()
        .is_err()
    );
    let mut bytes = Request::Ack {
        page: page(),
        view_version: Version(7),
        revision: 9,
    }
    .encode()
    .unwrap();
    bytes[4] = 2;
    assert!(Request::decode(&bytes).is_err());
}
#[test]
fn segment_pose_turns_at_halfway_holds_dormant_and_never_guesses_next_cell() {
    let mut p = pose();
    p.index = LocalIdx::new(0).unwrap();
    p.progress = 45;
    p.epoch = Tick(10);
    assert_eq!(
        p.relative_position(page(), Tick(10), 0., [-16, 32, 48]),
        [0.45, 0.25, 0.5]
    );
    assert_eq!(
        p.relative_position(page(), Tick(11), 0., [-16, 32, 48]),
        [0.5, 0.25, 0.5]
    );
    assert_eq!(
        p.relative_position(page(), Tick(12), 0., [-16, 32, 48]),
        [0.5, 0.30000000000000004, 0.5]
    );
    assert_eq!(p.progress_at(Tick(999), 1.), 99.);
    p.flags = 0;
    assert_eq!(p.progress_at(Tick(999), 1.), 45.);
    assert!(
        p.relative_position(page(), Tick(10), 0., [i64::MIN; 3])
            .iter()
            .all(|v| v.is_finite())
    );
}
