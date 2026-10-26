pub const MAX_COORDINATE: i32 = net::MAX_COORDINATE;
#[path = "../src/window.rs"]
pub mod window;
use core_types::{BlockPos, Cell, SpaceId};
use window::*;
fn guard() -> WindowGuard {
    WindowGuard {
        id: 42,
        revision: 7,
        inventory_revision: 9,
    }
}
fn at() -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x: 1,
        y: 64,
        z: -1,
    }
}
#[test]
fn protected_display_bit_preserves_legacy_layout_and_rejects_unknown_flags() {
    let mut state = WindowSnapshot {
        guard: guard(),
        kind: WindowKind::Chest(27),
        closed: false,
        read_only: false,
        binding: Some(at()),
        cursor: Stack::EMPTY,
        craft: [Stack::EMPTY; 9],
        craft_result: Stack::EMPTY,
        container: vec![Stack::EMPTY; 27],
        progress: 0,
        fuel: 0,
        energy: 0,
        sides: [0; 6],
    };
    let legacy = encode_snapshot(&state).unwrap();
    assert_eq!(legacy[30], 0);
    state.read_only = true;
    let bytes = encode_snapshot(&state).unwrap();
    let differences: Vec<_> = bytes
        .iter()
        .zip(&legacy)
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(differences, [30]);
    assert_eq!(bytes[30], 2);
    assert_eq!(decode_snapshot(&bytes).unwrap(), state);
    let mut invalid = bytes.clone();
    invalid[30] |= 4;
    assert_eq!(decode_snapshot(&invalid), Err(WindowCodecError::Value));
    invalid[30] = 3;
    assert_eq!(decode_snapshot(&invalid), Err(WindowCodecError::Value));
    state.closed = true;
    assert_eq!(encode_snapshot(&state), Err(WindowCodecError::Value));
    state.closed = false;
    state.binding = None;
    state.kind = WindowKind::Inventory;
    state.container.clear();
    assert_eq!(encode_snapshot(&state), Err(WindowCodecError::Value));
}
#[test]
fn typed_window_commands_roundtrip_and_never_accept_truncation_or_trailing_data() {
    for intent in [
        WindowIntent::OpenInventory,
        WindowIntent::OpenBlock {
            position: at(),
            expected: Cell::from_parts(1, 0),
        },
        WindowIntent::Click {
            guard: guard(),
            slot: 108,
            right: true,
            shift: true,
        },
        WindowIntent::RecipeFill {
            guard: guard(),
            recipe: 5,
            all: true,
        },
        WindowIntent::TakeCraft {
            guard: guard(),
            all: false,
        },
        WindowIntent::Close { guard: guard() },
        WindowIntent::MachineSide {
            guard: guard(),
            face: 5,
            mode: 5,
        },
    ] {
        let bytes = encode_intent(intent).unwrap();
        assert_eq!(decode_intent(&bytes).unwrap(), intent);
        for end in 0..bytes.len() {
            assert!(decode_intent(&bytes[..end]).is_err());
        }
        let mut extra = bytes;
        extra.push(0);
        assert_eq!(decode_intent(&extra), Err(WindowCodecError::Trailing));
    }
    assert_eq!(
        encode_intent(WindowIntent::OpenInventory).unwrap(),
        b"WIN1\0"
    );
}
#[test]
fn all_window_kinds_have_bounded_snapshots_with_strict_shapes_and_values() {
    for kind in [
        WindowKind::Inventory,
        WindowKind::Workbench,
        WindowKind::Chest(27),
        WindowKind::Chest(54),
        WindowKind::Furnace,
        WindowKind::Machine(0),
        WindowKind::Machine(1),
        WindowKind::Machine(2),
        WindowKind::Machine(3),
        WindowKind::Machine(4),
        WindowKind::Machine(5),
        WindowKind::Machine(6),
        WindowKind::Machine(7),
        WindowKind::Machine(8),
        WindowKind::Machine(9),
    ] {
        let mut state = WindowSnapshot {
            guard: guard(),
            kind,
            closed: false,
            read_only: false,
            binding: (kind != WindowKind::Inventory).then(at),
            cursor: Stack {
                item: 1,
                count: 64,
                damage: 0,
                payload: 0,
            },
            craft: [Stack::EMPTY; 9],
            craft_result: Stack::EMPTY,
            container: vec![Stack::EMPTY; kind.container_len().unwrap()],
            progress: 500,
            fuel: 1000,
            energy: 7,
            sides: [5; 6],
        };
        let bytes = encode_snapshot(&state).unwrap();
        assert!(bytes.len() < MAX_WINDOW_BYTES);
        assert_eq!(decode_snapshot(&bytes).unwrap(), state);
        for end in 0..bytes.len() {
            assert!(decode_snapshot(&bytes[..end]).is_err());
        }
        state.cursor.count = 65;
        assert_eq!(encode_snapshot(&state), Err(WindowCodecError::Value));
    }
    assert!(decode_snapshot(&vec![0; MAX_WINDOW_BYTES + 1]).is_err());
    assert!(
        encode_intent(WindowIntent::Click {
            guard: guard(),
            slot: 99,
            right: false,
            shift: false
        })
        .is_err()
    );
    assert!(
        encode_intent(WindowIntent::MachineSide {
            guard: guard(),
            face: 6,
            mode: 0
        })
        .is_err()
    );
}
