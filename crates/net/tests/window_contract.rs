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
            binding: (kind != WindowKind::Inventory).then(at),
            cursor: Stack {
                item: 1,
                count: 64,
                damage: 0,
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
