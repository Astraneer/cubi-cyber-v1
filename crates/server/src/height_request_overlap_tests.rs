//! Deterministic async overlap: encoded client intent survives a conditional
//! worker receipt already owned by the actual Server. Completion is delivered
//! explicitly, rather than trying to win a thread timing race.
use super::*;
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

static NEXT: AtomicU64 = AtomicU64::new(1);
struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Harness {
    server: Server,
    now: u64,
    _directory: Directory,
}
fn key(x: i32) -> HeightKey {
    HeightKey {
        space: 0,
        level: 0,
        x,
        z: 0,
    }
}
impl Harness {
    fn new() -> Self {
        let directory = Directory(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../.local/tmp")
                .join(format!(
                    "height-overlap-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                )),
        );
        std::fs::create_dir_all(&directory.0).unwrap();
        let mut config = ServerConfig::new(&directory.0);
        config.generation_enabled = false;
        config.spawn_override = Some([0.5, 200., 0.5]);
        let mut server =
            Server::open_with_world(config, World::new(rules::properties, 64)).unwrap();
        let message =
            net::decode_client(&std::fs::read(directory.0.join("owner.identity")).unwrap())
                .unwrap();
        let mut welcomed = server
            .handle(1, message, 0)
            .iter()
            .any(|o| matches!(o.message, ServerMessage::Welcome(_)));
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut now = 0;
        while !welcomed || server.awaiting_durability() || !server.height_ready() {
            assert!(
                Instant::now() < deadline,
                "height overlap fixture bootstrap"
            );
            now += 10;
            welcomed |= server
                .tick(now)
                .iter()
                .any(|o| matches!(o.message, ServerMessage::Welcome(_)));
            std::thread::sleep(Duration::from_millis(1));
        }
        Self {
            server,
            now,
            _directory: directory,
        }
    }
    fn send(&mut self, message: ClientMessage) {
        let bytes = net::encode_client(&message).unwrap();
        let message = net::decode_client(&bytes).unwrap();
        assert!(self.server.inbox.is_empty());
        assert!(self.server.enqueue(1, message, self.now).is_empty());
        // Drive the actual authority admission separately from worker polling,
        // so the overlap is causal and never depends on winning a thread race.
        let (connection, message, received_at) = self.server.inbox.pop_front().unwrap();
        let mut batch = Batch::empty(self.server.tick);
        let mut output = Vec::new();
        self.server
            .process_message(connection, message, received_at, &mut batch, &mut output);
        assert!(
            output
                .iter()
                .all(|o| !matches!(o.message, ServerMessage::Error(_)))
        );
        assert!(batch.changes.is_empty());
    }
    fn tile(&self, key: HeightKey) -> PublicHeightTile {
        let mut known = [255; 37];
        known[36] = 1;
        PublicHeightTile {
            key: authority_key(key),
            source: self
                .server
                .runtimes
                .height
                .index()
                .source(authority_key(key))
                .unwrap(),
            error: None,
            heights: [64; 289],
            known,
            atlas: [rules::ids::L_STONE as u16; 256],
            tint: [u32::MAX; 256],
            protected: [0; 32],
            replacement: ReplacementRequirement::None,
        }
    }
    fn baseline(&mut self, key: HeightKey) -> (PublicHeightTile, Vec<u8>, u64) {
        let tile = self.tile(key);
        self.server
            .peers
            .get_mut(&1)
            .unwrap()
            .height_subscriptions
            .insert(key, 0);
        self.server
            .height_worker
            .ready
            .push_back((1, tile.clone(), None));
        let published = self.server.publish_heights();
        let [out] = published.as_slice() else {
            panic!("one baseline Height");
        };
        let ServerMessage::Height(wire) = &out.message else {
            panic!("baseline Height");
        };
        (tile, net::encode_server(&out.message).unwrap(), wire.source)
    }
    fn expect_full(&mut self, expected: &[u8]) {
        let output = self.server.publish_heights();
        let [out] = output.as_slice() else {
            panic!("one explicit full Height, got {}", output.len());
        };
        assert_eq!(net::encode_server(&out.message).unwrap(), expected);
        assert_eq!(
            self.server.counters.height_submitted, 0,
            "coalescing must reuse the existing receipt"
        );
        assert!(self.server.height_worker.inflight.is_empty());
        assert!(self.server.height_worker.ready.is_empty());
    }
}

#[test]
fn encoded_full_height_and_exact_requests_merge_into_inflight_recert() {
    for exact in [false, true] {
        let mut h = Harness::new();
        let key = key(0);
        let (tile, bytes, public) = h.baseline(key);
        let id = jobs::JobId(u64::MAX);
        h.server
            .height_worker
            .inflight
            .insert(id, (1, key, tile.source, Some(public)));
        h.send(if exact {
            ClientMessage::RequestExactHeight { key, known: None }
        } else {
            ClientMessage::RequestHeight { key, known: None }
        });
        assert!(h.server.publish_heights().is_empty());
        let (_, _, _, known) = h.server.height_worker.inflight.remove(&id).unwrap();
        assert_eq!(
            known, None,
            "explicit full intent must survive the already-running conditional receipt"
        );
        h.server.height_worker.ready.push_back((1, tile, known));
        h.expect_full(&bytes);
    }
}

#[test]
fn encoded_full_request_merges_into_ready_recert_after_two_publications() {
    let mut h = Harness::new();
    let target = key(0);
    let (tile, bytes, public) = h.baseline(target);
    for x in [1, 2] {
        let key = key(x);
        let tile = h.tile(key);
        h.server
            .peers
            .get_mut(&1)
            .unwrap()
            .height_subscriptions
            .insert(key, 0);
        h.server.height_worker.ready.push_back((1, tile, None));
    }
    h.server
        .height_worker
        .ready
        .push_back((1, tile, Some(public)));
    h.send(ClientMessage::RequestHeight {
        key: target,
        known: None,
    });
    assert_eq!(
        h.server.publish_heights().len(),
        2,
        "unchanged two-publication budget"
    );
    assert_eq!(h.server.height_worker.ready.len(), 1);
    assert_eq!(
        h.server.height_worker.ready[0].2, None,
        "ready receipt must retain full intent"
    );
    h.expect_full(&bytes);
}

#[test]
fn encoded_full_request_overrides_queued_conditional_recert() {
    let mut h = Harness::new();
    let key = key(0);
    let (tile, bytes, public) = h.baseline(key);
    h.server
        .peers
        .get_mut(&1)
        .unwrap()
        .height_requests
        .push_back((key, Some(public)));
    h.send(ClientMessage::RequestHeight { key, known: None });
    let (requested, known) = h
        .server
        .peers
        .get_mut(&1)
        .unwrap()
        .height_requests
        .pop_front()
        .unwrap();
    assert_eq!(requested, key);
    assert_eq!(
        known, None,
        "ordinary full request must dominate queued conditional work"
    );
    h.server.height_worker.ready.push_back((1, tile, known));
    h.expect_full(&bytes);
}

#[test]
fn same_conditional_receipts_stay_silent_but_conflicting_known_values_require_full() {
    for conflict in [false, true] {
        let mut h = Harness::new();
        let key = key(0);
        let (tile, bytes, public) = h.baseline(key);
        let id = jobs::JobId(u64::MAX);
        h.server
            .height_worker
            .inflight
            .insert(id, (1, key, tile.source, Some(public)));
        let incoming = if conflict {
            public.checked_add(1).unwrap()
        } else {
            public
        };
        h.send(ClientMessage::RequestHeight {
            key,
            known: Some(incoming),
        });
        assert!(h.server.publish_heights().is_empty());
        let (_, _, _, known) = h.server.height_worker.inflight.remove(&id).unwrap();
        assert_eq!(known, if conflict { None } else { Some(public) });
        h.server.height_worker.ready.push_back((1, tile, known));
        if conflict {
            h.expect_full(&bytes);
        } else {
            assert!(
                h.server.publish_heights().is_empty(),
                "equal conditional input must not manufacture an unsolicited body"
            );
        }
    }
}
