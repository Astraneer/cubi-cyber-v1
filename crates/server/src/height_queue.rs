//! Bounded FIFO with an independent pending-key index. Membership/coalescing
//! never scans the FIFO; explicit Forget visits only this bounded queue.
use net::height::HeightKey;
use std::{
    cell::Cell,
    collections::{HashMap, VecDeque},
};
pub const MAXIMUM: usize = net::height::MAX_TILES;
type IndexKey = (u32, u8, i32, i32);
fn index_key(key: HeightKey) -> IndexKey {
    (key.space, key.level, key.x, key.z)
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Work {
    /// Actual HashMap membership/entry/get/remove calls, not claimed hash probes.
    pub index_lookups: u64,
    /// Actual FIFO entries visited by explicit removal, separate from lookups.
    pub fifo_visits: u64,
    pub pushes: u64,
    pub pops: u64,
    pub updates: u64,
    pub removals: u64,
}
impl std::ops::AddAssign for Work {
    fn add_assign(&mut self, rhs: Self) {
        self.index_lookups += rhs.index_lookups;
        self.fifo_visits += rhs.fifo_visits;
        self.pushes += rhs.pushes;
        self.pops += rhs.pops;
        self.updates += rhs.updates;
        self.removals += rhs.removals;
    }
}
pub(super) struct HeightQueue {
    order: VecDeque<HeightKey>,
    known: HashMap<IndexKey, Option<u64>>,
    work: Cell<Work>,
}
impl HeightQueue {
    pub fn new() -> Self {
        Self {
            order: VecDeque::with_capacity(MAXIMUM),
            known: HashMap::with_capacity(MAXIMUM),
            work: Cell::new(Work::default()),
        }
    }
    pub fn len(&self) -> usize {
        self.known.len()
    }
    pub fn is_empty(&self) -> bool {
        self.known.is_empty()
    }
    pub fn work(&self) -> Work {
        self.work.get()
    }
    fn record(&self, update: impl FnOnce(&mut Work)) {
        let mut work = self.work.get();
        update(&mut work);
        self.work.set(work);
    }
    pub fn contains_key(&self, key: &HeightKey) -> bool {
        self.record(|w| w.index_lookups += 1);
        self.known.contains_key(&index_key(*key))
    }
    pub fn force_full(&mut self, key: &HeightKey) -> bool {
        self.record(|w| w.index_lookups += 1);
        let Some(known) = self.known.get_mut(&index_key(*key)) else {
            return false;
        };
        *known = None;
        self.record(|w| w.updates += 1);
        true
    }
    pub fn push_back(&mut self, request: (HeightKey, Option<u64>)) -> bool {
        self.push(request, false)
    }
    pub fn push_front(&mut self, request: (HeightKey, Option<u64>)) -> bool {
        self.push(request, true)
    }
    fn push(&mut self, (key, known): (HeightKey, Option<u64>), front: bool) -> bool {
        if self.known.len() >= MAXIMUM {
            return false;
        }
        self.record(|w| w.index_lookups += 1);
        let std::collections::hash_map::Entry::Vacant(entry) = self.known.entry(index_key(key))
        else {
            return false;
        };
        entry.insert(known);
        if front {
            self.order.push_front(key);
        } else {
            self.order.push_back(key);
        }
        self.record(|w| w.pushes += 1);
        true
    }
    pub fn pop_front(&mut self) -> Option<(HeightKey, Option<u64>)> {
        let key = self.order.pop_front()?;
        self.record(|w| {
            w.index_lookups += 1;
            w.pops += 1;
        });
        let known = self
            .known
            .remove(&index_key(key))
            .expect("FIFO/index contain the same pending keys");
        Some((key, known))
    }
    pub fn remove(&mut self, key: &HeightKey) -> bool {
        self.record(|w| w.index_lookups += 1);
        if self.known.remove(&index_key(*key)).is_none() {
            return false;
        }
        let mut visits = 0;
        self.order.retain(|pending| {
            visits += 1;
            pending != key
        });
        self.record(|w| {
            w.fifo_visits += visits;
            w.removals += 1;
        });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_types::SpaceId;
    use net::{
        ClientMessage, ServerMessage,
        height::{HeightTile, Replacement},
    };
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
        time::{Duration, Instant},
    };
    static NEXT: AtomicU64 = AtomicU64::new(1);
    fn key(i: usize) -> HeightKey {
        HeightKey {
            space: 0,
            level: 0,
            x: i as i32 % 64 - 32,
            z: i as i32 / 64 - 32,
        }
    }
    #[test]
    fn fifo_coalescing_force_full_retry_forget_and_capacity_match_independent_deque() {
        let mut queue = HeightQueue::new();
        let mut oracle = VecDeque::new();
        for i in 0..MAXIMUM {
            assert!(queue.push_back((key(i), Some(i as u64))));
            oracle.push_back((key(i), Some(i as u64)));
        }
        assert_eq!(queue.len(), MAXIMUM);
        assert!(!queue.push_back((key(MAXIMUM), None)));
        assert!(!queue.push_back((key(3), None)));
        let mut rng = 57_u64;
        for _ in 0..20000 {
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
            let k = key((rng >> 16) as usize % (MAXIMUM + 32));
            match rng % 5 {
                0 => {
                    let old = oracle.iter_mut().find(|(key, _)| *key == k);
                    let expected = old.is_some();
                    if let Some((_, known)) = old {
                        *known = None;
                    }
                    assert_eq!(queue.force_full(&k), expected);
                }
                1 => {
                    let expected = oracle.iter().any(|(key, _)| *key == k);
                    oracle.retain(|(key, _)| *key != k);
                    assert_eq!(queue.remove(&k), expected);
                }
                2 => assert_eq!(queue.pop_front(), oracle.pop_front()),
                side => {
                    let expected =
                        oracle.len() < MAXIMUM && !oracle.iter().any(|(key, _)| *key == k);
                    let request = (k, Some(rng));
                    let actual = if side == 3 {
                        queue.push_back(request)
                    } else {
                        queue.push_front(request)
                    };
                    assert_eq!(actual, expected);
                    if expected {
                        if side == 3 {
                            oracle.push_back(request)
                        } else {
                            oracle.push_front(request)
                        }
                    }
                }
            }
            assert_eq!(queue.len(), oracle.len());
            assert_eq!(
                queue.contains_key(&k),
                oracle.iter().any(|(key, _)| *key == k)
            );
        }
        while let Some(request) = oracle.pop_front() {
            assert_eq!(queue.pop_front(), Some(request));
        }
        assert!(queue.is_empty());
        assert_eq!(queue.pop_front(), None);
        assert!(
            queue.work().fifo_visits > 0,
            "explicit Forget work is counted"
        );
    }
    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../.local/tmp")
                .join(format!(
                    "height-pending-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn actual_4096_subscriptions_refresh_uses_indexed_membership_and_idle_zero_allocations() {
        let directory = Directory::new();
        let mut config = crate::ServerConfig::new(&directory.0);
        config.generation_enabled = false;
        let mut server =
            crate::Server::open_with_world(config, world::World::new(rules::properties, 64))
                .unwrap();
        let ClientMessage::Hello(hello) =
            net::decode_client(&std::fs::read(directory.0.join("owner.identity")).unwrap())
                .unwrap()
        else {
            panic!("owner identity");
        };
        let mut welcomed = server
            .handle(1, ClientMessage::Hello(hello), 0)
            .iter()
            .any(|o| matches!(o.message, ServerMessage::Welcome(_)));
        let mut now = 0;
        let deadline = Instant::now() + Duration::from_secs(10);
        while !welcomed || server.awaiting_durability() {
            assert!(Instant::now() < deadline);
            now += 10;
            welcomed |= server
                .tick(now)
                .iter()
                .any(|o| matches!(o.message, ServerMessage::Welcome(_)));
            std::thread::sleep(Duration::from_millis(1));
        }
        // One genuinely published private-source head means the other4095
        // pending keys stay below the4096 cap. The old per-key FIFO scan was
        // quadratic precisely in this legal not-quite-full state.
        let mut known = [255; 37];
        known[36] = 1;
        let published_key = key(MAXIMUM - 1);
        let mut tile = HeightTile {
            key: published_key,
            source: 1,
            error: None,
            heights: [1; 289],
            known,
            atlas: [rules::ids::L_STONE as u16; 256],
            tint: [u32::MAX; 256],
            protected: [0; 32],
            replacement: Replacement::None,
        };
        let source = server.height_heads.publish(1, 1, &mut tile).unwrap();
        let peer = server.peers.get_mut(&1).unwrap();
        assert!(peer.ready);
        for i in 0..MAXIMUM {
            peer.height_subscriptions
                .insert(key(i), if i == MAXIMUM - 1 { source } else { 0 });
        }
        server.refresh_heights();
        assert_eq!(server.peers[&1].height_subscriptions.len(), MAXIMUM);
        assert_eq!(server.peers[&1].height_requests.len(), MAXIMUM - 1);
        let before = server.pending_height_queue_work();
        let scope = telemetry::AllocationScope::begin().unwrap();
        for _ in 0..100 {
            server.refresh_heights();
        }
        let allocations = scope.finish();
        let after = server.pending_height_queue_work();
        assert_eq!(
            after.index_lookups - before.index_lookups,
            100 * (MAXIMUM as u64 - 1)
        );
        assert_eq!(after.fifo_visits - before.fifo_visits, 0);
        assert_eq!(after.pushes, before.pushes);
        assert_eq!(allocations.allocating_calls(), 0);
        assert_eq!(allocations.dealloc_calls, 0);
        let oracle: VecDeque<_> = server.peers[&1]
            .height_subscriptions
            .keys()
            .filter(|key| **key != published_key)
            .map(|key| (*key, None))
            .collect();
        let mut oracle_visits = 0;
        for i in 0..MAXIMUM - 1 {
            assert!(oracle.iter().any(|(k, _)| {
                oracle_visits += 1;
                *k == key(i)
            }));
        }
        assert_eq!(oracle_visits, (MAXIMUM - 1) * MAXIMUM / 2);
        let queue = &mut server.peers.get_mut(&1).unwrap().height_requests;
        for expected in oracle {
            assert_eq!(queue.pop_front(), Some(expected));
        }
        assert!(queue.is_empty());
        assert_eq!(server.world().pages().count(), 0);
        assert_eq!(
            server.world().read(core_types::BlockPos {
                space: SpaceId(0),
                x: 0,
                y: 0,
                z: 0
            }),
            world::CellRead::Unknown
        );
        assert_eq!(server.counters().generation_submitted, 0);
        println!(
            "height_pending subscriptions4096 pending4095 oneRefreshIndex4095 fifoVisits0 independentOldOracleVisits{oracle_visits} idle100=0alloc/free"
        );
    }
}
