use client::network::{Connection, NetworkConfig};
use net::{
    ServerMessage,
    item_attributes::{Attribute, InventoryAttributes, ItemAttribute},
};
use rules::{
    item_payload::{ConfiguratorMode, Payload},
    items::I_CONFIGURATOR,
    stack::ItemStack,
    survival::Inventory,
};
fn image(revision: u64, mode: ConfiguratorMode) -> InventoryAttributes {
    InventoryAttributes {
        inventory_revision: revision,
        rows: vec![ItemAttribute {
            slot: 0,
            item: I_CONFIGURATOR,
            payload: Payload::configurator(mode).id().0,
            attribute: Attribute::Configurator(mode as u8),
        }],
    }
}
fn receive(connection: &mut Connection, message: ServerMessage) {
    let bytes = net::encode_server(&message).unwrap();
    connection
        .receive(net::decode_server(&bytes).unwrap(), 0)
        .unwrap();
}
#[test]
fn facts_are_visible_only_for_the_exact_owned_stack_revision_and_stale_frames_cannot_restore_them()
{
    let mut connection = Connection::new(NetworkConfig::default()).unwrap();
    let mut inventory = Inventory::empty();
    let stack = ItemStack::new(I_CONFIGURATOR, 1)
        .unwrap()
        .with_payload(Payload::configurator(ConfiguratorMode::Wrench).id())
        .unwrap();
    inventory.set_trusted(0, stack).unwrap();
    let attributes = image(inventory.revision(), ConfiguratorMode::Wrench);
    receive(
        &mut connection,
        ServerMessage::ItemAttributes(Box::new(attributes.clone())),
    );
    assert!(
        connection.item_attributes().is_none(),
        "facts cannot invent an owned inventory"
    );
    receive(
        &mut connection,
        ServerMessage::Inventory {
            data: inventory.encode().to_vec(),
        },
    );
    assert_eq!(
        connection.item_attributes().unwrap().matching(
            0,
            stack.item(),
            stack.payload().unwrap().0,
            inventory.revision()
        ),
        Some(Attribute::Configurator(3))
    );
    let mut conflicting = attributes.clone();
    conflicting.rows[0].attribute = Attribute::Configurator(4);
    assert!(
        connection
            .receive(ServerMessage::ItemAttributes(Box::new(conflicting)), 0)
            .is_err()
    );
    assert_eq!(
        connection.item_attributes(),
        Some(&attributes),
        "conflict rejection is atomic"
    );
    inventory.set_trusted(0, ItemStack::EMPTY).unwrap();
    receive(
        &mut connection,
        ServerMessage::Inventory {
            data: inventory.encode().to_vec(),
        },
    );
    assert!(
        connection.item_attributes().is_none(),
        "empty/new held state invalidates old facts immediately"
    );
    receive(
        &mut connection,
        ServerMessage::ItemAttributes(Box::new(InventoryAttributes {
            inventory_revision: inventory.revision(),
            rows: Vec::new(),
        })),
    );
    receive(
        &mut connection,
        ServerMessage::ItemAttributes(Box::new(attributes)),
    );
    assert!(
        connection.item_attributes().unwrap().rows.is_empty(),
        "delayed old mode cannot describe the new inventory"
    );
}
