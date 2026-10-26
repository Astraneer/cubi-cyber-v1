use net::item_attributes::*;
fn frame() -> InventoryAttributes {
    InventoryAttributes {
        inventory_revision: 0x0102030405060708,
        rows: vec![
            ItemAttribute {
                slot: 0,
                item: CONFIGURATOR_ITEM,
                payload: 0x1112131415161718,
                attribute: Attribute::Configurator(4),
            },
            ItemAttribute {
                slot: 40,
                item: ENERGY_TABLET_ITEM,
                payload: 0x2122232425262728,
                attribute: Attribute::Charge(180_000),
            },
        ],
    }
}
#[test]
fn owned_item_facts_match_independent_little_endian_golden_without_ledger_bytes() {
    let expected = "49415431 0807060504030201 02 000000 00 00 4701 1817161514131211 04000000 0000000000000000 28 01 3901 2827262524232221 20bf0200 0000000000000000";
    let bytes: Vec<_> = expected
        .split_whitespace()
        .flat_map(|s| {
            (0..s.len())
                .step_by(2)
                .map(move |i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        })
        .collect();
    assert_eq!(bytes.len(), 64);
    assert_eq!(frame().encode().unwrap(), bytes);
    assert_eq!(InventoryAttributes::decode(&bytes).unwrap(), frame());
}
#[test]
fn delayed_attributes_require_exact_owned_revision_slot_item_and_payload() {
    let facts = frame();
    assert_eq!(
        facts.matching(0, 327, 0x1112131415161718, facts.inventory_revision),
        Some(Attribute::Configurator(4))
    );
    for (slot, item, payload, revision) in [
        (1, 327, 0x1112131415161718, facts.inventory_revision),
        (0, 313, 0x1112131415161718, facts.inventory_revision),
        (0, 327, 0x1112131415161719, facts.inventory_revision),
        (0, 327, 0x1112131415161718, facts.inventory_revision + 1),
    ] {
        assert_eq!(facts.matching(slot, item, payload, revision), None);
    }
}
#[test]
fn tag_type_ref_charge_quantization_and_saved_machine_guards_reject_invalid_facts() {
    for row in [
        ItemAttribute {
            slot: 0,
            item: 327,
            payload: 0,
            attribute: Attribute::Configurator(1),
        },
        ItemAttribute {
            slot: 0,
            item: 327,
            payload: 1,
            attribute: Attribute::Configurator(5),
        },
        ItemAttribute {
            slot: 0,
            item: 313,
            payload: 1,
            attribute: Attribute::Configurator(2),
        },
        ItemAttribute {
            slot: 0,
            item: 313,
            payload: 0,
            attribute: Attribute::Charge(20),
        },
        ItemAttribute {
            slot: 0,
            item: 313,
            payload: 1,
            attribute: Attribute::Charge(0),
        },
        ItemAttribute {
            slot: 0,
            item: 313,
            payload: 1,
            attribute: Attribute::Charge(19),
        },
        ItemAttribute {
            slot: 0,
            item: 313,
            payload: 1,
            attribute: Attribute::Charge(1_000_020),
        },
        ItemAttribute {
            slot: 0,
            item: 63,
            payload: 0,
            attribute: Attribute::SavedMachine(63),
        },
        ItemAttribute {
            slot: 0,
            item: 64,
            payload: 1,
            attribute: Attribute::SavedMachine(63),
        },
        ItemAttribute {
            slot: 0,
            item: 65,
            payload: 1,
            attribute: Attribute::SavedMachine(65),
        },
    ] {
        assert!(row.validate().is_err(), "{row:?}");
    }
    for mode in 0..=4 {
        ItemAttribute {
            slot: 0,
            item: 327,
            payload: u64::from(mode),
            attribute: Attribute::Configurator(mode),
        }
        .validate()
        .unwrap();
    }
    for joules in [0, 20, 180_000, TABLET_CAPACITY] {
        ItemAttribute {
            slot: 40,
            item: 313,
            payload: u64::from(joules),
            attribute: Attribute::Charge(joules),
        }
        .validate()
        .unwrap();
    }
    for block in [63, 64, 66, 70, 71, 72, 74, 75, 76, 80] {
        ItemAttribute {
            slot: 0,
            item: block,
            payload: 1,
            attribute: Attribute::SavedMachine(block),
        }
        .validate()
        .unwrap();
    }
}
#[test]
fn all_truncated_prefixes_unknown_tags_reserved_bytes_duplicates_and_capacity_reject() {
    let bytes = frame().encode().unwrap();
    for len in 0..bytes.len() {
        assert!(
            InventoryAttributes::decode(&bytes[..len]).is_err(),
            "prefix{len}"
        );
    }
    for offset in [13, HEADER_BYTES + 16] {
        let mut bad = bytes.clone();
        bad[offset] = 1;
        assert!(InventoryAttributes::decode(&bad).is_err());
    }
    let mut bad = bytes.clone();
    bad[HEADER_BYTES + 1] = 3;
    assert!(InventoryAttributes::decode(&bad).is_err());
    let mut bad = bytes.clone();
    bad[12] = 255;
    assert!(InventoryAttributes::decode(&bad).is_err());
    let mut bad = frame();
    bad.rows[1].slot = 0;
    assert!(bad.encode().is_err());
    bad.rows[1].slot = 41;
    assert!(bad.encode().is_err());
    let full = InventoryAttributes {
        inventory_revision: 9,
        rows: (0..41)
            .map(|slot| ItemAttribute {
                slot,
                item: 313,
                payload: 0,
                attribute: Attribute::Charge(0),
            })
            .collect(),
    };
    let encoded = full.encode().unwrap();
    assert_eq!(encoded.len(), MAX_BYTES);
    assert_eq!(encoded.len(), 1000);
    assert_eq!(InventoryAttributes::decode(&encoded).unwrap(), full);
    let empty = InventoryAttributes {
        inventory_revision: 0,
        rows: vec![],
    };
    assert_eq!(empty.encode().unwrap().len(), HEADER_BYTES);
    assert_eq!(
        InventoryAttributes::decode(&empty.encode().unwrap()).unwrap(),
        empty
    );
}
