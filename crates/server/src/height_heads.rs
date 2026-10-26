//! Session-lifetime public Height versions. Private HID inputs certify workers;
//! public versions identify the complete published representation, including its
//! exact Geometry manifest. These numeric domains are never compared.
use net::height::{HeightKey, HeightTile, Replacement};
#[path = "height_heads/witness.rs"]
mod witness;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
static NEXT_SOURCE: AtomicU64 = AtomicU64::new(1);
type Key = (u64, HeightKey);
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_connections: usize,
    pub max_tiles: usize,
    pub cache_tiles: usize,
    pub cache_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_connections: 64,
            max_tiles: net::height::MAX_TILES,
            cache_tiles: 8192,
            cache_bytes: 32 * 1024 * 1024,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Bounds,
    Capacity,
    Exhausted,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub heads: usize,
    pub cached_tiles: usize,
    pub cached_bytes: usize,
    pub published: u64,
    pub reused: u64,
    pub witness_bytes: usize,
    pub public_witness_bytes: u64,
}
struct Head {
    private_source: u64,
    source: u64,
    witness: [u8; 32],
}
struct Cached {
    tile: Arc<HeightTile>,
    used: u64,
    bytes: usize,
}
pub struct HeightHeads {
    limits: Limits,
    peers: BTreeMap<u64, BTreeMap<HeightKey, Head>>,
    cache: BTreeMap<Key, Cached>,
    lru: BTreeSet<(u64, Key)>,
    bytes: usize,
    clock: u64,
    stats: Stats,
}
impl HeightHeads {
    pub fn new(limits: Limits) -> Result<Self, Error> {
        if limits.max_connections == 0
            || limits.max_connections > 64
            || limits.max_tiles == 0
            || limits.max_tiles > net::height::MAX_TILES
            || limits.cache_tiles == 0
            || limits.cache_bytes < 8192
        {
            return Err(Error::Bounds);
        }
        Ok(Self {
            limits,
            peers: BTreeMap::new(),
            cache: BTreeMap::new(),
            lru: BTreeSet::new(),
            bytes: 0,
            clock: 0,
            stats: Stats::default(),
        })
    }
    pub fn private_source(&self, connection: u64, key: HeightKey) -> Option<u64> {
        Some(self.peers.get(&connection)?.get(&key)?.private_source)
    }
    pub fn public_source(&self, connection: u64, key: HeightKey) -> Option<u64> {
        Some(self.peers.get(&connection)?.get(&key)?.source)
    }
    /// Reuse depends only on the complete public representation. Private HID
    /// changes remain worker certificates and cannot affect the public identity
    /// even after its body leaves the bounded LRU.
    pub fn publish(
        &mut self,
        connection: u64,
        private_source: u64,
        tile: &mut HeightTile,
    ) -> Result<u64, Error> {
        tile.validate().map_err(|_| Error::Bounds)?;
        if private_source == 0 {
            return Err(Error::Bounds);
        }
        if !self.peers.contains_key(&connection) && self.peers.len() >= self.limits.max_connections
        {
            return Err(Error::Capacity);
        }
        let old = self.peers.get(&connection).and_then(|p| p.get(&tile.key));
        if old.is_none()
            && self
                .peers
                .get(&connection)
                .is_some_and(|p| p.len() >= self.limits.max_tiles)
        {
            return Err(Error::Capacity);
        }
        let key = (connection, tile.key);
        let old_source = old.map(|h| h.source);
        let (witness, witness_bytes) = witness::body(tile);
        self.stats.public_witness_bytes += witness_bytes;
        let reuse = if let Some(cached) = self.cache.get(&key) {
            let requested = tile.source;
            tile.source = cached.tile.source;
            let equal = *cached.tile == *tile;
            tile.source = requested;
            equal
        } else {
            old.is_some_and(|old| old.witness == witness)
        };
        let source = if reuse {
            old_source.ok_or(Error::Bounds)?
        } else {
            NEXT_SOURCE
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                    value.checked_add(1)
                })
                .map_err(|_| Error::Exhausted)?
        };
        let next_clock = self.clock.checked_add(1).ok_or(Error::Exhausted)?;
        tile.source = source;
        let body = Arc::new(tile.clone());
        let bytes = std::mem::size_of::<HeightTile>()
            + match &body.replacement {
                Replacement::Column { geometry, .. } => {
                    geometry.capacity() * std::mem::size_of::<net::height::GeometryStamp>()
                }
                _ => 0,
            };
        if bytes > self.limits.cache_bytes {
            return Err(Error::Capacity);
        }
        self.peers.entry(connection).or_default().insert(
            tile.key,
            Head {
                private_source,
                source,
                witness,
            },
        );
        if let Some(old) = self.cache.remove(&key) {
            self.lru.remove(&(old.used, key));
            self.bytes -= old.bytes;
        }
        while self.cache.len() >= self.limits.cache_tiles
            || self.bytes + bytes > self.limits.cache_bytes
        {
            let Some((_, victim)) = self.lru.pop_first() else {
                return Err(Error::Capacity);
            };
            let old = self.cache.remove(&victim).expect("LRU member");
            self.bytes -= old.bytes;
        }
        self.clock = next_clock;
        self.lru.insert((next_clock, key));
        self.cache.insert(
            key,
            Cached {
                tile: body,
                used: next_clock,
                bytes,
            },
        );
        self.bytes += bytes;
        self.stats.published += 1;
        self.stats.reused += u64::from(reuse);
        Ok(source)
    }
    pub fn forget(&mut self, connection: u64, key: HeightKey) {
        if let Some(peer) = self.peers.get_mut(&connection) {
            peer.remove(&key);
            if peer.is_empty() {
                self.peers.remove(&connection);
            }
        }
        if let Some(old) = self.cache.remove(&(connection, key)) {
            self.lru.remove(&(old.used, (connection, key)));
            self.bytes -= old.bytes;
        }
    }
    pub fn forget_peer(&mut self, connection: u64) {
        let Some(peer) = self.peers.remove(&connection) else {
            return;
        };
        for key in peer.into_keys() {
            if let Some(old) = self.cache.remove(&(connection, key)) {
                self.lru.remove(&(old.used, (connection, key)));
                self.bytes -= old.bytes;
            }
        }
    }
    pub fn stats(&self) -> Stats {
        let heads: usize = self.peers.values().map(BTreeMap::len).sum();
        Stats {
            heads,
            witness_bytes: heads * 32,
            cached_tiles: self.cache.len(),
            cached_bytes: self.bytes,
            ..self.stats
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn tile(x: i32) -> HeightTile {
        let mut known = [255; 37];
        known[36] = 1;
        HeightTile {
            key: HeightKey {
                space: 0,
                level: 0,
                x,
                z: 0,
            },
            source: 1,
            error: None,
            heights: [8; 289],
            known,
            atlas: [3; 256],
            tint: [u32::MAX; 256],
            protected: [0; 32],
            replacement: Replacement::None,
        }
    }
    fn column(tile: &mut HeightTile, view: u64) {
        tile.protected = [255; 32];
        tile.replacement = Replacement::Column {
            min_y: 0,
            max_y: 7,
            geometry: (0..8)
                .map(|y| net::height::GeometryStamp {
                    page: core_types::PageId {
                        space: core_types::SpaceId(0),
                        x: tile.key.x,
                        y,
                        z: tile.key.z,
                    },
                    source: core_types::Version(view),
                    view,
                })
                .collect(),
        };
    }
    #[test]
    fn unchanged_private_hid_neighbor_manifest_gets_new_public_source() {
        let mut heads = HeightHeads::new(Limits::default()).unwrap();
        let mut image = tile(0);
        column(&mut image, 10);
        let a = heads.publish(1, 1, &mut image).unwrap();
        assert_eq!(heads.publish(1, 1, &mut image).unwrap(), a);
        column(&mut image, 11);
        let b = heads.publish(1, 1, &mut image).unwrap();
        assert!(b > a);
        assert_eq!(heads.private_source(1, image.key), Some(1));
        assert_eq!(heads.public_source(1, image.key), Some(b));
        assert_eq!(
            heads.publish(1, 2, &mut image).unwrap(),
            b,
            "hidden-only HID input change must preserve an equal cached public image"
        );
    }
    #[test]
    fn lru_eviction_uses_full_representation_and_forget_never_reuses_old_identity() {
        let mut heads = HeightHeads::new(Limits {
            cache_tiles: 1,
            ..Default::default()
        })
        .unwrap();
        let mut a = tile(0);
        column(&mut a, 3);
        let first = heads.publish(1, 1, &mut a).unwrap();
        heads.publish(1, 1, &mut tile(1)).unwrap();
        assert_eq!(
            heads.publish(1, 99, &mut a).unwrap(),
            first,
            "hidden-only private HID changes cannot change an evicted public body identity"
        );
        heads.publish(1, 1, &mut tile(1)).unwrap();
        a.heights[17] += 1;
        let surface_changed = heads.publish(1, 99, &mut a).unwrap();
        assert!(
            surface_changed > first,
            "an evicted public surface change must advance its identity even with the same private source"
        );
        heads.publish(1, 1, &mut tile(1)).unwrap();
        column(&mut a, 4);
        let changed = heads.publish(1, 99, &mut a).unwrap();
        assert!(changed > surface_changed);
        heads.forget(1, a.key);
        assert!(heads.publish(1, 1, &mut a).unwrap() > changed);
        heads.forget_peer(1);
        assert_eq!(heads.stats().heads, 0);
        assert_eq!(heads.stats().cached_bytes, 0);
        assert_eq!(heads.stats().witness_bytes, 0);
    }
    #[test]
    fn cached_body_equality_precedes_producer_signature_and_limits_do_not_mutate_heads() {
        let mut heads = HeightHeads::new(Limits {
            max_connections: 1,
            max_tiles: 1,
            ..Default::default()
        })
        .unwrap();
        let mut a = tile(0);
        let first = heads.publish(1, 1, &mut a).unwrap();
        a.heights[17] = 9;
        assert!(heads.publish(1, 1, &mut a).unwrap() > first);
        let before = heads.stats();
        assert_eq!(heads.publish(2, 1, &mut tile(0)), Err(Error::Capacity));
        assert_eq!(heads.publish(1, 1, &mut tile(1)), Err(Error::Capacity));
        assert_eq!(heads.stats(), before);
        let mut malformed = tile(0);
        malformed.known[36] = 255;
        assert_eq!(heads.publish(1, 1, &mut malformed), Err(Error::Bounds));
        assert_eq!(heads.stats(), before);
    }
}
