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
    out
}
