//! Reproducible H1 CPU workload. Worker occupancy is a deterministic queue model,
//! not a claim about GPU presentation or host wall-clock throughput.
use client::{
    ClientScene, SceneConfig, SceneOutput,
    edit_shell::{EditShell, build_page_patch},
};
use core_types::{BlockPos, Cell, PageId, SpaceId, Tick};
use jobs::{Completion, JobOutcome};
use world::Transaction;

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let owners: usize = args
        .get(1)
        .map_or(8192, |v| v.parse().expect("owner quota"));
    let workers: usize = args.get(2).map_or(2, |v| v.parse().expect("worker count"));
    let incremental = args.get(3).is_some_and(|v| v == "incremental");
    let mut fixed_pages = std::collections::BTreeSet::new();
    assert!((1..=64).contains(&workers));
    let cfg = SceneConfig {
        center: point(8, 8, 8),
        horizontal_radius: 2,
        vertical_radius: 2,
        max_new_jobs_per_phase: 16,
        // This fixture deliberately creates 343 all-air pages through the user
        // transaction API; reserve those initial full-page overrides explicitly.
        overrides: client::OverrideLimits {
            max_cells: 2 * 1024 * 1024,
            ..client::OverrideLimits::default()
        },
        coherent_rebasing: true,
        ..SceneConfig::forest()
    };
    let mut scene = ClientScene::new(cfg).unwrap();
    let mut tx = Transaction::new(Tick(0), SpaceId(0));
    for page in scene.requested_pages() {
        tx.create_page(page, vec![Cell::AIR; 4096]);
    }
    scene.apply_transaction(tx).unwrap();
    for _ in 0..1000 {
        scene.phase(Tick(0), 64).unwrap();
        while let Some(job) = scene.take_task(Tick(0), &mut ClientScene::default_budget()) {
            let result = job.payload.execute(&job.cancellation).unwrap();
            scene
                .complete(Completion {
                    id: job.id,
                    outcome: JobOutcome::Success(result),
                })
                .unwrap();
        }
        scene.phase(Tick(0), 64).unwrap();
        while let Some(mesh) = scene.take_composite() {
            assert!(scene.ack_composite(&mesh));
        }
        if scene.is_ready() {
            break;
        }
    }
    assert!(scene.is_ready());
    let hot: Vec<_> = (-1..=2)
        .flat_map(|y| {
            (-1..=2).flat_map(move |z| {
                (-1..=2).map(move |x| PageId {
                    space: SpaceId(0),
                    x,
                    y,
                    z,
                })
            })
        })
        .collect();
    assert_eq!(hot.len(), 64);
    let mut shell = EditShell::new(64, owners);
    let mut inflight: Vec<(usize, Completion<SceneOutput>)> = Vec::with_capacity(workers);
    let (
        mut requested,
        mut accepted,
        mut rejected,
        mut peak,
        mut rebases,
        mut patch_reads,
        mut patch_quads,
        mut upload_bytes,
    ) = (0usize, 0usize, 0usize, 0usize, 0usize, 0u64, 0usize, 0usize);
    for frame in 1..=600usize {
        let tick = Tick(frame as u64 / 3);
        let target = frame * 1000 / 60;
        while requested < target {
            let seq = requested;
            requested += 1;
            let page = hot[seq % 64];
            let rng = core_types::DeterministicRng::from_seed(seq as u64 ^ 0x991233).next_u64();
            let position = point(
                page.x * 16 + 2 + (rng % 12) as i32,
                page.y * 16 + 2 + ((rng >> 16) % 12) as i32,
                page.z * 16 + 2 + ((rng >> 32) % 12) as i32,
            );
            if shell.can_mark_cells(&[position]).is_err() {
                rejected += 1;
                continue;
            }
            let new = match scene.world().read(position) {
                world::CellRead::Known(cell) if cell == Cell::AIR => {
                    Cell::from_parts(rules::ids::STONE, 0)
                }
                world::CellRead::Known(_) => Cell::AIR,
                world::CellRead::Unknown => panic!("hot page must remain loaded"),
            };
            let mut tx = Transaction::new(tick, SpaceId(0));
            tx.set_cell(position, new);
            let change = scene.apply_transaction(tx).unwrap();
            shell.mark_changes(&change).unwrap();
            accepted += 1;
            peak = peak.max(shell.owner_cells());
        }
        if incremental {
            let updates = shell
                .rebuild_owner_updates(scene.world(), |_, _, _| u16::MAX)
                .unwrap();
            for update in updates {
                patch_reads += update.work.work.cells_scanned;
                patch_quads += update
                    .updates
                    .iter()
                    .map(|p| {
                        p.classes
                            .iter()
                            .filter(|c| **c != render::terrain_mesh::MeshClass::Empty)
                            .count()
                    })
                    .sum::<usize>();
                upload_bytes += 768 + update.updates.len() * 100;
                if fixed_pages.insert(update.page) {
                    upload_bytes += 16384;
                }
                assert!(shell.ack_owner_updates(&update, scene.world()));
            }
        } else {
            let patches = shell.rebuild(scene.world(), |_, _, _| u16::MAX).unwrap();
            for patch in patches {
                patch_reads += patch.work.work.cells_scanned;
                patch_quads += patch.mesh.quad_count();
                upload_bytes += patch.mesh.quad_count() * 16 + 512;
            }
        }
        let mut index = 0;
        while index < inflight.len() {
            if inflight[index].0 <= frame {
                let (_, completion) = inflight.remove(index);
                scene.complete(completion).unwrap();
            } else {
                index += 1;
            }
        }
        scene.phase(tick, 64).unwrap();
        while let Some(composite) = scene.take_composite() {
            assert!(scene.composite_is_current(&composite));
            if shell
                .can_replace_page_mask(composite.base.page, &composite.changed_owners)
                .is_err()
            {
                scene.return_composite(composite).unwrap();
                break;
            }
            if incremental {
                assert!(
                    composite.changed_owners.is_empty()
                        || fixed_pages.contains(&composite.base.page)
                );
                upload_bytes += composite.base.mesh.quad_count() * 16 + 768;
            } else {
                let patch = build_page_patch(
                    composite.base.page,
                    &composite.changed_owners,
                    scene.world(),
                    &|_, _, _| u16::MAX,
                )
                .unwrap();
                assert!(patch.is_current(scene.world()));
                patch_reads += patch.work.work.cells_scanned;
                patch_quads += patch.mesh.quad_count();
                upload_bytes +=
                    (patch.mesh.quad_count() + composite.base.mesh.quad_count()) * 16 + 512;
            }
            assert!(scene.ack_composite(&composite));
            shell
                .replace_page_mask(composite.base.page, composite.changed_owners)
                .unwrap();
            rebases += 1;
        }
        let mut budget = ClientScene::default_budget();
        while inflight.len() < workers {
            let Some(job) = scene.take_task(tick, &mut budget) else {
                break;
            };
            let result = job.payload.execute(&job.cancellation).unwrap();
            inflight.push((
                frame + 12,
                Completion {
                    id: job.id,
                    outcome: JobOutcome::Success(result),
                },
            ));
        }
        assert!(shell.owner_cells() <= owners);
        assert!(shell.page_count() <= 64);
    }
    let report = format!(
        "{{\"pages\":64,\"requested\":{requested},\"accepted\":{accepted},\"rejected\":{rejected},\"worker_occupancy_ms\":200,\"workers\":{workers},\"owner_quota\":{owners},\"peak_owners\":{peak},\"rebases\":{rebases},\"patch_reads\":{patch_reads},\"patch_quads\":{patch_quads},\"upload_bytes\":{upload_bytes},\"incremental\":{incremental},\"gpu_latency_measured\":false}}"
    );
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.local/artifacts");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("h1-cpu-{owners}-{workers}-{incremental}.json")),
        format!("{report}\n"),
    )
    .unwrap();
    println!("H1_CPU_QUEUE_MODEL {report}");
}
fn point(x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x,
        y,
        z,
    }
}
