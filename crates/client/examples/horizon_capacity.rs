//! Explicit capacity experiment; no GPU, private seed, World or live limit changes.
use client::view::horizon::{Certificate, Config, Projection, PublicSelection, seam_error_q1024};
use net::height::{HeightErrorBound, HeightKey};
use render::height_tile::HeightTileKey;
use std::{collections::BTreeMap, time::Instant};

fn main() -> Result<(), String> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let near_3d = arguments.iter().any(|argument| argument == "--near-3d");
    let capacities = arguments
        .into_iter()
        .filter(|argument| argument != "--near-3d")
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
