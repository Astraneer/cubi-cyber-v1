//! Bounded public-baseline workers. Edited leaves require real geometry manifests.
use super::*;
use height_index::{TileEdits, TileKey};
use height_tile::{PublicHeightTile, ReplacementRequirement};
use net::height::{GeometryStamp, HeightKey, HeightTile, Replacement};
type Pool = jobs::NativePool<TileEdits, PublicHeightTile>;
pub(super) const MAX_TILES: usize = 512;
pub(super) const MAX_GEOMETRY: usize = 128;
pub(super) struct HeightWorker {
    pool: Pool,
    inflight: BTreeMap<jobs::JobId, (u64, HeightKey, u64)>,
    ready: VecDeque<(u64, PublicHeightTile)>,
    next_job: u64,
    next_peer: u64,
}
fn authority_key(key: HeightKey) -> TileKey {
    TileKey {
        space: SpaceId(key.space),
        level: key.level,
        x: key.x,
        z: key.z,
    }
}
fn wire_key(key: TileKey) -> HeightKey {
    HeightKey {
        space: key.space.0,
        level: key.level,
        x: key.x,
        z: key.z,
    }
}
impl HeightWorker {
    pub(super) fn new(terrain: r#gen::Generator) -> Result<Self, ServerError> {
        let pool = Pool::new(2, 8, move |edits, cancel: &jobs::CancellationToken| {
            if cancel.is_cancelled() {
                return Err("cancelled public height".into());
            }
            let (tile, _) = height_tile::build_canonical_tile(
                &terrain,
                &edits,
                height_tile::HeightBuildLimits::default(),
            )
            .map_err(|_| "height build bounds".to_owned())?;
            if cancel.is_cancelled() {
                return Err("cancelled public height".into());
            }
            Ok(tile)
        })
        .map_err(|_| ServerError::Invalid("public height worker"))?;
        Ok(Self {
            pool,
            inflight: BTreeMap::new(),
            ready: VecDeque::new(),
            next_job: 1,
            next_peer: 0,
        })
    }
}
impl Server {
    fn height_allowed(&self, peer: &Peer, key: HeightKey) -> bool {
        if key.space != 0 || peer.space != SpaceId(0) || key.validate().is_err() {
            return false;
        }
        let Some([x, z, x1, z1]) = authority_key(key).bounds() else {
            return false;
        };
        let dx = (peer.player.pos[0] - (peer.player.pos[0].clamp(x as f64, x1 as f64))).abs();
        let dz = (peer.player.pos[2] - (peer.player.pos[2].clamp(z as f64, z1 as f64))).abs();
        dx <= 16384. && dz <= 16384.
    }
    pub(super) fn refresh_heights(&mut self) {
        for peer in self.peers.values_mut().filter(|p| p.ready) {
            for (key, published) in &peer.height_subscriptions {
                if self
                    .runtimes
                    .height
                    .index()
                    .source(authority_key(*key))
                    .is_ok_and(|s| s != *published)
                    && peer.height_requests.len() < MAX_TILES
                    && !peer.height_requests.iter().any(|(k, _)| k == key)
                {
                    peer.height_requests.push_back((*key, None));
                }
            }
        }
    }
    pub(super) fn publish_heights(&mut self) -> Vec<Outgoing> {
        let mut output = Vec::new();
        if !self.height_ready() {
            return output;
        }
        if self.height_worker.inflight.is_empty()
            && self.height_worker.ready.is_empty()
            && !self
                .peers
                .values()
                .any(|p| p.ready && !p.height_requests.is_empty())
        {
            return output;
        }
        for _ in 0..2 {
            if self.height_worker.ready.len() >= 8 {
                break;
            }
            let Some(done) = self.height_worker.pool.try_recv() else {
                break;
            };
            let Some((connection, key, source)) = self.height_worker.inflight.remove(&done.id)
            else {
                continue;
            };
            match done.outcome {
                jobs::JobOutcome::Success(tile)
                    if wire_key(tile.key) == key && tile.source == source =>
                {
                    self.height_worker.ready.push_back((connection, tile))
                }
                _ => output.push(Outgoing::reliable(
                    connection,
                    ServerMessage::HeightUnavailable(key),
                )),
            }
        }
        let attempts = self.height_worker.ready.len().min(8);
        let mut published = 0;
        for _ in 0..attempts {
            if published >= 2 {
                break;
            }
            let Some((connection, mut tile)) = self.height_worker.ready.pop_front() else {
                break;
            };
            let key = wire_key(tile.key);
            let Some(peer) = self
                .peers
                .get(&connection)
                .filter(|p| p.ready && p.height_subscriptions.contains_key(&key))
            else {
                continue;
            };
            if !self.height_allowed(peer, key) {
                continue;
            }
            if self.runtimes.height.index().source(tile.key) != Ok(tile.source) {
                let peer = self.peers.get_mut(&connection).expect("present peer");
                if !peer.height_requests.iter().any(|(k, _)| *k == key) {
                    peer.height_requests.push_back((key, None));
                }
                self.counters.height_discarded += 1;
                continue;
            }
            if peer.height_exact.contains(&key)
                && crate::height_tile::promote_exact_leaf(&mut tile).is_err()
            {
                output.push(Outgoing::reliable(
                    connection,
                    ServerMessage::HeightUnavailable(key),
                ));
                continue;
            }
            let replacement = match &tile.replacement {
                ReplacementRequirement::None => Replacement::None,
                ReplacementRequirement::Refine { .. } => Replacement::Refine,
                ReplacementRequirement::Column {
                    min_page_y,
                    max_page_y,
                    ..
                } => {
                    let mut geometry = Vec::with_capacity((*max_page_y - *min_page_y + 1) as usize);
                    let mut missing = Vec::new();
                    for y in *min_page_y..=*max_page_y {
                        let page = PageId {
                            space: tile.key.space,
                            x: tile.key.x,
                            y,
                            z: tile.key.z,
                        };
                        let stamp = self.world.page(page).and_then(|p| {
                            let source = geometry::source_version(p);
                            (peer.geometry_subscriptions.get(&page) == Some(&source))
                                .then(|| peer.geometry_views.get(&page).copied())
                                .flatten()
                                .map(|view| GeometryStamp { page, source, view })
                        });
                        if let Some(stamp) = stamp {
                            geometry.push(stamp);
                        } else {
                            missing.push(page);
                        }
                    }
                    if !missing.is_empty() {
                        let unknown_unloadable = missing.iter().any(|page| {
                            self.world.page(*page).is_none()
                                && (self.generation.is_none() || page.y < 0 || page.y >= 8)
                        });
                        let new_geometry = missing
                            .iter()
                            .filter(|p| !peer.geometry_subscriptions.contains_key(p))
                            .count();
                        if unknown_unloadable
                            || peer.geometry_subscriptions.len() + new_geometry > MAX_GEOMETRY
                        {
                            output.push(Outgoing::reliable(
                                connection,
                                ServerMessage::HeightUnavailable(key),
                            ));
                            continue;
                        }
                        let peer = self.peers.get_mut(&connection).expect("present peer");
                        for page in &missing {
                            if peer.geometry_subscriptions.len() < MAX_GEOMETRY
                                || peer.geometry_subscriptions.contains_key(page)
                            {
                                peer.geometry_subscriptions
                                    .entry(*page)
                                    .or_insert(Version(0));
                                if peer.geometry_requests.len() < MAX_GEOMETRY
                                    && !peer.geometry_requests.iter().any(|(p, _)| p == page)
                                {
                                    peer.geometry_requests.push_back((*page, None));
                                }
                            }
                        }
                        for page in missing {
                            if self.world.page(page).is_none() {
                                self.request_generation(page);
                            }
                        }
                        self.height_worker.ready.push_back((connection, tile));
                        continue;
                    }
                    Replacement::Column {
                        min_y: *min_page_y,
                        max_y: *max_page_y,
                        geometry,
                    }
                }
            };
            let wire = HeightTile {
                key,
                source: tile.source,
                error: tile.error,
                heights: tile.heights,
                known: tile.known,
                atlas: tile.atlas,
                tint: tile.tint,
                protected: tile.protected,
                replacement,
            };
            if wire.validate().is_err() {
                self.failed = true;
                break;
            }
            self.peers
                .get_mut(&connection)
                .expect("present peer")
                .height_subscriptions
                .insert(key, tile.source);
            output.push(Outgoing::reliable(
                connection,
                ServerMessage::Height(Box::new(wire)),
            ));
            self.counters.height_published += 1;
            published += 1;
        }
        let mut peers: Vec<_> = self.peers.keys().copied().collect();
        peers.sort_by_key(|id| (*id <= self.height_worker.next_peer, *id));
        let mut submitted = 0;
        for connection in peers {
            if submitted >= 2
                || self.height_worker.inflight.len() + self.height_worker.ready.len() >= 8
            {
                break;
            }
            let request = self
                .peers
                .get_mut(&connection)
                .filter(|p| p.ready)
                .and_then(|p| p.height_requests.pop_front());
            let Some((key, known)) = request else {
                continue;
            };
            self.height_worker.next_peer = connection;
            let Some(peer) = self.peers.get(&connection) else {
                continue;
            };
            if !self.height_allowed(peer, key) {
                output.push(Outgoing::reliable(
                    connection,
                    ServerMessage::HeightUnavailable(key),
                ));
                continue;
            }
            let akey = authority_key(key);
            let Ok(source) = self.runtimes.height.index().source(akey) else {
                continue;
            };
            if known == Some(source)
                || self
                    .height_worker
                    .inflight
                    .values()
                    .any(|(c, k, s)| *c == connection && *k == key && *s == source)
                || self
                    .height_worker
                    .ready
                    .iter()
                    .any(|(c, t)| *c == connection && t.key == akey && t.source == source)
            {
                continue;
            }
            let Ok(edits) = self.runtimes.height.index().capture(akey, 4096, 65536) else {
                output.push(Outgoing::reliable(
                    connection,
                    ServerMessage::HeightUnavailable(key),
                ));
                continue;
            };
            let id = jobs::JobId(self.height_worker.next_job);
            self.height_worker.next_job = self.height_worker.next_job.saturating_add(1);
            let spec = jobs::JobSpec::derived(
                jobs::JobKey {
                    page: PageId {
                        space: akey.space,
                        x: akey.x,
                        y: 0,
                        z: akey.z,
                    },
                    kind: 70 + u16::from(akey.level),
                },
                jobs::Priority::Background,
                jobs::WorkCost {
                    cells: edits.visited_cells as u64 + 4385,
                    quads: 0,
                    bytes: 2 * 1024 * 1024,
                    rays: 0,
                },
                2 * 1024 * 1024,
                self.tick,
            );
            match self.height_worker.pool.try_submit(jobs::Dispatch {
                id,
                spec,
                cancellation: jobs::CancellationToken::new(),
                payload: edits,
            }) {
                Ok(()) => {
                    self.height_worker
                        .inflight
                        .insert(id, (connection, key, source));
                    self.counters.height_submitted += 1;
                    submitted += 1;
                }
                Err(_) => {
                    self.peers
                        .get_mut(&connection)
                        .expect("present peer")
                        .height_requests
                        .push_front((key, known));
                    break;
                }
            }
        }
        output
    }
}
