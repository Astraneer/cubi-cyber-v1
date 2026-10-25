// The module remains unexported until the coordinated stack codec checkpoint.
pub use rules::{ItemId, blocks, items, machines};
#[path = "../src/item_payload.rs"]
mod item_payload;
use item_payload::*;
use rules::{
    blocks::MekKind,
    items::*,
    machines::{Machine, Slot, Tank},
    stack::ItemStack,
};
use telemetry::{AllocationCounts, AllocationScope, CountingAllocator};
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator::new(std::alloc::System);
#[test]
fn entire_machine_is_one_immutable_record_and_exactly_resolves_after_restart() {
    // Approved game/mek_tests.rs275..286 preserves the complete block entity.
    let mut machine = Machine::new(MekKind::Purifier);
    machine.energy = 19_920;
    machine.progress = 17;
    machine.tank = Tank {
        substance: rules::machine_recipes::SUB_OXYGEN,
        amount: 73,
    };
    machine.slots[0] = Slot::new(ItemStack::new(I_RAW_IRON, 10).unwrap());
    machine.slots[3] = Slot::tablet(180_000);
    machine.eject = true;
    machine.eject_delay = 7;
    let payload = Payload::machine(&machine).unwrap();
    let id = payload.id();
    assert_ne!(id.0, 0);
    assert_eq!(Record::new(payload.clone()).id, id);
    assert_eq!(payload.resolve_machine(payload.item()).unwrap(), machine);
    let mut ledger = Ledger::new(Config::default()).unwrap();
    let delta = ledger
        .extend(ledger.plan().unwrap(), [payload.clone()], [])
        .unwrap();
    assert_eq!(ledger.len(), 0);
    assert_eq!(ledger.revision(), 0);
    assert_eq!(ledger.byte_len(), 16);
    assert_eq!(delta.insertions().len(), 1);
    let delta = Delta::decode(&delta.encode().unwrap()).unwrap();
    ledger.acknowledge(delta).unwrap();
    let snapshot = ledger.snapshot();
    let bytes = snapshot.encode(Config::default()).unwrap();
    let mut restored = Ledger::restore(
        Config::default(),
        &Snapshot::decode(&bytes, Config::default()).unwrap(),
    )
    .unwrap();
    assert_eq!(
        restored
            .resolve(id, payload.item())
            .unwrap()
            .resolve_machine(payload.item())
            .unwrap(),
        machine
    );
    let duplicate = restored
        .extend(restored.plan().unwrap(), [payload.clone(), payload], [])
        .unwrap();
    assert!(duplicate.is_empty());
    restored.acknowledge(duplicate).unwrap();
    assert_eq!(restored.len(), 1);
    assert!(restored.resolve(id, I_IRON_INGOT).is_err());
}
#[test]
fn charge_has_explicit_item_and_exact_twenty_joule_units() {
    let payload = Payload::charge(1_000_000).unwrap();
    assert_eq!(payload.item(), I_ENERGY_TABLET);
    assert_eq!(payload.joules(I_ENERGY_TABLET), Ok(1_000_000));
    assert!(payload.joules(I_RAW_IRON).is_err());
    for value in [0, 1, 19, 21, 1_000_001] {
        assert!(Payload::charge(value).is_err());
    }
    assert_eq!(Payload::decode(payload.bytes()).unwrap(), payload);
    assert_eq!(payload.bytes(), [2, 57, 1, 64, 66, 15, 0]);
    // An independent fixed record guards changes to canonical hash/byte order.
    assert_eq!(payload.id().0, 0xa6b9_978c_704b_5a26);
}
#[test]
fn bounded_plan_composition_reuses_content_and_rejects_stale_or_late_invalid_records() {
    let config = Config {
        records: 1,
        bytes: 1024,
    };
    let mut ledger = Ledger::new(config).unwrap();
    let one = Payload::charge(20).unwrap();
    let two = Payload::charge(40).unwrap();
    let delta = ledger
        .extend(ledger.plan().unwrap(), [one.clone()], [])
        .unwrap();
    assert!(ledger.extend(delta.clone(), [two.clone()], []).is_err());
    assert!(ledger.is_empty());
    ledger.acknowledge(delta.clone()).unwrap();
    assert_eq!(ledger.acknowledge(delta), Err(Error::Stale));
    let delta = ledger
        .extend(ledger.plan().unwrap(), [two.clone()], [one.id()])
        .unwrap();
    assert_eq!(delta.removals().len(), 1);
    ledger.acknowledge(delta).unwrap();
    assert_eq!(ledger.get(two.id()), Some(&two));
    let mut snapshot = ledger.snapshot();
    snapshot.records[0].id = PayloadId(1);
    assert!(Ledger::restore(config, &snapshot).is_err());
    let mut bytes = ledger.snapshot().encode(config).unwrap();
    bytes.push(0);
    assert!(Snapshot::decode(&bytes, config).is_err());
    let mut bytes = ledger.snapshot().encode(config).unwrap();
    bytes[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(Snapshot::decode(&bytes, config).is_err());
    assert_eq!(ledger.len(), 1);
}
#[test]
fn empty_owner_polls_and_resolving_do_not_allocate_or_scan_records() {
    let mut ledger = Ledger::new(Config::default()).unwrap();
    let payload = Payload::charge(200).unwrap();
    let id = payload.id();
    ledger
        .acknowledge(
            ledger
                .extend(ledger.plan().unwrap(), [payload], [])
                .unwrap(),
        )
        .unwrap();
    let scope = AllocationScope::begin().unwrap();
    for _ in 0..10_000 {
        assert_eq!(
            ledger
                .resolve(id, I_ENERGY_TABLET)
                .unwrap()
                .joules(I_ENERGY_TABLET),
            Ok(200)
        );
        assert!(ledger.plan().unwrap().is_empty());
    }
    assert_eq!(scope.finish(), AllocationCounts::default());
}
#[test]
fn sparse_refs_preserve_legacy_prefix_and_reject_zero_duplicates_or_out_of_range() {
    let mut bytes = b"legacy-prefix".to_vec();
    refs::append(&mut bytes, [(0, None), (1, None)]).unwrap();
    assert_eq!(bytes, b"legacy-prefix");
    let start = bytes.len();
    refs::append(
        &mut bytes,
        [
            (0, Some(PayloadId(0x1122334455667788))),
            (1, None),
            (40, Some(PayloadId(7))),
        ],
    )
    .unwrap();
    assert_eq!(&bytes[start..start + 6], b"XRF1\x02\x00");
    let (decoded, used) = refs::read(&bytes[start..], 41).unwrap();
    assert_eq!(used, 26);
    assert_eq!(
        decoded,
        [(0, PayloadId(0x1122334455667788)), (40, PayloadId(7))]
    );
    for n in 4..26 {
        assert!(refs::read(&bytes[start..start + n], 41).is_err());
    }
    let mut zero = bytes[start..].to_vec();
    zero[8..16].fill(0);
    assert!(refs::read(&zero, 41).is_err());
    let mut duplicate = bytes[start..].to_vec();
    duplicate[16..18].fill(0);
    assert!(refs::read(&duplicate, 41).is_err());
    assert!(refs::read(&bytes[start..], 40).is_err());
    assert!(refs::append(&mut Vec::new(), [(0, Some(PayloadId(0)))]).is_err());
}
#[test]
fn quota_counts_wire_headers_exactly() {
    let payload = Payload::charge(20).unwrap();
    let config = Config {
        records: 1,
        bytes: 33,
    }; //16B snapshot+10B record+7B charge.
    let mut ledger = Ledger::new(config).unwrap();
    ledger
        .acknowledge(
            ledger
                .extend(ledger.plan().unwrap(), [payload.clone()], [])
                .unwrap(),
        )
        .unwrap();
    assert_eq!(ledger.byte_len(), 33);
    assert_eq!(ledger.snapshot().encode(config).unwrap().len(), 33);
    let ledger = Ledger::new(Config {
        records: 1,
        bytes: 32,
    })
    .unwrap();
    assert!(
        ledger
            .extend(ledger.plan().unwrap(), [payload], [])
            .is_err()
    );
}
#[test]
fn inline_encoded_values_have_exact_legacy_length_and_zero_allocations() {
    let scope = AllocationScope::begin().unwrap();
    for _ in 0..10_000 {
        let mut bytes = Encoded::<33>::from_prefix(b"INV1-prefix");
        bytes.append_refs([(0, None)]).unwrap();
        assert_eq!(&*bytes, b"INV1-prefix");
        bytes.append_refs([(0, Some(PayloadId(7)))]).unwrap();
        assert_eq!(bytes.len(), 27);
        assert_eq!(bytes.as_ref()[11..15], *b"XRF1");
        bytes[0] = b'X';
        assert_eq!(bytes[0], b'X');
        let before = bytes.len();
        assert!(bytes.append_refs([(0, Some(PayloadId(8)))]).is_err());
        assert_eq!(bytes.len(), before);
    }
    assert_eq!(scope.finish(), AllocationCounts::default());
}
