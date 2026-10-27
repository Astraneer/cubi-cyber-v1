//! Explicit public source fixture: no generator, seed, or client reconstruction.
use super::*;
use core_types::PageId;
use net::height::{GeometryStamp, HeightKey, HeightTile, Replacement};
use std::collections::BTreeSet;

pub(super) const KEY: HeightKey = HeightKey {
    space: 0,
    level: 0,
    x: 0,
    z: -1,
};

pub(super) fn cell(x: i32, y: i32, z: i32, pillar: bool) -> Cell {
    if y == 0 || pillar && (4..12).contains(&x) && (1..9).contains(&y) && (-12..-4).contains(&z) {
        Cell::from_parts(rules::ids::STONE, 0)
    } else if pillar && (4..12).contains(&x) && (9..11).contains(&y) && (-12..-4).contains(&z) {
        Cell::from_parts(rules::ids::LEAVES, 0)
    } else {
        Cell::AIR
    }
}

fn tile(key: HeightKey, source: u64) -> Box<HeightTile> {
    let mut known = [255; 37];
    known[36] = 1;
    let mut tile = HeightTile {
        key,
        source,
        error: Some(net::height::HeightErrorBound {
            max_error_q1024: if key == KEY { 10 * 1024 } else { 0 },
            min_height: 1,
            max_height: if key == KEY { 11 } else { 1 },
        }),
        heights: [1; 289],
        known,
        atlas: [rules::ids::L_STONE as u16; 256],
        tint: [u32::MAX; 256],
        protected: [0; 32],
        replacement: Replacement::None,
    };
    if key == KEY {
        tile.protected.fill(255);
        tile.replacement = Replacement::Column {
            min_y: 0,
            max_y: 7,
            geometry: (0..8)
                .map(|y| GeometryStamp {
                    page: PageId {
                        space: SpaceId(0),
                        x: 0,
                        y,
                        z: -1,
                    },
                    source: Version(40 + source),
                    view: source,
                })
                .collect(),
        };
    }
    // Exercise the production public wire validation, not a private source API.
    let encoded = net::height::encode(&tile).unwrap();
    Box::new(net::height::decode(&encoded).unwrap())
}

fn geometry(y: i32, source: u64, pillar: bool) -> net::PublicGeometry {
    let mut occupied = [0; 512];
    let mut material_indices = [false; 4096];
    let mut has_cutout = false;
    for ly in 0..16 {
        for z in 0..16 {
            for x in 0..16 {
                let cell = cell(x, y * 16 + ly, z - 16, pillar);
                if cell != Cell::AIR {
                    let cutout = cell.id() == rules::ids::LEAVES;
                    has_cutout |= cutout;
                    let i = (x + 16 * (z + 16 * ly)) as usize;
                    occupied[i / 8] |= 1 << (i % 8);
                    material_indices[i] = cutout;
                }
            }
        }
    }
    let mut palette = vec![];
    let mut indices = vec![];
    if occupied.iter().any(|v| *v != 0) {
        palette.push(net::PublicSurface {
            atlas_layer: rules::ids::L_STONE as u16,
            tint: u32::MAX,
            kind: net::PublicSurfaceKind::Opaque,
        });
        if has_cutout {
            palette.push(net::PublicSurface {
                atlas_layer: rules::ids::L_LEAVES as u16,
                tint: u32::MAX,
                kind: net::PublicSurfaceKind::Cutout,
            });
            indices.resize(material_indices.len().div_ceil(8), 0);
            for (i, cutout) in material_indices.iter().enumerate() {
                if *cutout {
                    indices[i / 8] |= 1 << (i % 8);
                }
            }
        }
    }
    let geometry = net::PublicGeometry {
        page: PageId {
            space: SpaceId(0),
            x: 0,
            y,
            z: -1,
        },
        source_version: Version(40 + source),
        view_version: source,
        known: [255; 512],
        occupied,
        palette,
        index_bits: u8::from(has_cutout),
        indices,
    };
    geometry
        .validate()
        .expect("explicit public fixture must satisfy its wire contract");
    geometry
}

pub(super) fn bootstrap(
    view: &mut ClientView,
    gpu: &render::Gpu,
    source: u64,
    pillar: bool,
) -> Result<(), String> {
    let mut admitted = BTreeSet::new();
    view.ingest_remote_height(tile(KEY, source))?;
    for y in 0..8 {
        view.ingest_remote_geometry(&geometry(y, source, pillar))?;
    }
    for _ in 0..1024 {
        for key in view.horizon_wanted_height().collect::<Vec<_>>() {
            if admitted.insert(key) {
                view.ingest_remote_height(tile(key, if key == KEY { source } else { 1 }))?;
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        std::thread::sleep(std::time::Duration::from_millis(1));
        pump(view, gpu).map_err(|e| {
            format!(
                "Far source{source} bootstrap: {e}; {:?}",
                view.horizon_stats()
            )
        })?;
        if view.has_committed_far_front()
            && view
                .horizon_mut()
                .is_some_and(|h| h.front_source(KEY) == Some(source))
            && view
                .horizon_stats()
                .is_some_and(|s| s.height_front > 0 && s.proxy_front == 8 && s.near_guard_ready)
        {
            return Ok(());
        }
    }
    Err(format!(
        "committed Far source did not converge: {:?} / {:?}",
        view.horizon_stats(),
        view.horizon_job_stats()
    ))
}

pub(super) fn metadata(view: &ClientView) -> Value {
    let stats = view.horizon_stats().unwrap();
    json!({"committed":view.has_committed_far_front(),"heightFront":stats.height_front,"proxyFront":stats.proxy_front,"knownSources":stats.known_geometry_sources,"guard":stats.near_guard_ready,"committedPlan":stats.committed_plan_id})
}

pub(super) fn camera(view: &mut ClientView, x: f64) -> Result<(), String> {
    let mut player = sim::Player::new([x, 4., 14.]);
    player.yaw = 180.;
    view.install_remote_player(&player, Tick(1))
}
