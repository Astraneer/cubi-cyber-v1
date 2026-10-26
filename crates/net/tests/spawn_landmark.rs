use core_types::{BlockPos, SpaceId};
use net::{CodecError, ServerMessage, decode_server, encode_server, spawn::Landmark};

fn landmark() -> Landmark {
    Landmark {
        session: 91,
        center: BlockPos {
            space: SpaceId(0),
            x: -123,
            y: 67,
            z: 456,
        },
    }
}

fn repair_checksum(bytes: &mut [u8]) {
    let hash = bytes[18..].iter().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x100_0000_01b3)
    });
    bytes[10..18].copy_from_slice(&hash.to_le_bytes());
}

#[test]
fn public_landmark_is_fixed_bounded_and_rejects_unsupported_layout_before_use() {
    let value = landmark();
    let message = ServerMessage::SpawnLandmark(value);
    let bytes = encode_server(&message).unwrap();
    assert_eq!(bytes.len(), 45);
    assert_eq!(&bytes[18..21], &[30, 1, 0]);
    assert_eq!(&bytes[21..29], &91u64.to_le_bytes());
    assert_eq!(decode_server(&bytes).unwrap(), message);
    for length in 0..bytes.len() {
        assert!(decode_server(&bytes[..length]).is_err());
    }
    for (offset, expected) in [(19, CodecError::Version), (29, CodecError::InvalidBounds)] {
        let mut invalid = bytes.clone();
        invalid[offset] = 2;
        repair_checksum(&mut invalid);
        assert_eq!(decode_server(&invalid), Err(expected));
    }
    for center in [
        BlockPos {
            space: SpaceId(1),
            x: 0,
            y: 0,
            z: 0,
        },
        BlockPos {
            space: SpaceId(0),
            x: net::MAX_COORDINATE - 64,
            y: 0,
            z: 0,
        },
        BlockPos {
            space: SpaceId(0),
            x: 0,
            y: i32::MIN,
            z: 0,
        },
    ] {
        assert!(
            encode_server(&ServerMessage::SpawnLandmark(Landmark { center, ..value })).is_err()
        );
    }
}
