use core_types::{SpaceId, Tick};
use net::audio_cue::*;
#[test]
fn causal_cue_has_fixed_canonical_bytes_and_rejects_every_bad_prefix() {
    let cue = Cue {
        kind: Kind::ChickenEgg,
        actor: 7,
        sequence: 9,
        tick: Tick(9),
        space: SpaceId(2),
        position: [1.25, 2., -3.],
    };
    let b = cue.encode().unwrap();
    assert_eq!(&b[..8], b"ACU1\0\0\0\0");
    assert_eq!(&b[8..16], &7u64.to_le_bytes());
    assert_eq!(&b[16..24], &9u64.to_le_bytes());
    assert_eq!(&b[40..48], &1.25f64.to_le_bytes());
    assert_eq!(Cue::decode(&b).unwrap(), cue);
    for n in 0..BYTES {
        assert!(Cue::decode(&b[..n]).is_err());
    }
    let mut bad = b;
    bad[4] = u8::MAX;
    assert!(Cue::decode(&bad).is_err());
    let mut bad = b;
    bad[5] = 1;
    assert!(Cue::decode(&bad).is_err());
    let mut bad = b;
    bad[36] = 1;
    assert!(Cue::decode(&bad).is_err());
    assert!(Cue { actor: 0, ..cue }.encode().is_err());
    assert!(Cue { sequence: 0, ..cue }.encode().is_err());
    assert!(Cue { sequence: 8, ..cue }.encode().is_err());
    assert!(
        Cue {
            position: [f64::NAN, 0., 0.],
            ..cue
        }
        .encode()
        .is_err()
    );
}

#[test]
fn owned_cues_use_explicit_event_sequences_without_changing_legacy_egg_bytes() {
    for (tag, kind) in [
        Kind::ChickenEgg,
        Kind::ChestOpen,
        Kind::ChestClose,
        Kind::ItemPickup,
        Kind::AttackWeak,
        Kind::AttackStrong,
        Kind::AttackCrit,
        Kind::AttackKnockback,
        Kind::BucketFill,
        Kind::BucketEmpty,
        Kind::Eat,
        Kind::EatDone,
    ]
    .into_iter()
    .enumerate()
    {
        let cue = Cue {
            kind,
            actor: 7,
            sequence: 42,
            tick: Tick(42),
            space: SpaceId(0),
            position: [1., 2., 3.],
        };
        let bytes = cue.encode().unwrap();
        assert_eq!(bytes.len(), BYTES);
        assert_eq!(&bytes[..4], b"ACU1");
        assert_eq!(&bytes[5..8], &[0; 3]);
        assert_eq!(&bytes[36..40], &[0; 4]);
        assert_eq!(bytes[4], tag as u8);
        assert_eq!(Cue::decode(&bytes), Ok(cue));
        let action = Cue { sequence: 3, ..cue };
        assert_eq!(
            action.encode().is_ok(),
            !matches!(kind, Kind::ChickenEgg | Kind::ItemPickup)
        );
        assert!(Cue { sequence: 0, ..cue }.encode().is_err());
    }
}

fn block(kind: bool) -> Cue {
    let player = net::PlayerId([0x51; 16]);
    let actor = BlockActor {
        player,
        material: BlockMaterial::Glass,
    };
    Cue {
        kind: if kind {
            Kind::BlockBreak(actor)
        } else {
            Kind::BlockPlace(actor)
        },
        actor: net::player_entity_id(player),
        sequence: 9001,
        tick: Tick(42),
        space: SpaceId(9),
        position: [-2.5, 75.5, 8.5],
    }
}

#[test]
fn block_cues_keep_exact_player_identity_and_family_in_bounded_nested_acu2() {
    for breaking in [false, true] {
        for material in 1..=10 {
            let mut cue = block(breaking);
            let mut actor = cue.kind.block().unwrap();
            actor.material = BlockMaterial::decode(material).unwrap();
            cue.kind = if breaking {
                Kind::BlockBreak(actor)
            } else {
                Kind::BlockPlace(actor)
            };
            let bytes = cue.encode().unwrap();
            assert_eq!(bytes.len(), MAX_BYTES);
            assert_eq!(&bytes[..4], b"ACU2");
            assert_eq!(bytes[4], if breaking { 12 } else { 13 });
            assert_eq!(bytes[5], material);
            assert_eq!(&bytes[64..80], &actor.player.0);
            assert_eq!(&bytes[36..40], &[0; 4]);
            assert_eq!(Cue::decode(&bytes), Ok(cue));
            let frame = net::ServerMessage::AudioCue(cue);
            assert_eq!(
                net::decode_server(&net::encode_server(&frame).unwrap()),
                Ok(frame)
            );
        }
    }
    // Identity retains all 128 public bits, including players with equal low64.
    let first = block(true);
    let mut actor = first.kind.block().unwrap();
    actor.player.0[15] ^= 1;
    let second = Cue {
        kind: Kind::BlockBreak(actor),
        actor: net::player_entity_id(actor.player),
        ..first
    };
    assert_eq!(
        &first.kind.block().unwrap().player.0[..8],
        &actor.player.0[..8]
    );
    assert_ne!(first.kind.block().unwrap().player, actor.player);
    assert_eq!(Cue::decode(&second.encode().unwrap()), Ok(second));
}

#[test]
fn block_cue_prefix_material_identity_and_centre_are_strict() {
    let cue = block(true);
    let bytes = cue.encode().unwrap();
    for n in 0..MAX_BYTES {
        assert!(Cue::decode(&bytes[..n]).is_err(), "prefix {n}");
    }
    for (index, value) in [(4, 0), (5, 0), (5, 11), (6, 1), (36, 1), (64, 0)] {
        let mut bad = bytes;
        bad[index] = value;
        assert!(Cue::decode(&bad).is_err(), "index {index} value {value}");
    }
    let mut bad = bytes;
    bad[..4].copy_from_slice(b"ACU1");
    assert!(Cue::decode(&bad).is_err());
    let mut bad = bytes.to_vec();
    bad.push(0);
    assert!(Cue::decode(&bad).is_err());
    assert!(
        Cue {
            position: [0., 75.5, 8.5],
            ..cue
        }
        .encode()
        .is_err()
    );
    assert!(
        Cue {
            actor: cue.actor ^ 1,
            ..cue
        }
        .encode()
        .is_err()
    );
    assert!(
        Cue {
            position: [f64::INFINITY, 0.5, 0.5],
            ..cue
        }
        .encode()
        .is_err()
    );
}
