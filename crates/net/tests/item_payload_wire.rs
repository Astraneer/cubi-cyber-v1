use core_types::{BlockPos, Cell, SpaceId};
use net::window::*;
use net::{Action, ActionId, ClientMessage, InventoryGuard};
fn image() -> WindowSnapshot {
    WindowSnapshot {
        guard: WindowGuard {
            id: 1,
            revision: 2,
            inventory_revision: 3,
        },
        kind: WindowKind::Inventory,
        closed: false,
        binding: None,
        cursor: Stack {
            item: 63,
            count: 1,
            damage: 0,
            payload: 0,
        },
        craft: [Stack::EMPTY; 9],
        craft_result: Stack::EMPTY,
        container: Vec::new(),
        progress: 0,
        fuel: 0,
        energy: 0,
        sides: [0; 6],
    }
}
fn hex(text: &str) -> Vec<u8> {
    text.as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}
#[test]
fn ordinary_v1_window_bytes_are_frozen_and_payload_is_a_strict_sparse_tail() {
    let ordinary = image();
    let frozen = hex(
        "57554931010000000000000002000000000000000300000000000000000000003f0001000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
    );
    assert_eq!(encode_snapshot(&ordinary).unwrap(), frozen);
    let mut tagged = ordinary.clone();
    tagged.cursor.payload = 42;
    let bytes = encode_snapshot(&tagged).unwrap();
    assert!(bytes.starts_with(&frozen));
    assert_eq!(
        &bytes[frozen.len()..],
        &[b'X', b'R', b'F', b'1', 1, 0, 0, 0, 42, 0, 0, 0, 0, 0, 0, 0]
    );
    assert_eq!(decode_snapshot(&bytes).unwrap(), tagged);
    for n in frozen.len() + 1..bytes.len() {
        assert!(decode_snapshot(&bytes[..n]).is_err());
    }
    let mut bad = bytes.clone();
    *bad.last_mut().unwrap() = 0;
    bad[frozen.len() + 8] = 0;
    assert!(decode_snapshot(&bad).is_err());
    tagged.cursor.count = 2;
    assert!(encode_snapshot(&tagged).is_err());
    let mut trailing = bytes;
    trailing.push(0);
    assert!(decode_snapshot(&trailing).is_err());
}
fn owned(payload: u64) -> ClientMessage {
    ClientMessage::Action {
        id: ActionId(9),
        action: Action::PlaceOwned {
            position: BlockPos {
                space: SpaceId(0),
                x: 1,
                y: 2,
                z: 3,
            },
            expected: Cell::AIR,
            cell: Cell::from_parts(63, 0),
            inventory: InventoryGuard {
                revision: 4,
                selected: 0,
                item: 63,
                count: 1,
                damage: 0,
                payload,
            },
        },
    }
}
#[test]
fn ordinary_place_keeps_tag7_and_immutable_owned_data_has_distinct_tag18() {
    let ordinary = net::encode_client(&owned(0)).unwrap();
    let tagged = net::encode_client(&owned(42)).unwrap();
    assert_eq!(ordinary.len(), 67);
    assert_eq!(ordinary[27], 7);
    assert_eq!(tagged.len(), 75);
    assert_eq!(tagged[27], 18);
    assert_eq!(net::decode_client(&ordinary).unwrap(), owned(0));
    assert_eq!(net::decode_client(&tagged).unwrap(), owned(42));
    let mut zero = tagged;
    zero[67..75].fill(0);
    let checksum = zero[18..].iter().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x100000001b3)
    });
    zero[10..18].copy_from_slice(&checksum.to_le_bytes());
    assert!(net::decode_client(&zero).is_err());
}
