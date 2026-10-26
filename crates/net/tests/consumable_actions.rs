use core_types::{BlockPos, Cell, SpaceId};
use net::*;
#[test]
fn consumable_actions_roundtrip_guard_aim_and_all_truncations() {
    let guard = InventoryGuard {
        revision: 7,
        selected: 8,
        item: 268,
        count: 16,
        damage: 0,
        payload: 0,
    };
    let actions = [
        Action::UseFood {
            held: true,
            inventory: guard,
        },
        Action::UseFood {
            held: false,
            inventory: guard,
        },
        Action::UseBucket {
            position: BlockPos {
                space: SpaceId(3),
                x: -9,
                y: 17,
                z: 24,
            },
            expected: Cell::from_parts(8, 0),
            inventory: guard,
        },
    ];
    for action in actions {
        let message = ClientMessage::AimedAction {
            id: ActionId(43),
            action,
            yaw: 150.,
            pitch: 42.,
        };
        let bytes = encode_client(&message).unwrap();
        assert!(bytes.len() <= 80);
        assert_eq!(decode_client(&bytes).unwrap(), message);
        for len in 0..bytes.len() {
            assert!(decode_client(&bytes[..len]).is_err());
        }
    }
    for guard in [
        InventoryGuard {
            selected: 9,
            ..guard
        },
        InventoryGuard {
            count: 2,
            payload: 1,
            ..guard
        },
    ] {
        for action in [
            Action::UseFood {
                held: true,
                inventory: guard,
            },
            Action::UseBucket {
                position: BlockPos {
                    space: SpaceId(0),
                    x: 0,
                    y: 0,
                    z: 0,
                },
                expected: Cell::AIR,
                inventory: guard,
            },
        ] {
            assert!(
                encode_client(&ClientMessage::Action {
                    id: ActionId(1),
                    action
                })
                .is_err()
            );
        }
    }
}
