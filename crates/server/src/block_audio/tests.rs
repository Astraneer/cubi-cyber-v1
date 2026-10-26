use super::*;
use std::{
    path::PathBuf,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct H {
    server: Server,
    directory: Directory,
    config: ServerConfig,
    now: u64,
    next: u64,
    out: Vec<Outgoing>,
}
impl H {
    fn new(wall: bool, unknown: bool) -> Self {
        Self::with_side_wall(wall, unknown, false)
    }
    fn with_side_wall(wall: bool, unknown: bool, side_wall: bool) -> Self {
        let directory = Directory(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../.local/tmp")
                .join(format!(
                    "block-audio-{}-{}",
                    std::process::id(),
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                )),
        );
        let mut config = ServerConfig::new(&directory.0);
        config.generation_enabled = false;
        config.spawn_override = Some([8.5, 129., 8.5]);
        let mut world = World::new(rules::properties, 1 << 20);
        let mut tx = Transaction::new(Tick(0), SpaceId(0));
        for z in if unknown { vec![0, 2] } else { vec![0] } {
            let mut cells = vec![Cell::AIR; 4096];
            cells[..256].fill(Cell::from_parts(rules::ids::STONE, 0));
            if wall && z == 0 {
                for y in 1..7 {
                    for x in 0..16 {
                        cells[x + 16 * 10 + 256 * y] = Cell::from_parts(rules::ids::STONE, 0);
                    }
                }
            }
            if side_wall && z == 0 {
                for y in 1..7 {
                    for z in 0..16 {
                        cells[11 + 16 * z + 256 * y] = Cell::from_parts(rules::ids::STONE, 0);
                    }
                }
            }
            tx.create_page(
                PageId {
                    space: SpaceId(0),
                    x: 0,
                    y: 8,
                    z,
                },
                cells,
            );
        }
        world.commit(tx).unwrap();
        let server = Server::open_with_world(config.clone(), world).unwrap();
        let mut h = Self {
            server,
            directory,
            config,
            now: 0,
            next: 1,
            out: Vec::new(),
        };
        let deadline = Instant::now() + Duration::from_secs(20);
        while !h.server.height_ready_for_publication() {
            assert!(Instant::now() < deadline);
            h.step();
        }
        let hello =
            net::decode_client(&std::fs::read(h.directory.0.join("owner.identity")).unwrap())
                .unwrap();
        h.send(1, hello);
        h.wait(|o| o.connection == 1 && matches!(o.message, ServerMessage::Welcome(_)));
        h.send(
            2,
            ClientMessage::Hello(Hello {
                identity: PlayerId::default(),
                name: "block-listener".into(),
                credential: None,
            }),
        );
        h.wait(|o| o.connection == 2 && matches!(o.message, ServerMessage::Welcome(_)));
        h.action(Action::Command("/gamemode creative".into()));
        h
    }
    fn send(&mut self, c: u64, m: ClientMessage) {
        self.out.extend(self.server.handle(
            c,
            net::decode_client(&net::encode_client(&m).unwrap()).unwrap(),
            self.now,
        ));
    }
    fn step(&mut self) {
        self.now += 10;
        self.out.extend(self.server.tick(self.now));
        assert!(
            !self.server.is_failed(),
            "{:?}",
            self.server.failure_reason()
        );
        thread::sleep(Duration::from_micros(300));
    }
    fn wait(&mut self, mut f: impl FnMut(&Outgoing) -> bool) -> Outgoing {
        let end = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(i) = self.out.iter().position(&mut f) {
                return self.out.remove(i);
            }
            assert!(Instant::now() < end, "{:?}", self.server.counters());
            self.step();
        }
    }
    fn action(&mut self, action: Action) -> ActionId {
        let id = ActionId(self.next);
        self.next += 1;
        self.send(1, ClientMessage::Action { id, action });
        self.accept(id);
        id
    }
    fn accept(&mut self, id: ActionId) {
        let o=self.wait(|o|o.connection==1&&matches!(o.message,ServerMessage::Action(r)if r.id==id&&r.status!=ActionStatus::Pending));
        assert!(
            matches!(o.message,ServerMessage::Action(r)if r.status==ActionStatus::Accepted),
            "{:?}",
            o.message
        );
    }
    fn raw(&mut self, page: PageId) {
        self.send(
            2,
            ClientMessage::RequestPage {
                page,
                acknowledged: None,
            },
        );
        let o=self.wait(|o|o.connection==2&&matches!(&o.message,ServerMessage::Page(net::PageUpdate::Snapshot(p))if p.page==page));
        let ServerMessage::Page(net::PageUpdate::Snapshot(p)) = o.message else {
            unreachable!()
        };
        self.send(
            2,
            ClientMessage::AckPage {
                page,
                version: p.version,
            },
        );
        let end = Instant::now() + Duration::from_secs(20);
        while self.server.peers[&2].history.confirmed_version(page) != Some(p.version) {
            assert!(Instant::now() < end);
            self.step();
        }
        assert_eq!(
            self.server.peers[&2].history.confirmed_version(page),
            Some(p.version)
        );
    }
    fn place_start(&mut self, position: BlockPos) -> ActionId {
        let id = ActionId(self.next);
        self.next += 1;
        self.send(
            1,
            ClientMessage::AimedAction {
                id,
                yaw: 0.,
                pitch: 30.,
                action: Action::Place {
                    position,
                    expected: Cell::AIR,
                    cell: Cell::from_parts(rules::ids::GLASS, 0),
                },
            },
        );
        id
    }
    fn place(&mut self, position: BlockPos) -> ActionId {
        let id = self.place_start(position);
        self.accept(id);
        id
    }
    fn mine(&mut self, position: BlockPos) -> ActionId {
        let peer = &self.server.peers[&1];
        let sequence = peer.input_sequence + 1;
        let target = Tick(self.server.current_tick().0 + 1);
        self.send(
            1,
            ClientMessage::Inputs(vec![InputFrame {
                sequence,
                target_tick: target,
                keys: InputFrame::ATTACK,
                yaw: 0.,
                pitch: 30.,
            }]),
        );
        let id = ActionId(self.next);
        self.next += 1;
        self.send(
            1,
            ClientMessage::AimedAction {
                id,
                yaw: 0.,
                pitch: 30.,
                action: Action::Mine {
                    position,
                    expected: Cell::from_parts(rules::ids::GLASS, 0),
                    tool: 0,
                },
            },
        );
        self.accept(id);
        id
    }
    fn cues(&self, id: ActionId) -> Vec<(u64, Cue)> {
        self.out
            .iter()
            .filter_map(|o| match o.message {
                ServerMessage::AudioCue(c) if c.kind.block().is_some() && c.sequence == id.0 => {
                    Some((o.connection, c))
                }
                _ => None,
            })
            .collect()
    }
}
fn position(z: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x: 8,
        y: 129,
        z,
    }
}
#[test]
fn real_owner_and_acknowledged_listener_hear_the_same_durable_action_once_and_restart_never_replays_it()
 {
    let mut h = H::new(false, false);
    let target = position(11);
    h.raw(target.page());
    let before = h.server.load_metrics().durable_wal_bytes;
    let placed = h.place(target);
    assert!(h.server.load_metrics().durable_wal_bytes > before);
    let cues = h.cues(placed);
    assert_eq!(cues.iter().map(|c| c.0).collect::<Vec<_>>(), [1, 2]);
    let cue = cues[0].1;
    assert_eq!(cues[1].1, cue);
    assert!(
        matches!(cue.kind,Kind::BlockPlace(a)if a.player==h.server.peers[&1].identity&&a.material==BlockMaterial::Glass)
    );
    let duplicate = ClientMessage::AimedAction {
        id: placed,
        yaw: 0.,
        pitch: 30.,
        action: Action::Place {
            position: target,
            expected: Cell::AIR,
            cell: Cell::from_parts(rules::ids::GLASS, 0),
        },
    };
    h.send(1, duplicate);
    h.accept(placed);
    assert_eq!(h.cues(placed).len(), 2);
    h.raw(target.page());
    let mined = h.mine(target);
    let mined_cues = h.cues(mined);
    assert_eq!(mined_cues.len(), 2);
    assert!(
        mined_cues
            .iter()
            .all(|(_, c)| matches!(c.kind,Kind::BlockBreak(a)if a.material==BlockMaterial::Glass))
    );
    assert_eq!(h.server.world().read(target), CellRead::Known(Cell::AIR));
    let directory = h.directory.0.clone();
    let config = h.config.clone();
    drop(h.server);
    let (store, recovery) = Store::open(&directory).unwrap();
    assert!(
        recovery
            .batches
            .iter()
            .any(|(_, batch)| batch.changes.iter().any(|change| change
                .cells
                .iter()
                .any(|c| c.page == target.page() && c.index == target.local_index())
                && change
                    .events
                    .iter()
                    .any(|e| decode_event(change, e).is_some_and(|(_, c, _)| c == cue))))
    );
    drop(store);
    let mut server = Server::open(config).unwrap();
    let hello =
        net::decode_client(&std::fs::read(directory.join("owner.identity")).unwrap()).unwrap();
    let mut out = server.handle(1, hello, 0);
    for now in (0..500).step_by(10) {
        out.extend(server.tick(now));
        thread::sleep(Duration::from_micros(300));
    }
    assert!(
        !out.iter()
            .any(|o| matches!(o.message,ServerMessage::AudioCue(c)if c.kind.block().is_some())),
        "recovered WAL must not replay transient block audio"
    );
}
#[test]
fn hidden_unknown_and_unacknowledged_raw_never_become_an_audio_side_channel() {
    for (wall, unknown, z) in [(true, false, 14), (false, true, 36), (false, false, 11)] {
        let mut h = H::new(wall, unknown);
        let target = position(z);
        if wall {
            h.action(Action::Command("/tp 8.5 129 11.5".into()));
        }
        if unknown {
            h.action(Action::Command("/tp 8.5 129 33.5".into()));
        }
        if wall || unknown {
            h.raw(target.page());
        }
        let id = h.place(target);
        let cues = h.cues(id);
        assert_eq!(
            cues.iter().map(|c| c.0).collect::<Vec<_>>(),
            [1],
            "wall={wall} unknown={unknown}"
        );
        if unknown {
            assert!(h.server.counters().block_audio_cell_reads > 0);
        }
    }
}
#[test]
fn retired_then_reentered_without_ack_revokes_the_old_witness_and_pending_edits_are_silent() {
    let mut h = H::new(false, false);
    let target = position(11);
    h.raw(target.page());
    h.send(2, ClientMessage::ForgetPage(target.page()));
    h.send(
        2,
        ClientMessage::RequestPage {
            page: target.page(),
            acknowledged: None,
        },
    );
    let id = h.place_start(target);
    let end = Instant::now() + Duration::from_secs(20);
    while h.server.world().read(target) != CellRead::Known(Cell::from_parts(rules::ids::GLASS, 0)) {
        assert!(Instant::now() < end);
        h.step();
    }
    assert!(h.server.pending.is_some());
    assert!(
        h.cues(id).is_empty(),
        "edit before fsync publication must be silent"
    );
    h.accept(id);
    assert_eq!(h.cues(id).iter().map(|c| c.0).collect::<Vec<_>>(), [1]);
}

#[test]
#[cfg(unix)]
fn actual_writer_storage_bound_failure_publishes_neither_block_sound_nor_acceptance() {
    use std::os::unix::fs::MetadataExt;
    let mut h = H::new(false, false);
    let target = position(11);
    h.raw(target.page());
    let deadline = Instant::now() + Duration::from_secs(20);
    while h.server.pending.is_some() {
        assert!(Instant::now() < deadline);
        h.out.extend(h.server.tick(h.now));
        thread::sleep(Duration::from_micros(300));
    }
    let path = h.directory.0.join("world.wal");
    let previous = std::fs::metadata(&path).unwrap();
    // ftruncate creates a sparse private fixture, not 512MiB of filled disk.
    // This exercises a real SaveWorker error before publication; it does not
    // pretend that a platform sync_all syscall error was injected.
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(save::MAX_WAL_BYTES)
        .unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().blocks(),
        previous.blocks()
    );
    h.out.clear();
    let id = h.place_start(target);
    while !h.server.is_failed() {
        assert!(Instant::now() < deadline);
        h.now += 10;
        h.out.extend(h.server.tick(h.now));
        thread::sleep(Duration::from_micros(300));
    }
    assert_eq!(
        h.server.world().read(target),
        CellRead::Known(Cell::from_parts(rules::ids::GLASS, 0)),
        "edit actually reached the failing durable batch"
    );
    assert!(h.cues(id).is_empty());
    assert!(!h.out.iter().any(|o|matches!(o.message,ServerMessage::Action(r)if r.id==id&&r.status==ActionStatus::Accepted)));
    assert!(h.out.iter().any(|o| matches!(
        o.message,
        ServerMessage::Error(RejectReason::StorageUnavailable)
    )));
    let directory = h.directory.0.clone();
    let config = h.config.clone();
    drop(h.server);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(previous.len())
        .unwrap();
    let recovered = Server::open(config).unwrap();
    assert_eq!(recovered.world().read(target), CellRead::Known(Cell::AIR));
    drop(recovered);
    assert!(directory.exists());
}

#[test]
fn current_los_revokes_a_captured_witness_even_when_its_raw_view_is_unchanged() {
    let mut h = H::with_side_wall(false, false, true);
    let target = position(11);
    h.raw(target.page());
    let view = h.server.peers[&2]
        .history
        .confirmed_version(target.page())
        .unwrap();
    let id = h.place_start(target);
    let deadline = Instant::now() + Duration::from_secs(20);
    while h.server.world().read(target) != CellRead::Known(Cell::from_parts(rules::ids::GLASS, 0)) {
        assert!(Instant::now() < deadline);
        h.step();
    }
    assert!(h.server.pending.is_some());
    // Direct guard test: the real foreground serializes queued movement behind
    // pending WAL. Exercise the stronger receiver contract independently of
    // that serialization, without claiming concurrent motion was observed.
    let observer = h.server.peers.get_mut(&2).unwrap();
    observer.player.teleport([12.5, 129., 8.5]).unwrap();
    observer.raw_view.invalidate_observer();
    assert_eq!(
        observer.history.confirmed_version(target.page()),
        Some(view)
    );
    h.accept(id);
    assert_eq!(h.cues(id).iter().map(|c| c.0).collect::<Vec<_>>(), [1]);
}

#[test]
fn shared_owner_public_delivery_credit_and_private_record_validation_are_fail_closed() {
    let mut h = H::new(false, false);
    let target = position(11);
    h.raw(target.page());
    let id = h.place_start(target);
    let deadline = Instant::now() + Duration::from_secs(20);
    while h.server.world().read(target) != CellRead::Known(Cell::from_parts(rules::ids::GLASS, 0)) {
        assert!(Instant::now() < deadline);
        h.step();
    }
    let changes = h.server.pending.as_ref().unwrap().changes.clone();
    h.accept(id);
    let (change, event) = changes
        .iter()
        .find_map(|change| {
            change
                .events
                .iter()
                .find(|e| e.kind == EVENT)
                .map(|event| (change, event))
        })
        .unwrap();
    let (_, cue, _) = decode_event(change, event).unwrap();
    assert!(matches!(cue.kind,Kind::BlockPlace(a)if a.material==BlockMaterial::Glass));
    assert_eq!(
        family(Cell::from_parts(rules::ids::IRON_ORE, 0)),
        family(Cell::from_parts(rules::ids::STONE, 0)),
        "wire family never distinguishes hidden ore IDs"
    );
    for mut invalid in [
        event.clone(),
        event.clone(),
        event.clone(),
        event.clone(),
        event.clone(),
    ]
    .into_iter()
    .enumerate()
    {
        match invalid.0 {
            0 => invalid.1.payload[0] = 33,
            1 => invalid.1.payload.push(0),
            2 => invalid.1.payload[1 + CUE_BYTES + 16..1 + CUE_BYTES + 24].fill(0),
            3 => invalid.1.payload[1 + CUE_BYTES + 24] = 1,
            _ => invalid.1.position = Some(BlockPos { x: 9, ..target }),
        }
        assert!(
            decode_event(change, &invalid.1).is_none(),
            "malformed private record {}",
            invalid.0
        );
    }
    let mut wrong_family = event.clone();
    let mut block = cue.kind.block().unwrap();
    block.material = BlockMaterial::Wood;
    wrong_family.payload[1..1 + CUE_BYTES].copy_from_slice(
        &Cue {
            kind: Kind::BlockPlace(block),
            ..cue
        }
        .encode()
        .unwrap(),
    );
    assert!(
        decode_event(change, &wrong_family).is_none(),
        "event family must match the actual committed cell change"
    );
    let mut output = (0..8)
        .map(|_| Outgoing::reliable(1, ServerMessage::AudioCue(cue)))
        .collect::<Vec<_>>();
    let clipped = h.server.counters().audio_cues_clipped;
    h.server.publish_block_audio(&changes, &mut output);
    assert_eq!(output.iter().filter(|o| o.connection == 1).count(), 8);
    assert_eq!(
        output.iter().filter(|o| o.connection == 2).count(),
        1,
        "unsaturated listener is a positive publication control"
    );
    assert!(h.server.counters().audio_cues_clipped > clipped);
    assert!(
        output
            .iter()
            .all(|o| net::encode_server(&o.message).unwrap().len() == 18 + 1 + 4 + CUE_BYTES)
    );
}

#[test]
fn empty_audio_publication_performs_no_allocation_world_query_or_event_work() {
    let mut h = H::new(false, false);
    let before = h.server.counters();
    let scope = telemetry::AllocationScope::begin().unwrap();
    for _ in 0..1000 {
        assert!(h.server.publish_audio_cues(&[]).is_empty());
    }
    let counts = scope.finish();
    assert_eq!(counts.allocating_calls(), 0);
    assert_eq!(counts.dealloc_calls, 0);
    let after = h.server.counters();
    assert_eq!(after.audio_cues_sent, before.audio_cues_sent);
    assert_eq!(after.audio_cues_clipped, before.audio_cues_clipped);
    assert_eq!(after.block_audio_rays, before.block_audio_rays);
    assert_eq!(after.block_audio_cell_reads, before.block_audio_cell_reads);
    let scope = telemetry::AllocationScope::begin().unwrap();
    let control = std::hint::black_box(Vec::<u8>::with_capacity(128));
    std::hint::black_box(control.capacity());
    drop(control);
    assert!(
        scope.finish().allocating_calls() > 0,
        "allocation oracle must be live"
    );
}
