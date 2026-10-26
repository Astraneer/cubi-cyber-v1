use core_types::{BlockPos, Cell, SpaceId};
use net::*;
#[test]
fn owned_block_tools_keep_aim_and_inventory_bounded() {
    let inventory = InventoryGuard {
        revision: 1,
        selected: 0,
        item: 327,
        count: 1,
        damage: 0,
        payload: 9,
    };
    let message = ClientMessage::AimedAction {
        id: ActionId(7),
        action: Action::UseBlockTool {
            position: BlockPos {
                space: SpaceId(0),
                x: 8,
                y: 2,
                z: 11,
            },
            expected: Cell::from_parts(96, 2),
            sneaking: true,
            inventory,
        },
        yaw: 0.,
        pitch: 0.,
    };
    let bytes = encode_client(&message).unwrap();
    assert_eq!(bytes.len(), 80);
    assert_eq!(decode_client(&bytes).unwrap(), message);
    for n in 0..bytes.len() {
        assert!(decode_client(&bytes[..n]).is_err());
    }
    for guard in [
        InventoryGuard {
            selected: 9,
            ..inventory
        },
        InventoryGuard {
            count: 2,
            ..inventory
        },
    ] {
        assert!(
            encode_client(&ClientMessage::Action {
                id: ActionId(1),
                action: Action::UseBlockTool {
                    position: BlockPos {
                        space: SpaceId(0),
                        x: 8,
                        y: 2,
                        z: 11
                    },
                    expected: Cell::AIR,
                    sneaking: false,
                    inventory: guard
                }
            })
            .is_err()
        );
    }
}
