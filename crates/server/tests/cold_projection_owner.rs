use core_types::{BlockPos, Cell, PageId, SpaceId, Tick, TxId};
use gen_authority::{PrivateSeed, ServerGenerator};
use server::{
    cold_projection::ColumnKey,
    cold_projection_owner::{Error, Limits, Owner},
    height_index::{CellEdit, HeightEditIndex, IndexLimits},
};
use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};
use world::{Transaction, World};
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
fn page(x: i32, y: i32) -> PageId {
    PageId {
        space: SpaceId(0),
        x,
        y,
        z: 0,
    }
}
fn generator() -> Arc<ServerGenerator> {
    Arc::new(ServerGenerator::new(
        r#gen::TerrainSeed(1),
        PrivateSeed::new([37; 32]),
    ))
}
fn index() -> HeightEditIndex {
    HeightEditIndex::new(IndexLimits::default()).unwrap()
}
fn owner(limits: Limits) -> Owner {
    Owner::new(generator(), limits).unwrap()
}
fn drive(owner: &mut Owner, w: &World, idx: &HeightEditIndex, tick: &mut u64) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        *tick += 1;
        owner.pump(w, idx, Tick(*tick), 2, 2).unwrap();
        let s = owner.stats();
        if s.running == 0 && s.queued == 0 {
            return;
        }
        assert!(Instant::now() < deadline, "{:?}", s);
        thread::sleep(Duration::from_micros(100));
    }
}
#[test]
fn durable_machine_emission_and_contents_do_not_revoke_or_rebuild_cold_projection() {
    use core_types::{ChannelMask, ComponentId};
    use server::height_authority::{Config, HeightAuthority};
    let position = BlockPos {
        space: SpaceId(0),
        x: 2,
        y: 82,
        z: 2,
    };
    let mut world = World::new(rules::properties, 128);
    let mut cells = vec![Cell::AIR; 4096];
    cells[position.local_index().get() as usize] =
        Cell::from_parts(rules::ids::ENERGIZED_SMELTER, 0);
    let mut tx = Transaction::new(Tick(1), SpaceId(0));
    tx.create_page(position.page(), cells);
    world.commit(tx).unwrap();
    let mut height = HeightAuthority::new(Config::default()).unwrap();
    let mut owner = owner(Limits::default());
    let mut tick = 1;
    owner.interest(1, position.page(), Tick(tick)).unwrap();
    drive(&mut owner, &world, height.index(), &mut tick);
    let head = owner.head(position.page(), &world, height.index()).unwrap();
    let channels = ChannelMask::SHAPE | ChannelMask::OPACITY | ChannelMask::FLUID;
    let geometry_input = world.input_version(position.page(), channels);
    let old_hid = height
        .index()
        .source(server::height_index::TileKey {
            space: position.space,
            level: 0,
            x: 0,
            z: 0,
        })
        .unwrap();
    // These are the exact actual Cell transitions produced by machine phase
    // ActiveView publication. Run their real World and durable HID paths.
    for lit in [true, false, true, false] {
        tick += 1;
        let mut tx = Transaction::new(Tick(tick), SpaceId(0));
        tx.set_cell(
            position,
            Cell::from_parts(
                rules::ids::ENERGIZED_SMELTER,
                if lit { rules::blocks::ST_LIT } else { 0 },
            ),
        );
        let change = world.commit(tx).unwrap();
        assert_eq!(change.cells.len(), 1);
        assert_eq!(change.channels, ChannelMask::EMISSION);
        assert_eq!(
            world.input_version(position.page(), channels),
            geometry_input
        );
        let plan = height
            .plan_batch(std::slice::from_ref(&change))
            .unwrap()
            .unwrap();
        height.apply_after_durable(plan).unwrap();
        owner.observe(std::slice::from_ref(&change), Tick(tick));
        assert_eq!(
            owner.head(position.page(), &world, height.index()),
            Some(head),
            "an unread resident HID row cannot revoke the public column"
        );
        let work = owner
            .pump(&world, height.index(), Tick(tick), 2, 2)
            .unwrap();
        assert_eq!(
            (work.submitted, work.completed, work.generated_columns),
            (0, 0, 0)
        );
    }
    assert!(
        height
            .index()
            .source(server::height_index::TileKey {
                space: position.space,
                level: 0,
                x: 0,
                z: 0,
            })
            .unwrap()
            > old_hid,
        "broad private Height sources really advanced"
    );
    tick += 1;
    let mut tx = Transaction::new(Tick(tick), SpaceId(0));
    tx.set_component(
        ComponentId(1),
        world::ComponentData {
            anchor: position,
            kind: 7,
            payload: vec![3, 4, 5],
        },
        ChannelMask::CONTENTS,
    );
    let change = world.commit(tx).unwrap();
    assert!(change.cells.is_empty());
    assert!(
        height
            .plan_batch(std::slice::from_ref(&change))
            .unwrap()
            .is_none()
    );
    owner.observe(&[change], Tick(tick));
    assert_eq!(
        owner.head(position.page(), &world, height.index()),
        Some(head)
    );
    let scope = telemetry::AllocationScope::begin().unwrap();
    for _ in 0..1000 {
        tick += 1;
        let work = owner
            .pump(&world, height.index(), Tick(tick), 2, 2)
            .unwrap();
        assert_eq!(
            (work.submitted, work.completed, work.generated_columns),
            (0, 0, 0)
        );
        assert_eq!(
            owner.head(position.page(), &world, height.index()),
            Some(head)
        );
    }
    let counts = scope.finish();
    assert_eq!(counts.allocating_calls(), 0);
    assert_eq!(counts.dealloc_calls, 0);
    // A true resident geometry edit still revokes the certificate and schedules
    // the bounded worker; there is no relaxation of source/materialization.
    let mut tx = Transaction::new(Tick(tick + 1), SpaceId(0));
    tx.set_cell(position, Cell::AIR);
    let change = world.commit(tx).unwrap();
    let plan = height
        .plan_batch(std::slice::from_ref(&change))
        .unwrap()
        .unwrap();
    height.apply_after_durable(plan).unwrap();
    owner.observe(&[change], Tick(tick + 1));
    assert!(
        owner
            .head(position.page(), &world, height.index())
            .is_none()
    );
    assert!(owner.stats().queued > 0);
    let key = ColumnKey { x: 0, z: 0 };
    let snapshot = server::cold_projection::ColumnSnapshot::capture(
        &world,
        height.index(),
        key,
        Default::default(),
    )
    .unwrap();
    let cold_edit = CellEdit {
        position: BlockPos {
            space: SpaceId(0),
            x: 17,
            y: 2,
            z: 1,
        },
        cell: Cell::AIR,
    };
    assert!(world.page(cold_edit.position.page()).is_none());
    let delta = HeightEditIndex::encode_delta(TxId(height.last_tx().0 + 1), &[cold_edit]).unwrap();
    height.apply_encoded_delta(&delta).unwrap();
    assert!(
        !snapshot.certificate().matches(&world, height.index()),
        "a cold page still depends on its exact saved override rows"
    );
    let snapshot = server::cold_projection::ColumnSnapshot::capture(
        &world,
        height.index(),
        key,
        Default::default(),
    )
    .unwrap();
    let mut tx = Transaction::new(Tick(tick + 2), SpaceId(0));
    tx.create_page(cold_edit.position.page(), vec![Cell::AIR; 4096]);
    world.commit(tx).unwrap();
    assert!(
        !snapshot.certificate().matches(&world, height.index()),
        "cold materialization must still invalidate even for equal AIR"
    );
}
#[test]
fn public_heads_and_independent_water_versions_survive_materialization_and_halo_only_edits() {
    let mut w = World::new(rules::properties, 128);
    let mut idx = index();
    let mut owner = owner(Limits::default());
    let mut tick = 0;
    for y in 0..8 {
        owner.interest(1, page(0, y), Tick(0)).unwrap();
    }
    drive(&mut owner, &w, &idx, &mut tick);
    let heads: Vec<_> = (0..8)
        .map(|y| owner.head(page(0, y), &w, &idx).unwrap())
        .collect();
    let initial = owner.water(page(0, 6), &w, &idx).unwrap();
    let untouched = owner.water(page(0, 3), &w, &idx).unwrap();
    assert_eq!(w.pages().count(), 0);
    assert_eq!(owner.stats().heads, 8);
    assert!(heads.iter().all(|h| h.source.0 > 0 && h.view > 0));
    let edit = CellEdit {
        position: BlockPos {
            space: SpaceId(0),
            x: 16,
            y: 100,
            z: 5,
        },
        cell: Cell::from_parts(rules::ids::WATER, 3),
    };
    idx.apply_committed(TxId(1), &[edit]).unwrap();
    owner.request(ColumnKey { x: 0, z: 0 }, Tick(tick)).unwrap();
    drive(&mut owner, &w, &idx, &mut tick);
    assert_eq!(
        heads,
        (0..8)
            .map(|y| owner.head(page(0, y), &w, &idx).unwrap())
            .collect::<Vec<_>>(),
        "neighborHIDsample must not changecentregeometry IDs"
    );
    let changed = owner.water(page(0, 6), &w, &idx).unwrap();
    assert!(changed.water_view > initial.water_view);
    assert!(changed.column_source > initial.column_source);
    assert_eq!(changed.head, initial.head);
    assert_eq!(owner.take_changed_water(), Some(page(0, 6)));
    assert!(owner.take_changed_water().is_none());
    assert!(owner.take_changed_geometry().is_none());
    let untouched_after = owner.water(page(0, 3), &w, &idx).unwrap();
    assert_eq!(untouched_after.cells, untouched.cells);
    assert!(untouched_after.column_source > untouched.column_source);
    assert_eq!(
        untouched_after.water_view, untouched.water_view,
        "private HID revision alone cannot change an identical public water image"
    );
    let generator = generator();
    let column = generator.generate_column(0, 0).unwrap();
    let mut tx = Transaction::new(Tick(tick), SpaceId(0));
    for y in 0..8 {
        let cells: Box<[Cell]> = column.page(page(0, y), None).unwrap().cells;
        tx.create_page(page(0, y), cells);
    }
    let change = w.commit(tx).unwrap();
    owner.observe(&[change], Tick(tick));
    assert!(owner.head(page(0, 3), &w, &idx).is_none());
    drive(&mut owner, &w, &idx, &mut tick);
    assert_eq!(
        heads,
        (0..8)
            .map(|y| owner.head(page(0, y), &w, &idx).unwrap())
            .collect::<Vec<_>>(),
        "samepublicbody reusesIDs after actualmaterialization"
    );
    let after = owner.water(page(0, 6), &w, &idx).unwrap();
    assert_eq!(after.water_view, changed.water_view);
    assert_eq!(after.cells, changed.cells);
}
#[test]
fn bounded_lru_can_evict_bodies_but_active_heads_remain_stable_and_forgotten_heads_never_aba() {
    let w = World::new(rules::properties, 16);
    let idx = index();
    let mut owner = owner(Limits {
        cache_columns: 1,
        max_pages_per_connection: 1,
        ..Limits::default()
    });
    let mut tick = 0;
    owner.interest(1, page(0, 3), Tick(0)).unwrap();
    drive(&mut owner, &w, &idx, &mut tick);
    let first = owner.head(page(0, 3), &w, &idx).unwrap();
    owner.interest(2, page(30, 3), Tick(tick)).unwrap();
    drive(&mut owner, &w, &idx, &mut tick);
    assert!(owner.stats().cache_units <= 8);
    assert_eq!(owner.head(page(0, 3), &w, &idx), Some(first));
    assert!(owner.geometry(page(0, 3), &w, &idx).is_none());
    owner.request(ColumnKey { x: 0, z: 0 }, Tick(tick)).unwrap();
    drive(&mut owner, &w, &idx, &mut tick);
    assert_eq!(owner.head(page(0, 3), &w, &idx), Some(first));
    owner.forget_peer(1);
    assert!(owner.head(page(0, 3), &w, &idx).is_none());
    owner.interest(3, page(0, 3), Tick(tick)).unwrap();
    drive(&mut owner, &w, &idx, &mut tick);
    let rebuilt = owner.head(page(0, 3), &w, &idx).unwrap();
    assert!(rebuilt.source > first.source);
    assert!(rebuilt.view > first.view);
    assert_eq!(
        owner.interest(3, page(1, 3), Tick(tick)),
        Err(Error::Capacity)
    );
    owner.forget_peer(2);
    owner.forget_peer(3);
    assert_eq!(owner.stats().heads, 0);
    assert_eq!(owner.stats().leases, 0);
    assert_eq!(owner.stats().columns, 0);
}
#[test]
fn stale_worker_retries_current_input_and_ten_thousand_idle_phases_do_no_allocations_or_generations()
 {
    let w = World::new(rules::properties, 16);
    let mut idx = index();
    let mut owner = owner(Limits::default());
    let mut tick = 1;
    owner.interest(1, page(0, 3), Tick(0)).unwrap();
    let first = owner.pump(&w, &idx, Tick(1), 2, 2).unwrap();
    assert_eq!(first.submitted, 1);
    idx.apply_committed(
        TxId(1),
        &[CellEdit {
            position: BlockPos {
                space: SpaceId(0),
                x: 3,
                y: 3,
                z: 3,
            },
            cell: Cell::AIR,
        }],
    )
    .unwrap();
    drive(&mut owner, &w, &idx, &mut tick);
    assert!(owner.head(page(0, 3), &w, &idx).is_some());
    let stats = owner.stats();
    let scope = telemetry::AllocationScope::begin().unwrap();
    for _ in 0..10000 {
        tick += 1;
        let work = owner.pump(&w, &idx, Tick(tick), 2, 2).unwrap();
        assert_eq!(work.submitted, 0);
        assert_eq!(work.generated_columns, 0);
        assert_eq!(work.tint_samples, 0);
        assert_eq!(work.probes, 0);
        assert!(owner.head(page(0, 3), &w, &idx).is_some());
    }
    let counts = scope.finish();
    assert_eq!(counts.allocating_calls(), 0);
    assert_eq!(counts.dealloc_calls, 0);
    assert_eq!(owner.stats(), stats);
    assert_eq!(w.pages().count(), 0);
}
#[test]
fn unsupported_noncanonical_pages_cannot_bypass_the_certified_projection() {
    let mut owner = owner(Limits::default());
    let world = World::new(rules::properties, 16);
    let index = index();
    for p in [
        page(0, -1),
        page(0, 8),
        PageId {
            space: SpaceId(7),
            ..page(0, 0)
        },
    ] {
        assert_eq!(owner.interest(1, p, Tick(0)), Err(Error::Bounds));
        assert_eq!(owner.interest_raw(1, p, Tick(0)), Err(Error::Bounds));
        assert!(owner.head(p, &world, &index).is_none());
        assert!(owner.geometry(p, &world, &index).is_none());
    }
    assert_eq!(
        (
            owner.stats().leases,
            owner.stats().connections,
            owner.stats().cache_entries
        ),
        (0, 0, 0)
    );
}
#[test]
fn eight_late_vertical_leases_reuse_one_cached_column_without_more_private_generation() {
    let w = World::new(rules::properties, 16);
    let idx = index();
    let mut owner = owner(Limits::default());
    let mut tick = 0;
    owner.interest(1, page(0, 0), Tick(0)).unwrap();
    drive(&mut owner, &w, &idx, &mut tick);
    let first = owner.head(page(0, 0), &w, &idx).unwrap();
    for y in 1..8 {
        owner.interest(1, page(0, y), Tick(tick)).unwrap();
        tick += 1;
        let work = owner.pump(&w, &idx, Tick(tick), 2, 2).unwrap();
        assert_eq!(work.submitted, 0);
        assert_eq!(work.generated_columns, 0);
        assert_eq!(work.probes, 1);
        assert!(owner.head(page(0, y), &w, &idx).is_some());
        assert_eq!(owner.head(page(0, 0), &w, &idx), Some(first));
    }
    assert_eq!(owner.stats().heads, 8);
    assert_eq!(owner.stats().cache_entries, 1);
    assert_eq!(w.pages().count(), 0);
}
#[test]
fn raw_and_far_leases_share_one_connection_without_releasing_each_other_or_hiding_cache_cost() {
    let world = World::new(rules::properties, 16);
    let index = index();
    let mut owner = owner(Limits {
        cache_columns: 1,
        max_connections: 1,
        ..Limits::default()
    });
    let mut tick = 0;
    let p = page(0, 3);
    owner.interest(1, p, Tick(tick)).unwrap();
    owner.interest_raw(1, p, Tick(tick)).unwrap();
    assert!(!owner.interest_raw(1, p, Tick(tick)).unwrap());
    assert_eq!(owner.interest_raw(2, p, Tick(tick)), Err(Error::Capacity));
    drive(&mut owner, &world, &index, &mut tick);
    let head = owner.head(p, &world, &index).unwrap();
    assert!(owner.stats().cache_bytes >= 8 * 4096 * std::mem::size_of::<Cell>());
    let first = owner.raw_cells(1, p, &world, &index, Tick(tick)).unwrap();
    owner.forget(1, p);
    assert_eq!(owner.head(p, &world, &index), Some(head));
    assert!(Arc::ptr_eq(
        &first,
        &owner.raw_cells(1, p, &world, &index, Tick(tick)).unwrap()
    ));
    owner.interest(1, page(30, 3), Tick(tick)).unwrap();
    drive(&mut owner, &world, &index, &mut tick);
    assert!(owner.raw_cells(1, p, &world, &index, Tick(tick)).is_none());
    assert_eq!(owner.stats().queued, 1);
    drive(&mut owner, &world, &index, &mut tick);
    assert_eq!(owner.head(p, &world, &index), Some(head));
    assert_eq!(
        &*owner.raw_cells(1, p, &world, &index, Tick(tick)).unwrap(),
        &*first
    );
    owner.forget_peer(1);
    assert_eq!(owner.stats().connections, 1);
    assert_eq!(owner.stats().raw_leases, 1);
    assert_eq!(owner.head(p, &world, &index), Some(head));
    owner.forget_raw_peer(1);
    assert_eq!(
        (
            owner.stats().connections,
            owner.stats().leases,
            owner.stats().heads
        ),
        (0, 0, 0)
    );
    assert!(owner.raw_cells(1, p, &world, &index, Tick(tick)).is_none());
    owner.interest_raw(2, p, Tick(tick)).unwrap();
    drive(&mut owner, &world, &index, &mut tick);
    assert!(owner.head(p, &world, &index).unwrap().source > head.source);
}
#[test]
fn reserved_raw_limit_does_not_consume_far_slots_and_has_no_connection_id_alias() {
    let mut owner = owner(Limits {
        max_connections: 1,
        max_pages_per_connection: 1,
        ..Limits::default()
    });
    let connection = u64::MAX;
    for x in 0..256 {
        owner.interest_raw(connection, page(x, 3), Tick(0)).unwrap();
    }
    assert_eq!(
        owner.interest_raw(connection, page(256, 3), Tick(0)),
        Err(Error::Capacity)
    );
    owner.interest(connection, page(300, 3), Tick(0)).unwrap();
    assert_eq!(
        owner.interest(connection, page(301, 3), Tick(0)),
        Err(Error::Capacity)
    );
    assert_eq!(
        (
            owner.stats().connections,
            owner.stats().raw_leases,
            owner.stats().leases
        ),
        (1, 256, 257)
    );
    assert_eq!(
        owner.interest_raw(0, page(0, 3), Tick(0)),
        Err(Error::Capacity)
    );
    owner.forget_raw_peer(connection);
    assert_eq!((owner.stats().connections, owner.stats().leases), (1, 1));
    owner.forget_peer(connection);
    assert_eq!(
        (
            owner.stats().connections,
            owner.stats().leases,
            owner.stats().queued
        ),
        (0, 0, 0)
    );
    assert_eq!(
        owner.interest_raw(0, page(0, 9), Tick(0)),
        Err(Error::Bounds)
    );
    owner.interest_raw(0, page(0, 3), Tick(0)).unwrap();
}
#[test]
fn hidden_only_private_materialization_changes_certificate_but_not_public_heads_or_water() {
    for evict in [false, true] {
        let mut world = World::new(rules::properties, 32);
        let index = index();
        let mut owner = owner(Limits {
            cache_columns: 1,
            ..Limits::default()
        });
        let p = page(1, 2);
        let mut tick = 0;
        owner.interest(1, p, Tick(0)).unwrap();
        drive(&mut owner, &world, &index, &mut tick);
        let first = owner.water(p, &world, &index).unwrap();
        let public_ids = owner.stats().public_ids;
        assert_eq!(
            public_ids, 2,
            "one leased page owns exactly one geometry and one water revision"
        );
        let before_wire = encoded_owner_images(&mut owner, p, &world, &index);
        if evict {
            owner.interest(1, page(30, 2), Tick(tick)).unwrap();
            drive(&mut owner, &world, &index, &mut tick);
            assert!(owner.geometry(p, &world, &index).is_none());
        }
        let allocated_before_change = owner.stats().public_ids;
        let generator = generator();
        let column = generator.generate_column(1, 0).unwrap();
        let mut values = column.page(p, None).unwrap().cells;
        let secret = BlockPos {
            space: SpaceId(0),
            x: 21,
            y: 35,
            z: 3,
        };
        assert_eq!(values[usize::from(secret.local_index().get())], Cell::AIR);
        values[usize::from(secret.local_index().get())] = Cell::from_parts(rules::ids::LAVA, 0);
        let values: Box<[Cell]> = values;
        let mut tx = Transaction::new(Tick(tick), SpaceId(0));
        tx.create_page(p, values);
        let change = world.commit(tx).unwrap();
        owner.observe(&[change], Tick(tick));
        assert!(owner.head(p, &world, &index).is_none());
        drive(&mut owner, &world, &index, &mut tick);
        let after = owner.water(p, &world, &index).unwrap();
        assert_eq!(
            owner.stats().public_ids,
            allocated_before_change,
            "hidden-only recertification cannot consume future public IDs; evicted={evict}"
        );
        assert_eq!(
            (after.head, after.water_view, after.column_source),
            (first.head, first.water_view, first.column_source)
        );
        assert_eq!(after.cells, first.cells);
        assert_eq!(
            encoded_owner_images(&mut owner, p, &world, &index),
            before_wire,
            "all actual encoded Geometry/Water headers and bodies, evicted={evict}"
        );
        assert_eq!(
            world.read(secret),
            world::CellRead::Known(Cell::from_parts(rules::ids::LAVA, 0)),
            "public sealing never edits private authority"
        );
        assert!(
            owner.stats().witness_digest_bytes
                <= server::cold_projection_owner::MAX_CONTENT_HEADS * 64
        );
        owner.forget_peer(1);
        assert_eq!(
            (
                owner.stats().witness_rows,
                owner.stats().witness_digest_bytes
            ),
            (0, 0)
        );
    }
}
fn encoded_owner_images(
    owner: &mut Owner,
    p: PageId,
    w: &World,
    index: &HeightEditIndex,
) -> (Vec<u8>, Vec<u8>) {
    let (head, g) = owner.geometry(p, w, index).unwrap();
    let geometry = net::PublicGeometry {
        page: p,
        source_version: head.source,
        view_version: head.view,
        known: g.known,
        occupied: g.occupied,
        palette: g
            .palette
            .iter()
            .map(|s| net::PublicSurface {
                atlas_layer: s.atlas_layer,
                tint: s.tint,
                kind: match s.kind {
                    server::lod::PublicSurfaceKind::Opaque => net::PublicSurfaceKind::Opaque,
                    server::lod::PublicSurfaceKind::Cutout => net::PublicSurfaceKind::Cutout,
                    server::lod::PublicSurfaceKind::Water => net::PublicSurfaceKind::Water,
                },
            })
            .collect(),
        index_bits: g.index_bits,
        indices: g.indices.clone(),
    };
    let body = owner.water(p, w, index).unwrap();
    let water = net::water::WaterHalo {
        page: p,
        geometry_source: head.source,
        geometry_view: head.view,
        water_view: body.water_view,
        // The outer public Height ledger owns this binding; private HID is
        // intentionally excluded, exactly as in the canonical courier.
        column_source: 1,
        cells: Box::new(*body.cells),
        tints: body.tints,
    };
    (
        net::encode_server(&net::ServerMessage::Geometry(Box::new(geometry))).unwrap(),
        net::encode_server(&net::ServerMessage::WaterHalo(Box::new(water))).unwrap(),
    )
}

#[test]
fn full_content_witness_map_has_a_measured_bound_including_btree_node_overhead() {
    use server::{cold_projection::ContentWitness, cold_projection_owner::MAX_CONTENT_HEADS};
    let mut entries = std::collections::BTreeMap::new();
    let scope = telemetry::AllocationScope::begin().unwrap();
    for i in 0..MAX_CONTENT_HEADS {
        entries.insert(
            page(i as i32, 3),
            ContentWitness {
                geometry: [1; 32],
                water: [2; 32],
            },
        );
    }
    std::hint::black_box(&entries);
    let allocated = scope.finish().allocated_bytes;
    let logical = entries.len() * std::mem::size_of::<(PageId, ContentWitness)>();
    assert_eq!(entries.len(), 49_152);
    assert_eq!(
        entries.len() * std::mem::size_of::<ContentWitness>(),
        3 * 1024 * 1024
    );
    assert!(allocated >= logical as u64 && allocated < 16 * 1024 * 1024);
    eprintln!(
        "publicWitness rows{} digestBytes{} logicalRowBytes{logical} actualBTreeHeapBytes{allocated} nodeAndSlackBytes{}",
        entries.len(),
        entries.len() * 64,
        allocated - logical as u64
    );
}
#[test]
fn combined_content_head_limit_rejects_before_mutation_and_shared_raw_lease_keeps_its_row() {
    use server::cold_projection_owner::MAX_CONTENT_HEADS;
    let mut o = owner(Limits {
        max_pages_per_connection: 4096,
        ..Limits::default()
    });
    for i in 0..MAX_CONTENT_HEADS {
        o.interest((i / 4096) as u64, page(i as i32, 3), Tick(0))
            .unwrap();
    }
    let before = o.stats();
    assert_eq!(before.leases, MAX_CONTENT_HEADS);
    assert_eq!(
        o.interest(12, page(1_000_000, 3), Tick(0)),
        Err(Error::Capacity)
    );
    assert_eq!(o.stats(), before);
    // A second lease of an existing row is independent of the unique-row cap.
    o.interest_raw(12, page(0, 3), Tick(0)).unwrap();
    o.forget(0, page(0, 3));
    assert_eq!(o.stats().leases, MAX_CONTENT_HEADS);
    assert_eq!(
        o.interest(12, page(1_000_000, 3), Tick(0)),
        Err(Error::Capacity)
    );
    o.forget_raw(12, page(0, 3));
    assert_eq!(o.stats().leases, MAX_CONTENT_HEADS - 1);
    o.interest(12, page(1_000_000, 3), Tick(0)).unwrap();
    assert_eq!(o.stats().leases, MAX_CONTENT_HEADS);
}
#[test]
fn neighbour_only_portal_changes_centre_public_head_even_after_body_eviction() {
    for evict in [false, true] {
        let mut world = World::new(rules::properties, 64);
        let mut tx = Transaction::new(Tick(0), SpaceId(0));
        for x in 0..2 {
            for y in 0..8 {
                tx.create_page(
                    page(x, y),
                    vec![Cell::from_parts(rules::ids::STONE, 0); 4096],
                );
            }
        }
        for x in 15..18 {
            tx.set_cell(
                BlockPos {
                    space: SpaceId(0),
                    x,
                    y: 40,
                    z: 8,
                },
                Cell::AIR,
            );
        }
        world.commit(tx).unwrap();
        let index = index();
        let mut owner = owner(Limits {
            cache_columns: 1,
            ..Limits::default()
        });
        let p = page(0, 2);
        let mut tick = 0;
        owner.interest(1, p, Tick(tick)).unwrap();
        drive(&mut owner, &world, &index, &mut tick);
        let first = owner.water(p, &world, &index).unwrap();
        let before_body = owner.geometry(p, &world, &index).unwrap().1;
        if evict {
            owner.interest(2, page(30, 2), Tick(tick)).unwrap();
            drive(&mut owner, &world, &index, &mut tick);
            assert!(owner.geometry(p, &world, &index).is_none());
        }
        let centre_version = world.input_version(p, server::far_water::capture::CHANNELS);
        let mut tx = Transaction::new(Tick(tick), SpaceId(0));
        for y in 40..128 {
            tx.set_cell(
                BlockPos {
                    space: SpaceId(0),
                    x: 17,
                    y,
                    z: 8,
                },
                Cell::AIR,
            );
        }
        let change = world.commit(tx).unwrap();
        assert_eq!(
            world.input_version(p, server::far_water::capture::CHANNELS),
            centre_version
        );
        owner.observe(&[change], Tick(tick));
        drive(&mut owner, &world, &index, &mut tick);
        let after = owner.water(p, &world, &index).unwrap();
        let after_body = owner.geometry(p, &world, &index).unwrap().1;
        assert_ne!(after_body.as_ref(), before_body.as_ref());
        assert!(
            after.head.source > first.head.source && after.head.view > first.head.view,
            "unchanged centre input cannot reuse a head for a changed exterior body; evicted={evict}"
        );
        assert!(after.water_view > first.water_view);
        assert_eq!(owner.take_changed_geometry(), Some(p));
        assert!(owner.take_changed_geometry().is_none());
        let index = 16 + 18 * (9 + 18 * 9); // centre local[15,8,8], X/Z/Y
        assert_eq!(first.cells[index], 2);
        assert_eq!(after.cells[index], 1);
    }
}
