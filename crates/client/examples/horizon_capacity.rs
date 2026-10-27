//! Explicit offline capacity experiment. Optional projection mode uses only a
//! fixed test authority seed in a native helper; no live settings or GPU change.
use client::view::horizon::{Certificate, Config, Projection, PublicSelection, seam_error_q1024};
use net::height::{HeightErrorBound, HeightKey};
use render::height_tile::HeightTileKey;
use std::{collections::BTreeMap, time::Instant};

fn main() -> Result<(), String> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let near_3d = arguments.iter().any(|argument| argument == "--near-3d");
    let proxy_payloads = arguments
        .iter()
        .any(|argument| argument == "--proxy-payloads");
    let capacities = arguments
        .into_iter()
        .filter(|argument| argument != "--near-3d" && argument != "--proxy-payloads")
        .map(|value| value.parse::<usize>().map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    let capacities = if capacities.is_empty() {
        vec![512, 1024, 2048, 4096]
    } else {
        capacities
    };
    let tan = libm::tan(70f64.to_radians() * 0.5);
    let diagonal = libm::tan(80f64.to_radians() * 0.5) * libm::sqrt(1. + (780f64 / 437.).powi(2));
    let projection = Projection {
        eye_min: [0., 80., 0.],
        eye_max: [16., 96., 16.],
        focal_pixels: 437. / (2. * tan),
        diagonal_tangent: diagonal,
        target_pixels: 32.,
        hysteresis_pixels: 8.,
        view_direction: [0., 0., 1.],
        max_abs_forward_y: 1.,
        view_cosine: libm::cos(libm::atan(diagonal) + 37.5f64.to_radians()),
    };
    for seed in [1, 1337] {
        let generator = terrain_gen::Generator::new(terrain_gen::TerrainSeed(seed));
        let mut known = BTreeMap::<HeightKey, Certificate>::new();
        for &capacity in &capacities {
            let began = Instant::now();
            let mut previous = Vec::<HeightTileKey>::new();
            let mut columns = 0;
            let mut intervals = 0;
            let mut noises = 0;
            let mut planner_tests = 0;
            let mut settled = false;
            for round in 1..=24 {
                let result = PublicSelection {
                    config: Config {
                        max_tiles: capacity,
                        max_sources: 16_384,
                        near_volume: near_3d.then_some([[-1, 4, -1], [2, 7, 2]]),
                        ..Config::default()
                    },
                    space: 0,
                    camera: [0, 0],
                    near_radius: 2,
                    projection,
                    certificates: known.values().copied().collect(),
                    previous,
                    refine: vec![],
                }
                .execute();
                let plan = match result {
                    Ok(plan) => plan,
                    Err(error) => {
                        println!(
                            "{{\"seed\":{seed},\"capacity\":{capacity},\"round\":{round},\"error\":\"{error}\",\"cachedCertificates\":{},\"seconds\":{}}}",
                            known.len(),
                            began.elapsed().as_secs_f64()
                        );
                        break;
                    }
                };
                planner_tests += plan.work.adjacency_tests + plan.work.seam_tests;
                previous = plan.plan.tiles.iter().map(|tile| tile.key).collect();
                let mut added = 0;
                for tile in &plan.plan.tiles {
                    let key = HeightKey {
                        space: tile.key.space,
                        level: tile.key.level,
                        x: tile.key.x,
                        z: tile.key.z,
                    };
                    if known.contains_key(&key) {
                        continue;
                    }
                    let bounds = tile.key.bounds().ok_or("tile bounds")?;
                    let bound = generator
                        .public_height_error([bounds[0] as i32, bounds[1] as i32], key.level)
                        .map_err(|error| format!("{error:?}"))?;
                    columns += bound.sampled_columns;
                    intervals += bound.intervals;
                    noises += bound.noise_evaluations;
                    let step = 1i64 << key.level;
                    let heights = std::array::from_fn(|index| {
                        generator
                            .column_info(
                                (bounds[0] + (index % 17) as i64 * step) as i32,
                                (bounds[1] + (index / 17) as i64 * step) as i32,
                            )
                            .height
                            .max(terrain_gen::SEA_LEVEL)
                            + 1
                    });
                    known.insert(
                        key,
                        Certificate {
                            key,
                            bound: HeightErrorBound {
                                max_error_q1024: libm::ceil(bound.max_error_blocks * 1024.) as u64,
                                min_height: bound.min_height,
                                max_height: bound.max_height,
                            },
                            seam_error_q1024: seam_error_q1024(&heights),
                            exact_replacement: false,
                        },
                    );
                    added += 1;
                }
                for key in &plan.exact_leaves {
                    let certificate = known.get_mut(key).ok_or("missing selected leaf")?;
                    if !certificate.exact_replacement {
                        certificate.exact_replacement = true;
                        added += 1;
                    }
                }
                if added == 0 {
                    let selected = plan.plan.tiles.len();
                    let exact = plan
                        .plan
                        .tiles
                        .iter()
                        .filter(|tile| {
                            known
                                .get(&HeightKey {
                                    space: tile.key.space,
                                    level: tile.key.level,
                                    x: tile.key.x,
                                    z: tile.key.z,
                                })
                                .is_some_and(|certificate| certificate.exact_replacement)
                        })
                        .count();
                    println!(
                        "{{\"seed\":{seed},\"near3d\":{near_3d},\"capacity\":{capacity},\"rounds\":{round},\"selected\":{selected},\"cachedCertificates\":{},\"maximumErrorPixels\":{},\"capacityLimited\":{},\"missingCertificates\":{},\"exactColumns\":{exact},\"heightPayloadBytes\":{},\"heightTrianglesUpperBound\":{},\"heightDoubleBufferBytes\":{},\"newCertificateColumns\":{columns},\"newCertificateIntervals\":{intervals},\"newCertificateNoiseQueries\":{noises},\"plannerPairTests\":{planner_tests},\"seconds\":{}}}",
                        known.len(),
                        plan.quality.maximum_error_pixels,
                        plan.quality.capacity_limited,
                        plan.quality.missing_certificates,
                        selected * 3276,
                        selected * 512,
                        selected * 3276 * 2,
                        began.elapsed().as_secs_f64()
                    );
                    if proxy_payloads {
                        let keys = plan
                            .plan
                            .tiles
                            .iter()
                            .filter(|tile| {
                                known
                                    .get(&HeightKey {
                                        space: tile.key.space,
                                        level: tile.key.level,
                                        x: tile.key.x,
                                        z: tile.key.z,
                                    })
                                    .is_some_and(|certificate| certificate.exact_replacement)
                            })
                            .map(|tile| tile.key)
                            .collect::<Vec<_>>();
                        measure_proxies(seed, capacity, &keys)?;
                    }
                    settled = true;
                    break;
                }
            }
            if !settled {
                eprintln!("selection did not settle: seed {seed}, capacity {capacity}");
            }
        }
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn measure_proxies(seed: u32, capacity: usize, keys: &[HeightTileKey]) -> Result<(), String> {
    use render::lod::{MaskedRayProxy, PublicPage, PublicVoxel, SurfaceKind, protection_groups};
    let began = Instant::now();
    let authority = gen_authority::ServerGenerator::new(
        terrain_gen::TerrainSeed(seed),
        gen_authority::PrivateSeed::new([37; 32]),
    );
    let world = world::World::new(rules::properties, 16);
    let index = server::height_index::HeightEditIndex::new(Default::default())
        .map_err(|error| format!("{error:?}"))?;
    let mut pages = 0;
    let mut cutout_quads = 0;
    let mut payload_bytes = 0;
    let mut public_bytes = 0;
    let mut maximum_payload = 0;
    let mut generated_columns = 0;
    for key in keys {
        let snapshot = server::cold_projection::ColumnSnapshot::capture(
            &world,
            &index,
            server::cold_projection::ColumnKey { x: key.x, z: key.z },
            Default::default(),
        )
        .map_err(|error| format!("{error:?}"))?;
        let built = snapshot
            .execute(&authority, || false)
            .map_err(|error| format!("{error:?}"))?;
        generated_columns += built.work.generated_columns;
        for projected in built.pages {
            let Some(geometry) = projected.geometry else {
                return Err("Unknown canonical geometry in capacity fixture".into());
            };
            let page = PublicPage::from_exposed(|[x, y, z]| {
                let at = usize::from(x) + 16 * (usize::from(z) + 16 * usize::from(y));
                if geometry.known[at / 8] & (1 << (at % 8)) == 0 {
                    return PublicVoxel::Unknown;
                }
                if geometry.occupied[at / 8] & (1 << (at % 8)) == 0 {
                    return PublicVoxel::Air;
                }
                let material = geometry.palette[usize::from(
                    geometry.palette_index(core_types::LocalIdx::new(at as u16).unwrap()),
                )];
                match material.kind {
                    server::lod::PublicSurfaceKind::Water => PublicVoxel::Air,
                    kind => PublicVoxel::Solid {
                        material: material.atlas_layer,
                        tint: material.tint,
                        kind: if kind == server::lod::PublicSurfaceKind::Opaque {
                            SurfaceKind::Opaque
                        } else {
                            SurfaceKind::Cutout
                        },
                    },
                }
            });
            let proxy = MaskedRayProxy::from_public_page(&page, protection_groups(&page, 4));
            let payload = render::lod_gpu::ProxyPayload::from_public(&proxy, 186)
                .map_err(|error| error.to_string())?;
            pages += 1;
            cutout_quads += u64::from(payload.cutout_quads());
            payload_bytes += payload.payload_bytes();
            public_bytes += payload.encode_public().len();
            maximum_payload = maximum_payload.max(payload.payload_bytes());
        }
    }
    println!(
        "{{\"seed\":{seed},\"capacity\":{capacity},\"proxyFormat\":\"PXP2\",\"exactColumns\":{},\"geometryPages\":{pages},\"cutoutQuads\":{cutout_quads},\"proxyPayloadBytes\":{payload_bytes},\"proxyDoubleBufferBytes\":{},\"proxyPublicBytes\":{public_bytes},\"maximumProxyPayload\":{maximum_payload},\"gpuArenaBytes\":16777216,\"fitsDoubleBuffer\":{},\"worldPages\":{},\"generatedTestColumns\":{generated_columns},\"projectionSeconds\":{}}}",
        keys.len(),
        payload_bytes * 2,
        payload_bytes * 2 <= 16 * 1024 * 1024,
        world.pages().count(),
        began.elapsed().as_secs_f64()
    );
    Ok(())
}

#[cfg(target_arch = "wasm32")]
fn measure_proxies(_: u32, _: usize, _: &[HeightTileKey]) -> Result<(), String> {
    Err("the fixed test-authority projection is a native offline experiment".into())
}
