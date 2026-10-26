//! Complete owner-loop observation; allocations include capture, clones,
//! encoding, acknowledgement/replay and transport work on the owner thread.
use super::{
    bots::{Bot, Fleet, Traffic},
    fixture::{self, BOTS, FLUIDS, MACHINES, MOBS, Prepared},
};
use core_types::Cell;
use rules::{
    entities::mob::Mob,
    ids::WATER,
    items::{I_DUST_IRON, I_IRON_INGOT},
    machines::Machine,
};
use server::{Server, transport::Runtime};
use std::{
    fs::File,
    io::{BufWriter, Write},
    path::PathBuf,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use telemetry::{AllocationCounts, AllocationScope};
fn add(a: &mut AllocationCounts, b: AllocationCounts) {
    a.alloc_calls += b.alloc_calls;
    a.zeroed_alloc_calls += b.zeroed_alloc_calls;
    a.realloc_calls += b.realloc_calls;
    a.dealloc_calls += b.dealloc_calls;
    a.failed_calls += b.failed_calls;
    a.allocated_bytes += b.allocated_bytes;
    a.released_bytes += b.released_bytes;
}
struct Driver {
    runtime: Runtime,
    bots: Fleet,
    started: Instant,
    output: BufWriter<File>,
    totals: AllocationCounts,
    polls: u64,
    phase: &'static str,
    report: PathBuf,
    directory: PathBuf,
}
impl Drop for Driver {
    fn drop(&mut self) {
        // Keep bounded per-peer numeric evidence even if a semantic oracle fails.
        for (i, row) in self.bots.stats().iter().enumerate() {
            let _ = writeln!(
                self.output,
                "{{\"kind\":\"peer_last_observation\",\"ordinal\":{},\"sent_bytes\":{},\"received_bytes\":{},\"entity_frames\":{},\"entity_poses\":{},\"lost\":{},\"reordered\":{},\"receive_pressure\":{},\"discarded_unreliable_assemblies\":{},\"peak_link_bytes\":{}}}",
                i,
                row.traffic.sent_bytes,
                row.traffic.received_bytes,
                row.traffic.entity_frames,
                row.traffic.entity_poses,
                row.lost,
                row.traffic.reordered,
                row.traffic.receive_pressure,
                row.discarded_unreliable,
                row.peak_link_bytes
            );
        }
        let _ = self.output.flush();
    }
}
impl Driver {
    fn poll(&mut self, frozen: Option<u64>) {
        let real = self.started.elapsed().as_millis() as u64;
        self.bots.check();
        let now = frozen.unwrap_or(real);
        let before = self.runtime.server().counters();
        let tick_before = self.runtime.server().current_tick();
        let scope = AllocationScope::begin().unwrap();
        let poll_started = Instant::now();
        let result = self.runtime.poll(now);
        let poll_micros = poll_started.elapsed().as_micros();
        let counts = scope.finish();
        add(&mut self.totals, counts);
        self.polls += 1;
        let after = self.runtime.server().counters();
        let load = self.runtime.server().load_metrics();
        if load.tick != tick_before
            || after.durable_batches != before.durable_batches
            || after.checkpoint_requests != before.checkpoint_requests
            || after.mob_steps != before.mob_steps
            || after.machine_steps != before.machine_steps
            || after.machine_prepared != before.machine_prepared
            || after.survival_events != before.survival_events
        {
            writeln!(self.output,"{{\"kind\":\"poll\",\"phase\":\"{}\",\"poll_micros_information_only\":{},\"before_tick\":{},\"tick\":{},\"mob_steps\":{},\"machine_steps\":{},\"machine_prepared\":{},\"machine_events\":{},\"machine_unknown\":{},\"machine_deferred\":{},\"machine_copy_bytes\":{},\"machine_stage\":\"{}\",\"machine_phase_pending\":{},\"fluid_events\":{},\"survival_reads\":{},\"survival_writes\":{},\"entity_overflows\":{},\"wal_bytes\":{},\"light_cells\":{},\"handoff_bytes\":{},\"replay_bytes\":{},\"checkpoints\":{},\"checkpoint_runtime_bytes\":{},\"alloc_calls\":{},\"allocated_bytes\":{},\"released_bytes\":{},\"live_mobs\":{},\"active_mobs\":{},\"registered_machines\":{},\"fluid_queue\":{},\"fluid_event_bytes\":{},\"fluid_oldest_ticks\":{},\"machine_queue\":{},\"machine_event_bytes\":{},\"machine_oldest_ticks\":{},\"pending_mob_actors\":{},\"light_pending\":{},\"light_bootstrap\":{}}}",self.phase,poll_micros,tick_before.0,load.tick.0,after.mob_steps-before.mob_steps,after.machine_steps-before.machine_steps,after.machine_prepared-before.machine_prepared,after.machine_events-before.machine_events,after.machine_unknown-before.machine_unknown,after.machine_deferred-before.machine_deferred,after.machine_copy_bytes-before.machine_copy_bytes,match load.machine_phase_stage {None=>"idle",Some(server::machines::OwnerStage::Prepare)=>"prepare",Some(server::machines::OwnerStage::Machines)=>"machines",Some(server::machines::OwnerStage::Resources)=>"resources",Some(server::machines::OwnerStage::Transit)=>"transit"},load.machine_phase_pending,after.survival_events-before.survival_events,after.survival_reads-before.survival_reads,after.survival_writes-before.survival_writes,after.entity_overflows-before.entity_overflows,load.durable_wal_bytes,after.light_cells-before.light_cells,after.runtime_handoff_bytes-before.runtime_handoff_bytes,after.runtime_replay_input_bytes-before.runtime_replay_input_bytes,after.checkpoint_requests-before.checkpoint_requests,after.checkpoint_runtime_bytes-before.checkpoint_runtime_bytes,counts.allocating_calls(),counts.allocated_bytes,counts.released_bytes,load.mobs,load.active_mobs,load.registered_machines,load.survival.entries,load.survival.event_payload_bytes,load.survival.oldest_overdue_ticks,load.machines.entries,load.machines.event_payload_bytes,load.machines.oldest_overdue_ticks,load.pending_mob_actors,after.light_pending_cells,after.light_pending_bootstrap_pages).unwrap();
        }
        if result.is_err() || self.runtime.server().is_failed() {
            let reason = self.runtime.server().failure_reason();
            let wal = std::fs::metadata(self.directory.join("world.wal")).map_or(0, |m| m.len());
            writeln!(self.output,"{{\"kind\":\"failed\",\"tick\":{},\"wal_bytes\":{},\"operation\":{:?},\"detail\":{:?}}}",load.tick.0,wal,reason.map_or("transport",|r|r.operation),reason.and_then(|r|r.detail).unwrap_or("none")).unwrap();
            self.output.flush().unwrap();
            panic!(
                "actual scale owner failed {:?}; numeric report {}",
                reason,
                self.report.display()
            );
        }
    }
    fn ready(&self) -> bool {
        self.bots.ready(BOTS)
    }
    fn checkpoint(&mut self, frozen: u64) {
        // Checkpoint captures are reported separately and remain inside the complete
        // owner allocation scope when the request occurs from Runtime::poll.
        let checkpoint_scope = AllocationScope::begin().unwrap();
        assert!(self.runtime.server_mut().request_checkpoint());
        let checkpoint_counts = checkpoint_scope.finish();
        add(&mut self.totals, checkpoint_counts);
        writeln!(
            self.output,
            "{{\"kind\":\"manual_checkpoint_capture\",\"allocated_bytes\":{},\"alloc_calls\":{}}}",
            checkpoint_counts.allocated_bytes,
            checkpoint_counts.allocating_calls()
        )
        .unwrap();
        while self.runtime.server().awaiting_durability() {
            self.poll(Some(frozen));
            thread::sleep(Duration::from_micros(300));
        }
    }
    fn drain(&mut self) -> u64 {
        // Outside the measured window hold the wall-clock admission frontier fixed.
        // An already-running phase may finish and at most one next tick can start;
        // all durable completions are still serviced by the real Runtime/owner.
        let now = self.started.elapsed().as_millis() as u64;
        let start = self.runtime.server().current_tick();
        let until = Instant::now() + Duration::from_secs(60);
        loop {
            self.poll(Some(now));
            let m = self.runtime.server().load_metrics();
            if !m.awaiting_durability && !m.mob_phase_pending && !m.machine_phase_pending {
                assert!(m.tick.0 <= start.0 + 1);
                return now;
            }
            assert!(Instant::now() < until, "phase drain {:?}", m);
            thread::sleep(Duration::from_micros(300));
        }
    }
}
pub fn run() -> PathBuf {
    let prepared = fixture::prepare();
    run_prepared(prepared)
}
fn run_prepared(prepared: Prepared) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../.local/artifacts")
        .join(format!(
            "spec86-{}-{}.jsonl",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            std::process::id()
        ));
    let output = BufWriter::new(File::create(&path).unwrap());
    let server = Server::open(prepared.directory.config()).unwrap();
    let initial = server.load_metrics();
    assert_eq!(
        (
            initial.mobs,
            initial.registered_machines,
            initial.survival.entries
        ),
        (MOBS, MACHINES, FLUIDS)
    );
    let local = "127.0.0.1:0".parse().unwrap();
    let runtime = Runtime::bind(server, local, local).unwrap();
    let address = runtime.udp_address().unwrap();
    let bots = prepared
        .hellos
        .iter()
        .cloned()
        .enumerate()
        .map(|(i, h)| Bot::new(address, h, i as u64 + 1, fixture::peer_position(i)))
        .collect();
    let started = Instant::now();
    let mut d = Driver {
        runtime,
        bots: Fleet::start(bots, started),
        started,
        output,
        totals: AllocationCounts::ZERO,
        polls: 0,
        phase: "live",
        report: path.clone(),
        directory: prepared.directory.0.clone(),
    };
    writeln!(d.output,"{{\"kind\":\"config\",\"bots\":{},\"mobs\":{},\"machines\":{},\"known_water_cells\":{},\"delay_ms\":75,\"jitter_ms\":20,\"loss_percent\":2,\"start_tick\":{},\"water_due_tick\":{},\"allocator_scope\":\"complete_Runtime_poll_owner_thread\",\"hardware_ms_gate\":false}}",BOTS,MOBS,MACHINES,FLUIDS,prepared.start_tick.0,prepared.measurement_tick.0).unwrap();
    d.output.flush().unwrap();
    let deadline = Instant::now() + Duration::from_secs(900);
    let mut baseline: Option<Vec<Mob>> = None;
    let mut machine_reference = Vec::new();
    let mut reference_tick = None;
    let mut last_status = Instant::now();
    loop {
        d.poll(None);
        let load = d.runtime.server().load_metrics();
        assert!(
            load.tick.0 < prepared.measurement_tick.0 + 1400,
            "logical workload did not drain"
        );
        if load.tick.0 >= prepared.measurement_tick.0 && baseline.is_none() {
            assert!(
                d.ready(),
                "all32 actual UDP sessions must join, receive authority TP and exchange pongs before due workload"
            );
            assert_eq!(load.active_mobs, MOBS);
            d.drain();
            reference_tick = Some(d.runtime.server().current_tick());
            baseline = Some(d.runtime.server().mob_entities().rows().to_vec());
            for &id in prepared.machine_ids.iter().step_by(157) {
                machine_reference.push((
                    id,
                    server::machines::read_machine(d.runtime.server().world(), id).unwrap(),
                ));
            }
        }
        if baseline.is_some()
            && load.survival.entries == 0
            && d.runtime.server().counters().survival_events >= FLUIDS as u64
            && load.tick.0 >= prepared.measurement_tick.0 + 8
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "host liveness guard, not a performance threshold: {:?}",
            load
        );
        if last_status.elapsed() > Duration::from_secs(10) {
            eprintln!(
                "scale_progress tick={} mobs={}/{} machines={} fluid_queue={} fluid_work={} report={}",
                load.tick.0,
                load.active_mobs,
                load.mobs,
                load.registered_machines,
                load.survival.entries,
                d.runtime.server().counters().survival_events,
                path.display()
            );
            d.output.flush().unwrap();
            last_status = Instant::now();
        }
        thread::sleep(Duration::from_micros(500));
    }
    assert_eq!(
        d.bots.pages_ready(),
        BOTS,
        "all public page requests must complete during the measured actual workload"
    );
    let _live_frozen = d.drain();
    let end = d.runtime.server().current_tick();
    let counters = d.runtime.server().counters();
    assert_eq!(counters.survival_events, FLUIDS as u64);
    assert_eq!(d.runtime.server().mob_entities().len(), MOBS);
    assert!(counters.machine_steps >= MACHINES as u64);
    for before in baseline.unwrap() {
        let after = d.runtime.server().mob_entities().get(before.id).unwrap();
        assert_eq!(
            after.age - before.age,
            after.last_tick.0 - before.last_tick.0,
            "active known body must execute each canonical tick once"
        );
        assert!(end.0 - after.last_tick.0 <= 1);
        assert!(after.body.position.iter().all(|v| v.is_finite()));
    }
    for &p in &prepared.water_cells {
        assert_eq!(
            d.runtime.server().world().read(p),
            world::CellRead::Known(Cell::from_parts(WATER, 0))
        );
    }
    struct NoExternalIo;
    impl rules::machines::MachineWorld for NoExternalIo {}
    let mut isolated = NoExternalIo;
    let reference_ticks = end.0 - reference_tick.expect("confirmed reference boundary").0;
    for (id, mut expected) in machine_reference {
        for _ in 0..reference_ticks {
            expected.tick([0; 3], rules::machines::Face::North, &mut isolated);
        }
        let actual = server::machines::read_machine(d.runtime.server().world(), id).unwrap();
        assert_eq!(
            (
                actual.energy,
                actual.progress,
                actual.slots[0],
                actual.slots[1]
            ),
            (
                expected.energy,
                expected.progress,
                expected.slots[0],
                expected.slots[1]
            ),
            "sampled isolated Smelter must match one Detailed rule tick per elapsed global tick; id={id:?} elapsed={reference_ticks}"
        );
    }
    let mut min_steps = u64::MAX;
    let mut max_steps = 0;
    for &id in &prepared.machine_ids {
        let record = d.runtime.server().world().component(id).unwrap();
        let m = Machine::decode(&record.data.payload).unwrap();
        assert_eq!(m.energy % 50, 0);
        let steps = (20_000 - m.energy) / 50;
        assert!(steps > 0, "every actual registered machine must work");
        let output = if m.slots[1].stack.is_empty() {
            0
        } else {
            assert_eq!(m.slots[1].stack.item(), I_IRON_INGOT);
            u64::from(m.slots[1].stack.count())
        };
        assert_eq!(u64::from(m.progress), steps % 200);
        assert_eq!(output, steps / 200);
        assert_eq!(m.slots[0].stack.item(), I_DUST_IRON);
        assert_eq!(u64::from(m.slots[0].stack.count()) + output, 64);
        min_steps = min_steps.min(steps);
        max_steps = max_steps.max(steps);
    }
    // Sleeping population remains present. Only actual authorized spectator
    // commands can remove observers from the active owner sections.
    d.phase = "sleep_transition";
    d.bots.spectate();
    let until = Instant::now() + Duration::from_secs(180);
    loop {
        d.poll(None);
        let load = d.runtime.server().load_metrics();
        let c = d.runtime.server().counters();
        if d.bots.spectators() == BOTS
            && load.active_mobs == 0
            && load.machines.entries == 0
            && !load.awaiting_durability
            && !load.mob_phase_pending
            && !load.machine_phase_pending
            && c.light_pending_cells == 0
            && c.light_pending_seed_pages == 0
            && c.light_pending_sky_pages == 0
            && c.light_pending_bootstrap_pages == 0
        {
            break;
        }
        assert!(Instant::now() < until, "sleep transition {load:?}");
        thread::sleep(Duration::from_micros(500));
    }
    let cold_frozen = d.drain();
    d.checkpoint(cold_frozen);
    let cold_before = d.runtime.server().counters();
    let cold_allocated = d.totals.allocated_bytes;
    let cold_start = d.runtime.server().current_tick();
    d.phase = "sleeping_5000_mobs_10000_machines";
    while d.runtime.server().current_tick().0 < cold_start.0 + 32 {
        d.poll(None);
        thread::sleep(Duration::from_micros(500));
    }
    d.drain();
    let cold_after = d.runtime.server().counters();
    assert_eq!(d.runtime.server().mob_entities().len(), MOBS);
    assert_eq!(
        d.runtime.server().load_metrics().registered_machines,
        MACHINES
    );
    assert_eq!(cold_before.mob_steps, cold_after.mob_steps);
    assert_eq!(cold_before.machine_steps, cold_after.machine_steps);
    assert_eq!(cold_before.survival_events, cold_after.survival_events);
    assert_eq!(
        cold_before.runtime_handoff_bytes,
        cold_after.runtime_handoff_bytes
    );
    assert_eq!(
        cold_before.runtime_replay_input_bytes,
        cold_after.runtime_replay_input_bytes
    );
    assert_eq!(
        cold_before.checkpoint_requests,
        cold_after.checkpoint_requests
    );
    writeln!(d.output,"{{\"kind\":\"sleeping_complete\",\"ticks\":{},\"retained_mobs\":{},\"retained_machines\":{},\"runtime_handoff_bytes\":0,\"runtime_replay_bytes\":0,\"owner_allocated_bytes\":{}}}",d.runtime.server().current_tick().0-cold_start.0,MOBS,MACHINES,d.totals.allocated_bytes-cold_allocated).unwrap();
    let world_hash = d.runtime.server().world().canonical_hash();
    let mob_samples: Vec<_> = d
        .runtime
        .server()
        .mob_entities()
        .rows()
        .iter()
        .step_by(37)
        .copied()
        .collect();
    let machine_samples: Vec<_> = prepared
        .machine_ids
        .iter()
        .step_by(71)
        .map(|&id| {
            (
                id,
                d.runtime
                    .server()
                    .world()
                    .component(id)
                    .unwrap()
                    .data
                    .clone(),
            )
        })
        .collect();
    let frozen = d.drain();
    d.checkpoint(frozen);
    let transport = d.runtime.counters();
    let mut total_lost = 0;
    let bots = d.bots.finish();
    for (i, bot) in bots.iter().enumerate() {
        let t: Traffic = bot.traffic;
        assert!(t.sent_bytes > 0 && t.received_bytes > 0 && t.pongs > 0);
        assert!(bot.queue_peak_bytes() <= 524_288);
        total_lost += bot.loss();
        writeln!(d.output,"{{\"kind\":\"peer\",\"ordinal\":{},\"sent_bytes\":{},\"received_bytes\":{},\"sent_packets\":{},\"received_packets\":{},\"entity_frames\":{},\"entity_poses\":{},\"receive_pressure\":{},\"discarded_unreliable_assemblies\":{},\"lost\":{},\"peak_link_bytes\":{}}}",i,t.sent_bytes,t.received_bytes,t.sent_packets,t.received_packets,t.entity_frames,t.entity_poses,t.receive_pressure,bot.discarded_snapshots(),bot.loss(),bot.queue_peak_bytes()).unwrap();
    }
    assert!(total_lost > 0);
    assert_eq!(transport.peak_connections, BOTS);
    assert_eq!(transport.queue_overflows, 0);
    writeln!(d.output,"{{\"kind\":\"live_complete\",\"tick\":{},\"fluid_visits\":{},\"machine_steps\":{},\"mob_steps\":{},\"min_machine_steps\":{},\"max_machine_steps\":{},\"owner_polls\":{},\"owner_allocated_bytes\":{},\"owner_released_bytes\":{},\"handoff_bytes\":{},\"replay_bytes\":{},\"checkpoint_runtime_bytes\":{},\"wall_ms_information_only\":{}}}",end.0,counters.survival_events,counters.machine_steps,counters.mob_steps,min_steps,max_steps,d.polls,d.totals.allocated_bytes,d.totals.released_bytes,counters.runtime_handoff_bytes,counters.runtime_replay_input_bytes,counters.checkpoint_runtime_bytes,d.started.elapsed().as_millis()).unwrap();
    d.output.flush().unwrap();
    drop(d);
    let reopened = Server::open(prepared.directory.config()).unwrap();
    assert_eq!(reopened.world().canonical_hash(), world_hash);
    for mob in mob_samples {
        assert_eq!(reopened.mob_entities().get(mob.id), Some(&mob));
    }
    for (id, data) in machine_samples {
        assert_eq!(reopened.world().component(id).unwrap().data, data);
    }
    drop(reopened);
    let mut final_record = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    writeln!(final_record,"{{\"kind\":\"restart_exact\",\"world\":true,\"mob_samples\":true,\"machine_samples\":true}} ").unwrap();
    path
}
