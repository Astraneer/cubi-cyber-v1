use core_types::{BlockPos, Cell, ChannelMask, ComponentId, SpaceId, Tick};
use rules::item_payload::{self, Ledger, Payload};
use rules::{
    blocks::MekKind,
    ids::*,
    items::*,
    machines::{Machine, SideMode, Slot},
    stack::ItemStack,
    survival::furnace::Chest,
};
use server::machines::*;
use world::{Transaction, World};
fn p(x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x,
        y,
        z,
    }
}
fn commit(w: &mut World, r: &mut MachineRuntime, plan: PhasePlan) {
    r.validate(w, &plan).unwrap();
    for tx in plan.transactions {
        w.commit(tx).unwrap();
    }
    r.acknowledge(plan.delta).unwrap();
}
fn fixture(pull: bool, cube: bool) -> (World, MachineRuntime) {
    let mut w = World::new(rules::properties, 128);
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    tx.create_page(p(0, 0, 0).page(), vec![Cell::AIR; 4096]);
    let mut source = Machine::new(MekKind::Smelter);
    source.eject = !pull;
    source.sides = [SideMode::Output; 6];
    source.slots[1] = Slot {
        stack: ItemStack::new(I_ENERGY_TABLET, 1).unwrap(),
        charge: 180_000,
    };
    tx.set_cell(p(4, 4, 4), Cell::from_parts(ENERGIZED_SMELTER, 3));
    tx.set_component(
        ComponentId(1),
        machine_component(p(4, 4, 4), &source),
        ChannelMask::CONTENTS,
    );
    let target = if pull { p(7, 4, 4) } else { p(5, 4, 4) };
    if cube {
        let mut m = Machine::new(MekKind::Cube);
        m.sides = [SideMode::Both; 6];
        tx.set_cell(target, Cell::from_parts(ENERGY_CUBE, 3));
        tx.set_component(
            ComponentId(2),
            machine_component(target, &m),
            ChannelMask::CONTENTS,
        );
    } else {
        tx.set_cell(target, Cell::from_parts(CHEST, 0));
        tx.set_component(
            ComponentId(2),
            server::survival::chest_component(target, &Chest::default()),
            ChannelMask::CONTENTS,
        );
    }
    let mut plain = Machine::new(MekKind::Smelter);
    plain.energy = 10_000;
    plain.slots[0] = Slot::new(ItemStack::new(I_DUST_IRON, 1).unwrap());
    tx.set_cell(p(8, 8, 8), Cell::from_parts(ENERGIZED_SMELTER, 3));
    tx.set_component(
        ComponentId(3),
        machine_component(p(8, 8, 8), &plain),
        ChannelMask::CONTENTS,
    );
    if pull {
        for x in 5..=6 {
            tx.set_cell(p(x, 4, 4), Cell::from_parts(LOGISTICAL_TRANSPORTER, 0));
        }
    }
    w.commit(tx).unwrap();
    let mut r = MachineRuntime::new(Config::default()).unwrap();
    let ids = if cube {
        vec![ComponentId(1), ComponentId(2), ComponentId(3)]
    } else {
        vec![ComponentId(1), ComponentId(3)]
    };
    let delta = r
        .extend_registration(r.noop_delta().unwrap(), &w, ids, Tick(0))
        .unwrap();
    r.acknowledge(delta).unwrap();
    if pull {
        let plan = r
            .plan_topology(&w, &[p(5, 4, 4), p(6, 4, 4)], Tick(0), Budget::default())
            .unwrap();
        commit(&mut w, &mut r, plan);
        let plan = r
            .plan_connections(
                &w,
                TransmitterConfig {
                    position: p(5, 4, 4),
                    block: LOGISTICAL_TRANSPORTER,
                    modes: 2 << (2 * 4),
                    color: 0,
                },
                Tick(0),
                Budget::default(),
            )
            .unwrap();
        commit(&mut w, &mut r, plan);
    }
    (w, r)
}
fn full(payload: Payload) -> Ledger {
    let mut l = Ledger::new(item_payload::Config {
        records: 1,
        bytes: 4096,
    })
    .unwrap();
    let delta = l.extend(l.plan().unwrap(), [payload], []).unwrap();
    l.acknowledge(delta).unwrap();
    l
}
#[test]
fn full_record_or_byte_quota_blocks_only_owned_ejection_or_pull_and_plain_recipe_keeps_ticking() {
    for pull in [false, true] {
        for bytes_full in [false, true] {
            let (mut w, mut r) = fixture(pull, false);
            let used = Payload::charge(20).unwrap();
            let mut ledger = Ledger::new(item_payload::Config {
                records: if bytes_full { 10 } else { 1 },
                bytes: if bytes_full {
                    16 + 10 + used.bytes().len()
                } else {
                    4096
                },
            })
            .unwrap();
            let delta = ledger.extend(ledger.plan().unwrap(), [used], []).unwrap();
            ledger.acknowledge(delta).unwrap();
            let before = ledger.snapshot();
            for tick in 1..=4 {
                let plan = r
                    .plan_tick_with_payloads(&w, &ledger, Tick(tick), Budget::default())
                    .unwrap();
                assert!(plan.owner_complete());
                if tick == 1 {
                    assert!(plan.work.payload_deferred > 0);
                }
                assert!(plan.payloads.is_empty());
                commit(&mut w, &mut r, plan);
                assert!(!r.owner_pending());
                assert_eq!(r.packet_count(), 0);
                let source = read_machine(&w, ComponentId(1)).unwrap();
                assert_eq!(
                    (source.slots[1].stack.count(), source.slots[1].charge),
                    (1, 180_000)
                );
                let plain = read_machine(&w, ComponentId(3)).unwrap();
                assert_eq!(
                    (plain.progress, plain.energy),
                    (tick as u32, 10_000 - 50 * tick)
                );
                assert_eq!(ledger.snapshot(), before);
            }
            let bytes = r.snapshot().encode().unwrap();
            r = MachineRuntime::restore(
                Config::default(),
                &RuntimeSnapshot::decode(&bytes, Config::default()).unwrap(),
            )
            .unwrap();
            assert!(!r.owner_pending());
        }
    }
}
#[test]
fn full_ledger_reuses_owned_identical_record_and_allows_numeric_machine_to_machine_charge() {
    for numeric in [false, true] {
        let (mut w, mut r) = fixture(false, numeric);
        let ledger = full(Payload::charge(if numeric { 20 } else { 180_000 }).unwrap());
        let plan = r
            .plan_tick_with_payloads(&w, &ledger, Tick(1), Budget::default())
            .unwrap();
        assert!(plan.owner_complete());
        assert!(plan.payloads.is_empty());
        commit(&mut w, &mut r, plan);
        assert!(
            read_machine(&w, ComponentId(1)).unwrap().slots[1]
                .stack
                .is_empty()
        );
        if numeric {
            let cube = read_machine(&w, ComponentId(2)).unwrap();
            assert_eq!(cube.energy + u64::from(cube.slots[0].charge), 180_000);
            assert!(cube.slots[0].stack.payload().is_none());
        } else {
            let chest = Chest::decode(&w.component(ComponentId(2)).unwrap().data.payload).unwrap();
            assert_eq!(
                chest.slots()[0].payload(),
                Some(Payload::charge(180_000).unwrap().id())
            );
        }
    }
}
#[test]
fn pending_effective_view_reserves_last_record_before_machine_transfer() {
    let (mut w, mut r) = fixture(false, false);
    let ledger = Ledger::new(item_payload::Config {
        records: 1,
        bytes: 4096,
    })
    .unwrap();
    let delta = ledger
        .extend(ledger.plan().unwrap(), [Payload::charge(20).unwrap()], [])
        .unwrap();
    let view = ledger.view(&delta).unwrap();
    let plan = r
        .plan_tick_with_payloads(&w, &view, Tick(1), Budget::default())
        .unwrap();
    assert!(plan.owner_complete() && plan.payloads.is_empty() && plan.work.payload_deferred > 0);
    commit(&mut w, &mut r, plan);
    assert_eq!(
        read_machine(&w, ComponentId(1)).unwrap().slots[1].charge,
        180_000
    );
    assert_eq!(read_machine(&w, ComponentId(3)).unwrap().progress, 1);
    assert!(ledger.is_empty());
}
