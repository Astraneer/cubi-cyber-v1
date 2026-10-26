use rules::item_payload::{Capacity, Config, Ledger, MAX_DELTA_RECORDS, Payload, Resolver};

#[test]
fn effective_view_capacity_accounts_pending_bytes_records_removals_and_delta_changes() {
    let first = Payload::charge(20).unwrap();
    let cost = 10 + first.bytes().len();
    let mut ledger = Ledger::new(Config {
        records: 2,
        bytes: 16 + 2 * cost,
    })
    .unwrap();
    assert_eq!(
        ledger.remaining_capacity(),
        Some(Capacity {
            records: 2,
            bytes: 2 * cost,
            changes: MAX_DELTA_RECORDS
        })
    );
    let delta = ledger
        .extend(ledger.plan().unwrap(), [first.clone()], [])
        .unwrap();
    let view = ledger.view(&delta).unwrap();
    assert_eq!(
        view.remaining_capacity(),
        Some(Capacity {
            records: 1,
            bytes: cost,
            changes: MAX_DELTA_RECORDS - 1
        })
    );
    assert_eq!(view.resolve(first.id(), first.item()).unwrap(), &first);
    assert!(view.remaining_capacity().unwrap().allows(1, cost));
    assert!(!view.remaining_capacity().unwrap().allows(1, cost + 1));
    ledger.acknowledge(delta).unwrap();
    let delta = ledger
        .extend(ledger.plan().unwrap(), [], [first.id()])
        .unwrap();
    let view = ledger.view(&delta).unwrap();
    assert_eq!(
        view.remaining_capacity(),
        Some(Capacity {
            records: 2,
            bytes: 2 * cost,
            changes: MAX_DELTA_RECORDS - 1
        })
    );
    assert!(view.resolve(first.id(), first.item()).is_err());
}
#[test]
fn affected_record_budget_can_be_full_while_persistent_storage_still_has_room() {
    let ledger = Ledger::new(Config {
        records: 2048,
        bytes: 1024 * 1024,
    })
    .unwrap();
    let rows = (1..=MAX_DELTA_RECORDS).map(|i| Payload::charge(i as u32 * 20).unwrap());
    let delta = ledger.extend(ledger.plan().unwrap(), rows, []).unwrap();
    let view = ledger.view(&delta).unwrap();
    let capacity = view.remaining_capacity().unwrap();
    assert_eq!((capacity.records, capacity.changes), (1024, 0));
    assert!(capacity.bytes > 0);
    assert!(!capacity.allows(1, 17));
    assert!(capacity.allows(0, 0));
    assert_eq!(ledger.len(), 0);
}
