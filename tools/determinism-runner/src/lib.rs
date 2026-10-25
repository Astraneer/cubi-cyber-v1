//! Actual shared deterministic scenario, executed as a native binary and WASM.
//! The authority dependency is test tooling, never part of client-web assets.

use core_types::{BlockPos, Cell, ChannelMask, ComponentId, PAGE_VOLUME, PageId, SpaceId, Tick};
use std::fmt::Write;
use world::{ComponentData, Transaction, World};

const DONOR_PROBE: [u64; 4] = [
    0x433b_5b0e_f285_6c41,
    0x0e00_9b2a_a4b1_3193,
    0xf9b2_1370_b7f5_0024,
    0x45da_32de_09dd_965a,
];

/// Hex strings preserve all 64 bits when crossing the JavaScript boundary.
#[cfg_attr(target_arch = "wasm32", wasm_bindgen::prelude::wasm_bindgen)]
pub fn run_trace() -> String {
    let probe = authority::legacy_probe_reference();
    assert_eq!(
        probe, DONOR_PROBE,
        "generator diverged from pinned donor v4"
    );
    let mut world = World::new(rules::properties, 256);
    let space = SpaceId(0);
    let mut initial = Transaction::new(Tick(0), space);
    for (x, y) in [(0, 0), (-1, -1)] {
        initial.create_page(PageId { space, x, y, z: 0 }, vec![Cell::AIR; PAGE_VOLUME]);
    }
    world.commit(initial).expect("valid initial pages");
    let mut out = String::from("{\"schema\":1,\"probe\":[");
    for (index, hash) in probe.into_iter().enumerate() {
        if index != 0 {
            out.push(',');
        }
        write!(out, "\"{hash:016x}\"").expect("write to String");
    }
    out.push_str("],\"ticks\":[");
    for tick in 1..=128_u64 {
        let t = tick as i32;
        let negative = tick % 2 != 0;
        let position = BlockPos {
            space,
            x: if negative { -1 - t % 16 } else { t % 16 },
            y: if negative {
                -1 - (t * 3) % 16
            } else {
                (t * 3) % 16
            },
            z: (t * 5) % 16,
        };
        let id = if tick % 3 == 0 {
            rules::ids::DIRT
        } else {
            rules::ids::STONE
        };
        let mut transaction = Transaction::new(Tick(tick), space);
        transaction.set_cell(position, Cell::from_parts(id, tick as u16));
        transaction.set_component(
            ComponentId(tick % 4),
            ComponentData {
                anchor: position,
                kind: 1,
                payload: tick.to_le_bytes().to_vec(),
            },
            ChannelMask::CONTENTS,
        );
        world
            .commit(transaction)
            .expect("valid deterministic transaction");
        if tick != 1 {
            out.push(',');
        }
        write!(out, "\"{:016x}\"", world.canonical_hash()).expect("write to String");
    }
    out.push_str("],\"physics\":[");
    for (index, hash) in sim::determinism_trace().into_iter().enumerate() {
        if index != 0 {
            out.push(',');
        }
        write!(out, "\"{hash:016x}\"").expect("write to String");
    }
    out.push_str("],\"entities\":[");
    for (index, hash) in sim::entities::trace::determinism_trace()
        .into_iter()
        .enumerate()
    {
        if index != 0 {
            out.push(',');
        }
        write!(out, "\"{hash:016x}\"").expect("write to String");
    }
    out.push_str("]}");
    movement_capture()
        .append_json(&mut out)
        .expect("complete bounded movement trace");
    out
}

fn movement_capture() -> sim::replay::Recorder {
    let mut recorder = sim::replay::Recorder::new(128).expect("bounded recorder");
    let mut player = sim::Player::new([0.5, 2.0, 0.5]);
    for tick in 0..64u64 {
        recorder
            .set_time(tick * 50_000_000)
            .expect("monotonic clock");
        if tick % 16 == 0 {
            player
                .teleport([0.5, 1.0, 0.5])
                .expect("bounded checkpoint");
            player.set_flight(tick == 16).expect("shared flight rule");
        }
        player.yaw = (tick * 3) as f32;
        let world = |p: [i32; 3]| {
            if tick >= 48 && p[0] > 1 {
                return world::CellRead::Unknown;
            }
            world::CellRead::Known(Cell::from_parts(
                if p[1] < 0 {
                    rules::ids::STONE
                } else if (32..48).contains(&tick) && p[1] < 2 {
                    rules::ids::WATER
                } else {
                    rules::ids::AIR
                },
                0,
            ))
        };
        recorder.tick(
            &mut player,
            &world,
            sim::Keys {
                fwd: true,
                jump: tick % 17 == 0,
                sprint: tick % 7 == 0,
                sneak: tick % 9 == 0,
                ..sim::Keys::default()
            },
            tick % 13 == 0,
        );
    }
    recorder
}

/// Reproduce a native or browser local F8 movement payload in actual WASM.
#[cfg_attr(target_arch = "wasm32", wasm_bindgen::prelude::wasm_bindgen)]
pub fn replay_report(report: &str) -> String {
    match sim::replay::Archive::from_report(report) {
        Ok(archive) => match archive.replay() {
            Ok(result) => format!("OK:{}:{:016x}", result.steps, result.final_hash),
            Err(error) => format!("FAILED:{}:{}", error.sequence, error.reason),
        },
        Err(error) => format!("INVALID:{error}"),
    }
}
