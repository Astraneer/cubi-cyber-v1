//! Independent numeric expectations from the pinned MEK behavior references.
use rules::{
    blocks::MekKind,
    items::*,
    machine_recipes as chem,
    machines::{
        events::{self, EventWork},
        logistics::{self, Arrival, Route, Transit},
        sides, *,
    },
    stack::ItemStack,
};
use std::{alloc::System, hint::black_box};
use telemetry::{AllocationScope, CountingAllocator};
#[global_allocator]
static ALLOCATOR: CountingAllocator<System> = CountingAllocator::new(System);

#[derive(Default)]
struct World {
    lava: u8,
    sky: bool,
    sink: bool,
    energy: u64,
    source: Option<u8>,
    drained: u32,
    substance: u32,
    item_ticks: Vec<u64>,
    tick: u64,
}
impl MachineWorld for World {
    fn lava_neighbors(&mut self, _: [i32; 3]) -> u8 {
        self.lava
    }
    fn sky_open(&mut self, _: [i32; 3]) -> bool {
        self.sky
    }
    fn source(&mut self, _: [i32; 3]) -> Option<u8> {
        self.source
    }
    fn drain_lava(&mut self, _: [i32; 3]) -> bool {
        self.drained += 1;
        true
    }
    fn push_energy(&mut self, _: [i32; 3], _: Face, n: u64) -> u64 {
        if self.sink {
            self.energy += n;
            n
        } else {
            0
        }
    }
    fn push_substance(&mut self, _: [i32; 3], _: Face, _: u8, n: u32) -> u32 {
        if self.sink {
            self.substance += n;
            n
        } else {
            0
        }
    }
    fn push_items(&mut self, _: [i32; 3], _: Face, s: ItemStack) -> u16 {
        if self.sink {
            self.item_ticks.push(self.tick);
            s.count()
        } else {
            0
        }
    }
}
fn stack(id: u16, n: u16) -> Slot {
    Slot::new(ItemStack::new(id, n).unwrap())
}
fn tick(m: &mut Machine, w: &mut World) -> Step {
    w.tick += 1;
    m.tick([0, 64, 0], Face::South, w)
}

#[test]
fn passive_and_fueled_heat_match_reference_traces() {
    for (lava, power) in [(1, 4), (2, 16), (3, 33), (4, 52), (5, 73), (6, 96)] {
        let mut m = Machine::new(MekKind::Heat);
        let mut w = World {
            lava,
            sink: true,
            ..World::default()
        };
        for _ in 0..2000 {
            tick(&mut m, &mut w);
        }
        assert_eq!(m.produced, power);
        if lava == 4 {
            assert!((m.heat / 10.0 - 537.18).abs() < 0.01);
        }
        assert!(!m.active.shown);
    }
    let mut m = Machine::new(MekKind::Heat);
    let mut w = World {
        lava: 4,
        sink: true,
        ..World::default()
    };
    let mut first = Vec::new();
    for i in 0..200 {
        tick(&mut m, &mut w);
        if i < 12 {
            first.push(m.produced);
        }
        if [19, 59, 99, 199].contains(&i) {
            assert_eq!(
                m.produced,
                match i {
                    19 => 24,
                    59 => 48,
                    99 => 51,
                    _ => 52,
                }
            );
        }
    }
    assert_eq!(first, [0, 0, 0, 1, 2, 4, 5, 7, 8, 10, 11, 13]);
    let mut m = Machine::new(MekKind::Heat);
    m.slots[0] = stack(I_COAL, 1);
    let mut w = World {
        sink: true,
        ..World::default()
    };
    let mut first = Vec::new();
    let mut total = 0;
    for i in 0..200 {
        tick(&mut m, &mut w);
        total += m.produced;
        if i < 20 {
            first.push(m.produced);
        }
        if i == 0 {
            assert_eq!(m.tank.amount, 70);
        }
    }
    assert_eq!(
        first,
        [
            0, 0, 2, 4, 7, 10, 13, 17, 20, 19, 17, 15, 14, 13, 12, 11, 10, 9, 8, 7
        ]
    );
    assert_eq!(total, 262);
}

#[test]
fn heat_fuel_lava_bucket_and_saturated_buffer() {
    let mut m = Machine::new(MekKind::Heat);
    m.slots[0] = stack(I_LAVA_BUCKET, 1);
    let mut w = World {
        sink: true,
        ..World::default()
    };
    let mut burned = 0;
    let mut joules = 0;
    for _ in 0..300 {
        tick(&mut m, &mut w);
        burned += usize::from(m.active.logic);
        joules += m.produced;
    }
    assert_eq!(burned, 100);
    assert_eq!(joules, 9781);
    assert_eq!(m.slots[0].stack.item(), I_BUCKET);
    let mut m = Machine::new(MekKind::Heat);
    m.energy = m.capacity();
    m.slots[0] = stack(I_LAVA_BUCKET, 1);
    tick(&mut m, &mut World::default());
    assert_eq!(m.energy, 160_000);
    assert_eq!(m.tank.amount, 1000);
    assert!(!m.active.logic);
}

#[test]
fn steel_infuser_consumes_energy_and_carbon_at_exact_ticks() {
    let mut m = Machine::new(MekKind::Infuser);
    m.energy = 20_000;
    m.slots[0] = stack(I_COAL, 2);
    m.slots[1] = stack(I_IRON_INGOT, 1);
    let mut w = World::default();
    for t in 1..=200 {
        tick(&mut m, &mut w);
        if t == 1 {
            assert_eq!((m.tank.amount, m.progress, m.energy), (10, 1, 19_950));
        }
        if t < 200 {
            assert!(m.slots[2].stack.is_empty());
        }
    }
    assert_eq!(
        m.slots[2].stack,
        ItemStack::new(I_ENRICHED_IRON, 1).unwrap()
    );
    assert_eq!((m.tank.amount, m.energy), (10, 10_000));
    m.slots[1] = m.slots[2];
    m.slots[2] = Slot::default();
    for _ in 0..200 {
        tick(&mut m, &mut w);
    }
    assert_eq!(m.slots[2].stack.item(), I_DUST_STEEL);
    assert_eq!(m.energy, 0);
    assert_eq!(m.tank.amount, 0);
}

#[test]
fn chamber_and_crusher_all_recipe_outputs_tick_200() {
    for (kind, table) in [
        (MekKind::Enrich, &chem::ENRICH[..]),
        (MekKind::Crusher, &chem::CRUSH[..]),
    ] {
        for r in table {
            let mut m = Machine::new(kind);
            m.energy = 20_000;
            m.slots[0] = stack(r.input, r.in_n as u16);
            let mut w = World::default();
            for t in 1..=200 {
                tick(&mut m, &mut w);
                assert_eq!(
                    m.slots[1].stack.count(),
                    if t == 200 { r.out_n as u16 } else { 0 }
                );
            }
            assert_eq!(m.slots[1].stack.item(), r.out);
            assert_eq!(m.energy, 10_000);
        }
    }
}

#[test]
fn pause_holds_progress_but_wrong_input_resets() {
    let mut m = Machine::new(MekKind::Enrich);
    m.energy = 100;
    m.slots[0] = stack(I_RAW_IRON, 3);
    let mut w = World::default();
    for _ in 0..10 {
        tick(&mut m, &mut w);
    }
    assert_eq!((m.progress, m.last), (2, Step::NoEnergy));
    m.energy = 20_000;
    m.slots[1] = stack(I_DUST_IRON, 64);
    tick(&mut m, &mut w);
    assert_eq!((m.progress, m.last), (2, Step::NoRoom));
    m.slots[1] = stack(I_DUST_GOLD, 1);
    tick(&mut m, &mut w);
    assert_eq!((m.progress, m.last), (2, Step::WrongOutput));
    m.slots[0] = Slot::default();
    tick(&mut m, &mut w);
    assert_eq!((m.progress, m.last), (0, Step::Reset));
}

#[test]
fn purifier_gas_pause_and_fractional_upgrades() {
    let mut m = Machine::new(MekKind::Purifier);
    m.energy = 80_000;
    m.tank = Tank {
        substance: chem::SUB_OXYGEN,
        amount: 150,
    };
    m.slots[0] = stack(I_RAW_IRON, 1);
    let mut w = World::default();
    for _ in 0..300 {
        m.energy = 80_000;
        tick(&mut m, &mut w);
    }
    assert_eq!((m.progress, m.tank.amount, m.last), (150, 0, Step::NoGas));
    m.slots[1] = stack(I_FLINT, 5);
    for _ in 0..50 {
        m.energy = 80_000;
        tick(&mut m, &mut w);
    }
    assert_eq!(m.slots[2].stack, ItemStack::new(I_CLUMP_IRON, 2).unwrap());
    assert_eq!(m.tank.amount, 0);
    let mut m = Machine::new(MekKind::Purifier);
    m.install(0, 3);
    m.slots[0] = stack(I_RAW_IRON, 1);
    let mut spent = 0;
    let mut first = 0;
    let mut last = 0;
    for t in 0..84 {
        m.energy = m.capacity();
        m.tank = Tank {
            substance: chem::SUB_OXYGEN,
            amount: 210,
        };
        tick(&mut m, &mut w);
        let used = 210 - m.tank.amount;
        spent += used;
        if t == 0 {
            first = used;
        }
        last = used;
    }
    assert_eq!((spent, first, last), (474, 5, 6));
    assert_eq!(m.slots[2].stack.count(), 2);
}

#[test]
fn separator_400_ticks_and_output_jam_dump_order() {
    let mut m = Machine::new(MekKind::Separator);
    m.energy = 160_000;
    m.insert_substance(chem::SUB_WATER, 2000, true);
    let mut w = World::default();
    for _ in 0..400 {
        tick(&mut m, &mut w);
    }
    assert_eq!(
        (m.tank.amount, m.gas[0].amount, m.gas[1].amount, m.energy),
        (1200, 800, 400, 0)
    );
    tick(&mut m, &mut w);
    assert_eq!(m.last, Step::NoEnergy);
    let mut m = Machine::new(MekKind::Separator);
    m.insert_substance(chem::SUB_WATER, 24_000, true);
    for _ in 0..1200 {
        m.energy = 160_000;
        tick(&mut m, &mut w);
    }
    assert_eq!(m.gas[0].amount, 2400);
    m.gas_mode[0] = GasMode::Dump;
    m.energy = 160_000;
    tick(&mut m, &mut w);
    assert_eq!((m.last, m.gas[0].amount), (Step::HydrogenFull, 2392));
    tick(&mut m, &mut w);
    assert_eq!((m.last, m.gas[0].amount), (Step::Worked, 2386));
}

#[test]
fn wind_sky_is_rechecked_every_twenty_ticks() {
    for (y, j) in [
        (20, 60),
        (44, 157),
        (49, 178),
        (60, 223),
        (63, 235),
        (64, 239),
        (100, 386),
        (105, 406),
        (120, 467),
        (123, 480),
        (i32::MAX, 480),
        (i32::MIN, 60),
    ] {
        assert_eq!(wind_power(y), j);
    }
    let mut m = Machine::new(MekKind::Wind);
    let mut w = World {
        sky: true,
        ..World::default()
    };
    for t in 1..=45 {
        if t == 5 {
            w.sky = false;
        }
        if t == 30 {
            w.sky = true;
        }
        let before = m.energy;
        m.tick([0, 49, 0], Face::South, &mut w);
        assert_eq!(
            m.energy - before,
            if (21..=40).contains(&t) { 0 } else { 178 }
        );
    }
}

#[test]
fn tablets_are_typed_quantized_and_charge_in_exact_order() {
    let mut m = Machine::new(MekKind::Enrich);
    m.slots[2] = Slot::tablet(TABLET_CAPACITY);
    let mut w = World::default();
    for t in 1..=4 {
        tick(&mut m, &mut w);
        assert_eq!(m.energy, 5000 * t);
    }
    assert_eq!(m.slots[2].charge, 980_000);
    let mut m = Machine::new(MekKind::Cube);
    m.energy = 4_000_000;
    m.slots[1] = Slot::tablet(0);
    for t in 1..=250 {
        tick(&mut m, &mut w);
        assert_eq!(m.slots[1].charge, 4000 * t);
    }
    assert_eq!(m.slots[1].charge, TABLET_CAPACITY);
    let mut m = Machine::new(MekKind::Wind);
    m.slots[0] = Slot::tablet(0);
    w.sky = true;
    for _ in 0..3 {
        m.tick([0, 49, 0], Face::South, &mut w);
    }
    assert_eq!(m.slots[0].charge, 340);
}

#[test]
fn upgrade_install_tick21_and_no_runtime_pow() {
    let mut m = Machine::new(MekKind::Enrich);
    m.slots[3] = stack(I_UPGRADE_SPEED, 4);
    let mut w = World::default();
    for t in 1..=21 {
        tick(&mut m, &mut w);
        assert_eq!(m.upgrades[0], if t == 21 { 4 } else { 0 });
    }
    assert_eq!((m.ticks(), m.per_tick(), m.capacity()), (63, 500, 20_000));
    m.install(1, 4);
    assert_eq!((m.ticks(), m.per_tick(), m.capacity()), (63, 159, 63_245));
    let mut p = Machine::new(MekKind::Purifier);
    p.install(0, 1);
    assert_eq!(p.per_tick(), 356);
    p.install(1, 1);
    assert_eq!((p.per_tick(), p.capacity()), (267, 106_681));
    assert_eq!(Machine::new(MekKind::Cube).install(0, 8), 0);
}

#[test]
fn active_view_and_ejection_preserve_reference_cadence() {
    let mut a = ActiveView::default();
    let mut changes = Vec::new();
    for t in 1..=300 {
        let old = a.shown;
        a.countdown();
        if a.shown != old {
            changes.push((t, a.shown));
        }
        let old = a.shown;
        a.set(t <= 100 && t % 2 == 1);
        if a.shown != old {
            changes.push((t, a.shown));
        }
    }
    assert_eq!(changes, [(1, true), (2, false), (3, true), (160, false)]);
    let mut m = Machine::new(MekKind::Enrich);
    m.eject = true;
    let mut w = World {
        sink: true,
        ..World::default()
    };
    for t in 0..120 {
        m.eject = !(15..100).contains(&t);
        m.slots[1] = stack(I_DUST_IRON, 64);
        w.tick = t;
        m.tick([0; 3], Face::North, &mut w);
    }
    assert_eq!(w.item_ticks, [0, 11, 107, 118]);
    for facing in Face::ALL {
        for r in RelativeSide::ALL {
            assert_eq!(sides::relative(facing, sides::direction(facing, r)), r);
        }
    }
}

#[test]
fn pump_selects_source_once_then_charges_continuously() {
    let mut m = Machine::new(MekKind::Pump);
    m.energy = 40_000;
    let mut w = World {
        source: Some(chem::SUB_WATER),
        ..World::default()
    };
    for t in 1..=38 {
        tick(&mut m, &mut w);
        if t < 19 {
            assert_eq!(m.energy, 40_000);
            assert_eq!(m.tank.amount, 0);
        }
        if t == 19 {
            assert_eq!((m.energy, m.tank.amount), (39_900, 1000));
        }
    }
    assert_eq!((m.energy, m.tank.amount), (38_000, 2000));
    assert_eq!(w.drained, 0);
    let mut m = Machine::new(MekKind::Pump);
    m.energy = 40_000;
    let mut w = World {
        source: Some(chem::SUB_LAVA),
        sink: true,
        ..World::default()
    };
    for _ in 0..19 {
        tick(&mut m, &mut w);
    }
    assert_eq!(w.drained, 1);
    assert_eq!((m.tank.amount, w.substance), (744, 256));
}

#[test]
fn fixed_codec_restores_exact_continuation_and_rejects_bad_bytes() {
    for kind in [
        MekKind::Heat,
        MekKind::Infuser,
        MekKind::Enrich,
        MekKind::Smelter,
        MekKind::Crusher,
        MekKind::Purifier,
        MekKind::Separator,
        MekKind::Pump,
        MekKind::Wind,
        MekKind::Cube,
    ] {
        let mut m = Machine::new(kind);
        m.energy = m.capacity();
        let mut w = World {
            sky: true,
            source: Some(chem::SUB_WATER),
            ..World::default()
        };
        for _ in 0..37 {
            tick(&mut m, &mut w);
        }
        let mut raw = [0; MACHINE_BYTES];
        m.encode(&mut raw);
        let mut restored = Machine::decode(&raw).unwrap();
        assert_eq!(restored, m);
        for _ in 0..63 {
            tick(&mut m, &mut w);
            tick(&mut restored, &mut w);
            assert_eq!(restored, m);
        }
        for index in [0, 4, 94, 95] {
            let mut bad = raw;
            bad[index] = 255;
            assert!(Machine::decode(&bad).is_err());
        }
    }
}

#[test]
fn component_encoding_sends_only_actual_pump_frontier() {
    let m = Machine::new(MekKind::Enrich);
    let mut bytes = [255; MACHINE_BYTES];
    let used = m.encode(&mut bytes);
    assert_eq!(used, MACHINE_HEADER_BYTES);
    assert_eq!(Machine::decode(&bytes[..used]).unwrap(), m);
    assert!(bytes[used..].iter().all(|&b| b == 0));
    assert!(Machine::decode(&bytes[..used - 1]).is_err());
    let mut pump = Machine::new(MekKind::Pump);
    pump.energy = 40_000;
    let mut w = World {
        source: Some(chem::SUB_WATER),
        ..World::default()
    };
    for _ in 0..19 {
        tick(&mut pump, &mut w);
    }
    let used = pump.encode(&mut bytes);
    assert_eq!(used, MACHINE_HEADER_BYTES + 12);
    assert_eq!(Machine::decode(&bytes[..used]).unwrap(), pump);
    assert!(Machine::decode(&bytes[..used - 1]).is_err());
}

#[test]
fn fair_energy_allocation_matches_pinned_network_table() {
    let inf = u64::MAX / 4;
    for (total, requests, want) in [
        (100, vec![10, inf, inf], vec![10, 45, 45]),
        (11, vec![3, inf, inf], vec![3, 4, 4]),
        (7, vec![inf, 1, inf], vec![3, 1, 3]),
        (
            40_000,
            vec![8000; 9],
            vec![4445, 4445, 4445, 4445, 4444, 4444, 4444, 4444, 4444],
        ),
    ] {
        let mut actual = vec![0; requests.len()];
        logistics::fair_share(total, &requests, &mut actual).unwrap();
        assert_eq!(actual, want);
    }
}

#[test]
fn transit_is_exactly_one_step_per_tick_and_waits_for_commit_ack() {
    let mut t = Transit::new(2, 2);
    let route = t.add_route(Route::new(1, &[1, 2], 7).unwrap()).unwrap();
    let id = t
        .insert(ItemStack::new(I_RAW_IRON, 10).unwrap(), route, 0)
        .unwrap();
    let mut arrivals = Vec::<Arrival>::with_capacity(2);
    for tick in 1..=39 {
        t.tick(tick, &mut arrivals).unwrap();
        t.tick(tick, &mut arrivals).unwrap();
        assert!(arrivals.is_empty());
    }
    t.tick(40, &mut arrivals).unwrap();
    assert_eq!(arrivals[0].stack.count(), 10);
    assert_eq!(arrivals[0].destination, 7);
    assert_eq!(t.packets().len(), 1);
    t.acknowledge(id, 3).unwrap();
    assert_eq!(t.packets()[0].stack.count(), 7);
    t.acknowledge(id, 7).unwrap();
    assert!(t.packets().is_empty());
}

#[test]
fn h2_two_machine_chain_matches_detailed_ticks_for_120000_boundaries() {
    let mut a = Machine::new(MekKind::Crusher);
    a.slots[0] = stack(I_IRON_INGOT, 64);
    a.energy = a.capacity();
    let mut b = Machine::new(MekKind::Smelter);
    b.energy = b.capacity();
    let mut slow = [a, b];
    let mut fast = slow.clone();
    let mut world = World::default();
    let mut work = EventWork::default();
    let mut detailed_ticks = 0u64;
    for boundary in 0u64..120_000 {
        // External dependency events occur at the same global boundary in both
        // simulations: supply, loss/restoration of energy, output congestion and
        // transfer reservation ACK. No hidden per-tick work in the event path.
        for pair in [&mut slow, &mut fast] {
            if boundary % 31 == 0 {
                pair[0].slots[0] = stack(I_IRON_INGOT, 64);
            }
            if boundary % 17 != 0 {
                for m in pair.iter_mut() {
                    m.insert_energy(15_000);
                }
            } else {
                for m in pair.iter_mut() {
                    m.energy = 0;
                }
            }
            if boundary % 43 == 0 {
                pair[1].slots[1] = stack(I_IRON_INGOT, 64);
            } else if boundary % 43 == 3 {
                pair[1].slots[1] = Slot::default();
            }
            let out = pair[0].slots[1].stack;
            if !out.is_empty()
                && (pair[1].slots[0].stack.is_empty()
                    || pair[1].slots[0].stack.item() == out.item())
            {
                let n = out.count().min(64 - pair[1].slots[0].stack.count());
                if n > 0 {
                    pair[0].slots[1].stack = out.with_count(out.count() - n).unwrap();
                    pair[1].slots[0] = stack(out.item(), pair[1].slots[0].stack.count() + n);
                }
            }
        }
        let span = 1 + (boundary * 13 % 37);
        for _ in 0..span {
            for m in &mut slow {
                m.tick([0; 3], Face::South, &mut world);
                detailed_ticks += 1;
            }
        }
        for m in &mut fast {
            events::advance(m, span, &mut work).unwrap();
        }
        assert_eq!(slow, fast, "boundary {boundary}");
    }
    assert!(work.transitions >= 100_000);
    assert!(work.completed_operations > 10_000);
    assert!(
        detailed_ticks >= work.transitions * 4,
        "detailed {detailed_ticks}, events {}",
        work.transitions
    );
    eprintln!(
        "H2_EVENT_KERNEL boundaries=120000 detailed_ticks={detailed_ticks} event_transitions={} completed={} reduction={:.3}",
        work.transitions,
        work.completed_operations,
        detailed_ticks as f64 / work.transitions as f64
    );
}

#[test]
fn repeated_machine_and_idle_event_phases_allocate_nothing() {
    let mut m = Machine::new(MekKind::Enrich);
    let mut w = World::default();
    let mut work = EventWork::default();
    let positive = AllocationScope::begin().unwrap();
    black_box(Vec::<u64>::with_capacity(64));
    assert!(positive.finish().allocating_calls() > 0);
    let scope = AllocationScope::begin().unwrap();
    for _ in 0..10_000 {
        m.tick([0; 3], Face::South, &mut w);
        events::advance(&mut m, 1000, &mut work).unwrap();
    }
    let counts = scope.finish();
    assert_eq!(counts.allocating_calls(), 0);
    assert_eq!(counts.dealloc_calls, 0);
}

#[test]
fn h2_2048_copies_match_detailed_chain_under_shared_outages() {
    let mut crusher = Machine::new(MekKind::Crusher);
    crusher.slots[0] = stack(I_IRON_INGOT, 64);
    let smelter = Machine::new(MekKind::Smelter);
    let mut detailed = vec![[crusher, smelter]; 2048];
    let mut projected = detailed.clone();
    let mut world = World::default();
    let mut work = EventWork::default();
    for boundary in 0..20 {
        for (index, (slow, fast)) in detailed.iter_mut().zip(projected.iter_mut()).enumerate() {
            for pair in [&mut *slow, &mut *fast] {
                let powered = (boundary + index) % 7 != 0;
                for m in pair.iter_mut() {
                    if powered {
                        m.insert_energy(20_000);
                    } else {
                        m.energy = 0;
                    }
                }
                if boundary % 5 == 0 {
                    pair[1].slots[1] = if index % 3 == 0 {
                        stack(I_IRON_INGOT, 64)
                    } else {
                        Slot::default()
                    };
                }
                let output = pair[0].slots[1].stack;
                if !output.is_empty() && pair[1].slots[0].stack.count() + output.count() <= 64 {
                    pair[1].slots[0] = stack(
                        output.item(),
                        pair[1].slots[0].stack.count() + output.count(),
                    );
                    pair[0].slots[1] = Slot::default();
                }
            }
            for _ in 0..50 {
                for m in slow.iter_mut() {
                    m.tick([0; 3], Face::South, &mut world);
                }
            }
            for m in fast.iter_mut() {
                events::advance(m, 50, &mut work).unwrap();
            }
            assert_eq!(slow, fast, "copy {index},boundary {boundary}");
        }
    }
    let detailed_ticks = 2048u64 * 2 * 1000;
    assert!(detailed_ticks > work.transitions * 4);
    eprintln!(
        "H2_COPIES copies=2048 detailed_ticks={detailed_ticks} event_transitions={} reduction={:.3}",
        work.transitions,
        detailed_ticks as f64 / work.transitions as f64
    );
}

#[test]
fn indexed_wake_heap_coalesces_without_scanning_sleeping_machines() {
    use events::{Wake, WakeQueue};
    use std::collections::BTreeMap;
    let mut queue = WakeQueue::new(128);
    let mut expected = BTreeMap::<u32, Wake>::new();
    let mut random = 42u64;
    for tick in 0..50_000u64 {
        random ^= random << 13;
        random ^= random >> 7;
        random ^= random << 17;
        let wake = Wake {
            at: tick + ((random >> 16) % 20),
            machine: (random % 128) as u32,
            generation: ((random >> 32) % 20) as u32,
        };
        queue.schedule(wake).unwrap();
        if let Some(old) = expected.get_mut(&wake.machine) {
            if wake.generation > old.generation
                || (wake.generation == old.generation && wake.at < old.at)
            {
                *old = wake;
            }
        } else {
            expected.insert(wake.machine, wake);
        }
        let wanted = expected
            .values()
            .filter(|w| w.at <= tick)
            .min_by_key(|w| (w.at, w.machine))
            .copied();
        assert_eq!(queue.pop_due(tick), wanted);
        if let Some(w) = wanted {
            expected.remove(&w.machine);
        }
        assert_eq!(queue.len(), expected.len());
    }
    let mut queue = WakeQueue::new(4096);
    for machine in 0..4096 {
        queue
            .schedule(Wake {
                at: 1_000_000,
                machine,
                generation: 1,
            })
            .unwrap();
    }
    let scope = AllocationScope::begin().unwrap();
    for tick in 0..10_000 {
        assert_eq!(black_box(queue.pop_due(tick)), None);
    }
    let counts = scope.finish();
    assert_eq!(counts.allocating_calls(), 0);
    assert_eq!(counts.dealloc_calls, 0);
    assert_eq!(queue.len(), 4096);
}

#[test]
fn machine_sided_insert_rejects_wrong_roles_and_preserves_whole_stacks() {
    let mut m = Machine::new(MekKind::Smelter);
    let input = stack(I_DUST_IRON, 8);
    assert_eq!(
        m.insert_item(Face::North, Face::West, input, false),
        0,
        "west is output when facing north"
    );
    assert_eq!(
        m.insert_item(Face::North, Face::South, input, false),
        0,
        "south is energy slot"
    );
    assert_eq!(m.insert_item(Face::North, Face::Up, input, false), 8);
    assert_eq!(m.slots[0].stack.count(), 8);
    assert_eq!(
        m.insert_item(Face::North, Face::Up, stack(I_DUST_GOLD, 1), false),
        0
    );
    m.slots[0] = stack(I_DUST_IRON, 60);
    assert_eq!(m.insert_item(Face::North, Face::Up, input, true), 0);
    assert_eq!(m.slots[0].stack.count(), 60);
    assert_eq!(m.insert_item(Face::North, Face::Up, input, false), 4);
    assert_eq!(m.slots[0].stack.count(), 64);
}

#[test]
fn incremental_network_split_merge_preserves_reference_reservoir_shares() {
    use rules::machines::grid::{Grid, GridWork, Resource};
    let mut grid = Grid::new(1024, 8);
    let mut work = GridWork::default();
    for x in 0..10 {
        grid.add([x, 64, 0], Resource::Energy, 63, &mut work)
            .unwrap();
    }
    let original = grid.network_at([0, 64, 0]).unwrap();
    grid.insert([3, 64, 0], 0, 40_000).unwrap();
    work = GridWork::default();
    assert_eq!(grid.remove([4, 64, 0], &mut work).unwrap(), 0);
    let a = grid.network(grid.network_at([0, 64, 0]).unwrap()).unwrap();
    let b = grid.network(grid.network_at([9, 64, 0]).unwrap()).unwrap();
    assert_eq!(
        (a.members().len(), a.stored, b.members().len(), b.stored),
        (4, 17_777, 5, 22_223)
    );
    assert_eq!(grid.network_at([9, 64, 0]).unwrap(), original);
    assert_eq!(work.topology_nodes, 9);
    grid.add([4, 64, 0], Resource::Energy, 63, &mut work)
        .unwrap();
    let n = grid.network(original).unwrap();
    assert_eq!(
        (n.members().len(), n.capacity(), n.stored),
        (10, 80_000, 40_000)
    );
    grid.remove([9, 64, 0], &mut work).unwrap();
    let n = grid.network(original).unwrap();
    assert_eq!(
        (n.members().len(), n.capacity(), n.stored),
        (9, 72_000, 40_000)
    );
    // Hundreds of unrelated components do not increase a local split traversal.
    for x in 0..100 {
        grid.add([x * 2, 0, 20], Resource::Energy, 63, &mut work)
            .unwrap();
    }
    work = GridWork::default();
    grid.remove([3, 64, 0], &mut work).unwrap();
    assert_eq!(work.topology_nodes, 8);
}

#[test]
fn nonempty_fluid_networks_do_not_mix_and_empty_reconcile_is_local() {
    use rules::machines::grid::{Endpoint, Grid, GridWork, GridWorld, Resource};
    let mut grid = Grid::new(8, 4);
    let mut work = GridWork::default();
    let water = grid.add([0; 3], Resource::Fluid, 63, &mut work).unwrap();
    grid.insert([0; 3], chem::SUB_WATER, 1000).unwrap();
    grid.add([2, 0, 0], Resource::Fluid, 63, &mut work).unwrap();
    grid.insert([2, 0, 0], chem::SUB_LAVA, 1000).unwrap();
    grid.add([1, 0, 0], Resource::Fluid, 63, &mut work).unwrap();
    assert_ne!(grid.network_at([0; 3]), grid.network_at([2, 0, 0]));
    struct Sink;
    impl GridWorld for Sink {
        fn room(&mut self, _: Endpoint, _: Resource, _: u8) -> u64 {
            u64::MAX
        }
        fn give(&mut self, _: Endpoint, _: Resource, _: u8, n: u64) -> u64 {
            n
        }
    }
    grid.set_endpoints(
        water,
        &[Endpoint {
            owner: 1,
            position: [0, 1, 0],
            face: Face::Down,
            pull: false,
        }],
    )
    .unwrap();
    grid.phase(water, &mut Sink, &mut work).unwrap();
    grid.phase(water, &mut Sink, &mut work).unwrap();
    assert_eq!(grid.network(water).unwrap().substance, 0);
    grid.reconcile([1, 0, 0], &mut work).unwrap();
    assert_eq!(grid.network_at([0; 3]), grid.network_at([2, 0, 0]));
    let n = grid.network(grid.network_at([0; 3]).unwrap()).unwrap();
    assert_eq!((n.stored, n.substance), (1000, chem::SUB_LAVA));
}

#[test]
fn network_cached_endpoint_dedup_and_phases_allocate_nothing() {
    use rules::machines::grid::{Endpoint, Grid, GridWork, GridWorld, Resource};
    struct Sink {
        received: [u64; 2],
    }
    impl GridWorld for Sink {
        fn room(&mut self, _: Endpoint, _: Resource, _: u8) -> u64 {
            u64::MAX
        }
        fn give(&mut self, e: Endpoint, _: Resource, _: u8, n: u64) -> u64 {
            self.received[e.owner as usize] += n;
            n
        }
    }
    let mut grid = Grid::new(8, 8);
    let mut work = GridWork::default();
    let id = grid.add([0; 3], Resource::Energy, 63, &mut work).unwrap();
    grid.set_endpoints(
        id,
        &[
            Endpoint {
                owner: 0,
                position: [1, 0, 0],
                face: Face::West,
                pull: false,
            },
            Endpoint {
                owner: 0,
                position: [1, 0, 0],
                face: Face::Up,
                pull: false,
            },
            Endpoint {
                owner: 1,
                position: [-1, 0, 0],
                face: Face::East,
                pull: false,
            },
        ],
    )
    .unwrap();
    assert_eq!(grid.network(id).unwrap().endpoints().len(), 2);
    let mut sink = Sink { received: [0; 2] };
    let scope = AllocationScope::begin().unwrap();
    for _ in 0..1000 {
        grid.insert([0; 3], 0, 100).unwrap();
        grid.phase(id, &mut sink, &mut work).unwrap();
    }
    let counts = scope.finish();
    assert_eq!(counts.allocating_calls(), 0);
    assert_eq!(counts.dealloc_calls, 0);
    assert_eq!(sink.received, [50_000, 50_000]);
    assert_eq!(work.endpoint_queries, 2000);
}
