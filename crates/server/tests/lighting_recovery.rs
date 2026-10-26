//! Lost bounded journal history repairs from retained semantic page snapshots.
//! Repeated unrelated history must not reset a still draining full-page reseed.
use core_types::{BlockPos, Cell, CellProperties, LocalIdx, PageId, SpaceId, Tick};
use server::lighting::{ServerLighting, ServerLightingConfig};
use world::{GameplayEvent, JournalLimits, LightSample, Transaction, World};

fn properties(cell: Cell) -> CellProperties {
    let mut properties = CellProperties::AIR;
    properties.emission = match cell.id() {
        2 => [15, 9, 3],
        3 => [5, 15, 11],
        _ => [0; 3],
    };
    properties
}
fn page(x: i32) -> PageId {
    PageId {
        space: SpaceId(0),
        x,
        y: 0,
        z: 0,
    }
}
fn emitter() -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x: 15,
        y: 8,
        z: 8,
    }
}
fn unrelated_history(world: &mut World) {
    for _ in 0..3 {
        let mut transaction = Transaction::new(world.tick(), SpaceId(0));
        transaction.event(GameplayEvent {
            kind: 1,
            subject: None,
            position: None,
            payload: Vec::new(),
        });
        world.commit(transaction).unwrap();
    }
    assert!(world.journal_len() <= 2);
}
fn change_emitter(world: &mut World, id: u16) {
    let mut transaction = Transaction::new(Tick(world.tick().0 + 1), SpaceId(0));
    transaction.set_cell(emitter(), Cell::from_parts(id, 0));
    world.commit(transaction).unwrap();
}
fn slow_config() -> ServerLightingConfig {
    ServerLightingConfig {
        max_bootstrap_pages: 8,
        engine: world::lighting::LightingLimits {
            max_seed_pages: 1,
            max_frontier_cells: 4096,
            reserved_interactive_cells: 256,
            ..Default::default()
        },
        step: world::lighting::LightBudget {
            max_cells: 64,
            max_neighbor_reads: 384,
            max_seed_cells: 64,
            max_updates: 64,
        },
        ..Default::default()
    }
}
fn assert_manhattan(world: &World, emission: [u8; 3]) {
    for (id, page) in world.pages() {
        for index in 0..4096 {
            let local = LocalIdx::new(index).unwrap();
            let (x, y, z) = local.xyz();
            let distance = (id.x * 16 + i32::from(x) - emitter().x).unsigned_abs()
                + (i32::from(y) - emitter().y).unsigned_abs()
                + (i32::from(z) - emitter().z).unsigned_abs();
            let attenuation = distance.min(255) as u8;
            let expected = LightSample {
                rgb: emission.map(|value| value.saturating_sub(attenuation)),
                block: emission
                    .into_iter()
                    .max()
                    .unwrap()
                    .saturating_sub(attenuation),
                sky: 0,
            };
            assert_eq!(page.light(local), expected, "page{id:?} cell{index}");
        }
    }
}

#[test]
fn repeated_lag_retains_reseed_progress_and_covers_missed_colour_and_removed_emitter() {
    let mut world = World::with_limits(
        properties,
        JournalLimits {
            max_records: 2,
            ..JournalLimits::default()
        },
    );
    let mut create = Transaction::new(Tick(1), SpaceId(0));
    for x in 0..3 {
        create.create_page(page(x), vec![Cell::AIR; 4096]);
    }
    world.commit(create).unwrap();
    let mut owner = ServerLighting::attach(&mut world, slow_config()).unwrap();
    change_emitter(&mut world, 2);
    let mut recoveries = 0;
    let mut queued = 0;
    let mut settled = false;
    for step in 0..3000 {
        if step == 17 {
            change_emitter(&mut world, 3);
        }
        unrelated_history(&mut world);
        let tick = Tick(world.tick().0 + 1);
        let phase = owner.step(&mut world, tick, &|_| None).unwrap();
        recoveries += phase.work.input_recoveries;
        queued += phase.work.bootstrap_pages_queued;
        assert_eq!(phase.work.foreground_cell_reads, 0);
        assert!(phase.work.recovery_page_probes <= 32);
        assert!(phase.bootstrap_pending <= 3);
        assert!(phase.progress.pending_seed_pages <= 1);
        assert!(phase.progress.work.cells_examined <= 64);
        assert!(phase.progress.work.neighbor_reads <= 384);
        assert!(phase.progress.work.seed_cells_examined <= 64);
        owner.acknowledge_after_durable(&mut world, &phase).unwrap();
        if step > 17 && owner.is_idle() {
            settled = true;
            break;
        }
    }
    assert!(settled, "repeated lag must not restart every seed");
    assert!(recoveries > 10);
    assert!(queued <= 4, "only the changed source needs another reseed");
    assert_manhattan(&world, [5, 15, 11]);
    assert!(
        world
            .page(page(1))
            .unwrap()
            .light(LocalIdx::new(8 * 256 + 8 * 16).unwrap())
            .rgb[1]
            > 0
    );

    // The removed page is absent from the current snapshot. Its unchanged
    // neighbour must nevertheless withdraw the emitter's retained contribution.
    let mut remove = Transaction::new(Tick(world.tick().0 + 1), SpaceId(0));
    remove.remove_page(page(0));
    world.commit(remove).unwrap();
    let mut settled = false;
    for _ in 0..3000 {
        unrelated_history(&mut world);
        let tick = Tick(world.tick().0 + 1);
        let phase = owner.step(&mut world, tick, &|_| None).unwrap();
        assert!(phase.bootstrap_pending <= 2);
        assert!(phase.progress.work.cells_examined <= 64);
        owner.acknowledge_after_durable(&mut world, &phase).unwrap();
        if owner.is_idle() {
            settled = true;
            break;
        }
    }
    assert!(
        settled,
        "removed emitter must converge despite repeated lag"
    );
    assert_eq!(world.loaded_pages(), 2);
    assert_manhattan(&world, [0; 3]);
    assert_eq!(world.journal_len(), 2, "journal limit was not enlarged");
    println!(
        "LIGHT_INPUT_RECOVERY_OK recoveries{recoveries} queued{queued} journalRecords2 seedPages1 cellsPerPhase64 missedColour+removedEmitter exact"
    );
}

#[test]
fn lost_roof_opacity_closes_and_reopens_the_entire_certified_vertical_sky_chain() {
    fn settle(owner: &mut ServerLighting, world: &mut World) -> u64 {
        let mut recoveries = 0;
        for _ in 0..5000 {
            unrelated_history(world);
            let tick = Tick(world.tick().0 + 1);
            let phase = owner
                .step(world, tick, &|position: BlockPos| {
                    (position.y >= 15).then_some(15)
                })
                .unwrap();
            recoveries += phase.work.input_recoveries;
            assert!(phase.bootstrap_pending <= 1);
            assert!(phase.progress.pending_seed_pages <= 1);
            assert!(phase.progress.work.cells_examined <= 64);
            assert!(phase.progress.work.neighbor_reads <= 384);
            assert!(phase.progress.work.seed_cells_examined <= 64);
            owner.acknowledge_after_durable(world, &phase).unwrap();
            if owner.is_idle() {
                return recoveries;
            }
        }
        panic!("the retained sky snapshot must converge despite repeated lag");
    }
    let mut world = World::with_limits(
        |cell: Cell| {
            if cell.id() == 1 {
                CellProperties::SOLID
            } else {
                CellProperties::AIR
            }
        },
        JournalLimits {
            max_records: 2,
            ..JournalLimits::default()
        },
    );
    let mut create = Transaction::new(Tick(1), SpaceId(0));
    create.create_page(page(0), vec![Cell::AIR; 4096]);
    world.commit(create).unwrap();
    let mut owner = ServerLighting::attach(&mut world, slow_config()).unwrap();
    let mut recoveries = settle(&mut owner, &mut world);
    assert!((0..4096).all(|index| {
        world
            .page(page(0))
            .unwrap()
            .light(LocalIdx::new(index).unwrap())
            .sky
            == 15
    }));
    for (cell, expected_sky) in [(Cell::from_parts(1, 0), 0), (Cell::AIR, 15)] {
        let mut roof = Transaction::new(Tick(world.tick().0 + 1), SpaceId(0));
        for z in 0..16 {
            for x in 0..16 {
                roof.set_cell(
                    BlockPos {
                        space: SpaceId(0),
                        x,
                        y: 15,
                        z,
                    },
                    cell,
                );
            }
        }
        world.commit(roof).unwrap();
        recoveries += settle(&mut owner, &mut world);
        for index in 0..4096 {
            assert_eq!(
                world
                    .page(page(0))
                    .unwrap()
                    .light(LocalIdx::new(index).unwrap()),
                LightSample {
                    sky: expected_sky,
                    ..LightSample::default()
                },
                "complete roof state {cell:?}, cell{index}"
            );
        }
    }
    assert!(recoveries > 10);
    assert_eq!(world.journal_len(), 2);
    assert_eq!(world.loaded_pages(), 1);
    println!(
        "LIGHT_SNAPSHOT_ROOF_OK recoveries{recoveries} sky15-0-15 all4096cells exact; journalRecords2 seedPages1"
    );
}
