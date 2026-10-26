use net::*;
#[test]
fn owned_entity_requests_and_respawn_are_bounded_and_reject_invalid_guards() {
    let inventory = InventoryGuard {
        revision: 1,
        selected: 0,
        item: 294,
        count: 1,
        damage: 0,
        payload: 0,
    };
    for action in [
        Action::AttackEntity {
            target: 90,
            inventory,
        },
        Action::UseEntity {
            target: 90,
            inventory,
        },
        Action::Respawn,
    ] {
        let message = ClientMessage::AimedAction {
            id: ActionId(7),
            action,
            yaw: 180.,
            pitch: 0.,
        };
        let b = encode_client(&message).unwrap();
        assert!(b.len() <= 80);
        assert_eq!(decode_client(&b).unwrap(), message);
        for n in 0..b.len() {
            assert!(decode_client(&b[..n]).is_err());
        }
    }
    assert!(
        encode_client(&ClientMessage::Action {
            id: ActionId(1),
            action: Action::AttackEntity {
                target: 0,
                inventory
            }
        })
        .is_err()
    );
    assert!(
        encode_client(&ClientMessage::Action {
            id: ActionId(1),
            action: Action::UseEntity {
                target: 90,
                inventory: InventoryGuard {
                    count: 2,
                    payload: 1,
                    ..inventory
                }
            }
        })
        .is_err()
    );
}
