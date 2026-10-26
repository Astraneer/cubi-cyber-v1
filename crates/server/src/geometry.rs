//! Bounded public geometry projection. Canonical far requests use immutable
//! private-authority workers without creating gameplay World pages.
use crate::{Outgoing, Server, cold_projection::ColumnKey, cold_projection_owner, lod};
use core_types::{PageId, Tick, Version};
use net::{PublicGeometry, PublicSurface, PublicSurfaceKind, ServerMessage};
use std::{collections::BTreeMap, sync::Arc};
use world::Page;
pub(super) fn channels() -> core_types::ChannelMask {
    core_types::ChannelMask::SHAPE
        | core_types::ChannelMask::OPACITY
        | core_types::ChannelMask::FLUID
}
pub(super) fn source_version(page: &Page) -> Version {
    page.channel_version(channels())
}
fn canonical(page: PageId) -> bool {
    page.space.0 == 0 && (0..8).contains(&page.y)
}
pub(super) struct GeometryWorker {
    pub(super) projection: Option<cold_projection_owner::Owner>,
    allow_cold: bool,
    projection_due: BTreeMap<(u64, PageId), (Version, Tick)>,
    delay: u64,
    next_peer: u64,
}
impl GeometryWorker {
    pub(super) fn new(
        generator: Arc<gen_authority::ServerGenerator>,
        delay: u8,
        cold_enabled: bool,
    ) -> Result<Self, crate::ServerError> {
        let limits = cold_projection_owner::Limits {
            completion_delay_ticks: delay,
            capture: crate::cold_projection::Limits {
                allow_cold: cold_enabled,
                ..Default::default()
            },
            ..Default::default()
        };
        let projection = Some(
            cold_projection_owner::Owner::new(generator, limits)
                .map_err(|_| crate::ServerError::Invalid("cold public projection worker"))?,
        );
        Ok(Self {
            projection,
            allow_cold: cold_enabled,
            projection_due: BTreeMap::new(),
            delay: u64::from(delay),
            next_peer: 0,
        })
    }
    pub(super) fn uses_cold(&self, page: PageId) -> bool {
        self.projection.is_some() && canonical(page)
    }
    pub(super) fn projects_missing(&self, page: PageId) -> bool {
        self.allow_cold && self.uses_cold(page)
    }
    pub(super) fn forget(&mut self, connection: u64, page: PageId) {
        self.projection_due.remove(&(connection, page));
        if let Some(owner) = &mut self.projection {
            owner.forget(connection, page);
        }
    }
    pub(super) fn forget_peer(&mut self, connection: u64) {
        self.projection_due.retain(|(c, _), _| *c != connection);
        if let Some(owner) = &mut self.projection {
            owner.forget_peer(connection);
        }
    }
    pub(super) fn observe(&mut self, changes: &[world::WorldChange], now: Tick) {
        if let Some(owner) = &mut self.projection {
            owner.observe(changes, now);
        }
    }
}
fn wire(
    page: PageId,
    head: cold_projection_owner::Head,
    body: &lod::PublicGeometryData,
) -> PublicGeometry {
    PublicGeometry {
        page,
        source_version: head.source,
        view_version: head.view,
        known: body.known,
        occupied: body.occupied,
        index_bits: body.index_bits,
        indices: body.indices.clone(),
        palette: body
            .palette
            .iter()
            .map(|s| PublicSurface {
                atlas_layer: s.atlas_layer,
                tint: s.tint,
                kind: match s.kind {
                    lod::PublicSurfaceKind::Opaque => PublicSurfaceKind::Opaque,
                    lod::PublicSurfaceKind::Cutout => PublicSurfaceKind::Cutout,
                    lod::PublicSurfaceKind::Water => PublicSurfaceKind::Water,
                },
            })
            .collect(),
    }
}
impl Server {
    pub fn cold_projection_stats(&self) -> Option<cold_projection_owner::Stats> {
        self.geometry.projection.as_ref().map(|owner| owner.stats())
    }
    pub(super) fn publish_geometry(&mut self) -> Vec<Outgoing> {
        let mut output = Vec::new();
        if self.height_ready()
            && let Some(owner) = &mut self.geometry.projection
        {
            match owner.pump(&self.world, self.runtimes.height.index(), self.tick, 2, 2) {
                Ok(work) => {
                    self.counters.geometry_cells += work.geometry_cells;
                    self.counters.geometry_tint_samples += work.tint_samples;
                    self.counters.geometry_public_witness_bytes += work.public_witness_bytes;
                    self.counters.geometry_submitted += work.submitted;
                    self.counters.geometry_discarded += work.stale + work.failed;
                }
                Err(_) => {
                    self.fail("cold_projection_worker");
                    return output;
                }
            }
        }
        // Public exterior connectivity may change a centre image after an
        // edit in another page. Forward completed public-head changes, rather
        // than relying on the changed authoritative PageId alone.
        for _ in 0..4 {
            let Some(page) = self
                .geometry
                .projection
                .as_mut()
                .and_then(|owner| owner.take_changed_geometry())
            else {
                break;
            };
            let key = net::height::HeightKey {
                space: page.space.0,
                level: 0,
                x: page.x,
                z: page.z,
            };
            for peer in self.peers.values_mut().filter(|p| p.ready) {
                if peer.geometry_subscriptions.contains_key(&page)
                    && !peer.geometry_requests.iter().any(|(p, _)| *p == page)
                {
                    peer.geometry_requests.push_back((page, None));
                }
                if peer.height_subscriptions.contains_key(&key) {
                    if let Some((_, known)) =
                        peer.height_requests.iter_mut().find(|(k, _)| *k == key)
                    {
                        *known = None;
                    } else if peer.height_requests.len() < crate::height_stream::MAX_TILES {
                        peer.height_requests.push_back((key, None));
                    }
                }
            }
        }
        for _ in 0..4 {
            let Some(page) = self
                .geometry
                .projection
                .as_mut()
                .and_then(|owner| owner.take_changed_water())
            else {
                break;
            };
            self.far_water.wake_projection(page, self.tick);
        }
        let mut connections: Vec<_> = self.peers.keys().copied().collect();
        connections.sort_by_key(|id| (*id <= self.geometry.next_peer, *id));
        let mut submitted = 0;
        for connection in connections {
            if submitted >= 2 {
                break;
            }
            let request = self
                .peers
                .get_mut(&connection)
                .filter(|p| p.ready)
                .and_then(|p| p.geometry_requests.pop_front());
            let Some((page, known)) = request else {
                continue;
            };
            self.geometry.next_peer = connection;
            let allowed = self
                .peers
                .get(&connection)
                .and_then(|p| self.observer_page(p, page.space))
                .is_some_and(|center| net::aoi_contains(center, page, 1024, 1024, false));
            if !allowed {
                output.push(Outgoing::reliable(
                    connection,
                    ServerMessage::GeometryUnavailable(page),
                ));
                continue;
            }
            // The bounded exterior policy currently certifies only the
            // canonical profile. Unsupported spaces/heights stay unavailable;
            // ore redaction alone cannot authorize enclosed occupancy images.
            if !canonical(page) {
                output.push(Outgoing::reliable(
                    connection,
                    ServerMessage::GeometryUnavailable(page),
                ));
                continue;
            }
            if self.geometry.uses_cold(page) {
                if !self.geometry.allow_cold && self.world.page(page).is_none() {
                    output.push(Outgoing::reliable(
                        connection,
                        ServerMessage::GeometryUnavailable(page),
                    ));
                    continue;
                }
                let owner = self.geometry.projection.as_mut().expect("cold owner");
                if owner.interest(connection, page, self.tick).is_err() {
                    output.push(Outgoing::reliable(
                        connection,
                        ServerMessage::GeometryUnavailable(page),
                    ));
                    continue;
                }
                if let Some((head, body)) =
                    owner.geometry(page, &self.world, self.runtimes.height.index())
                {
                    if self.geometry.delay != 0 && known != Some(head.source) {
                        let entry = self
                            .geometry
                            .projection_due
                            .entry((connection, page))
                            .or_insert((
                                head.source,
                                Tick(self.tick.0.saturating_add(self.geometry.delay)),
                            ));
                        if entry.0 != head.source {
                            *entry = (
                                head.source,
                                Tick(self.tick.0.saturating_add(self.geometry.delay)),
                            );
                        }
                        if self.tick < entry.1 {
                            self.peers
                                .get_mut(&connection)
                                .expect("listed peer")
                                .geometry_requests
                                .push_back((page, known));
                            submitted += 1;
                            continue;
                        }
                        self.geometry.projection_due.remove(&(connection, page));
                    }
                    // A known global public head confirms the same immutable
                    // source/view even when this connection reused cached bytes.
                    let peer = self.peers.get_mut(&connection).expect("listed peer");
                    peer.geometry_subscriptions.insert(page, head.source);
                    peer.geometry_views.insert(page, head.view);
                    if known != Some(head.source) {
                        output.push(Outgoing::reliable(
                            connection,
                            ServerMessage::Geometry(Box::new(wire(page, head, &body))),
                        ));
                        self.counters.geometry_published += 1;
                    }
                } else {
                    let _ = owner.request(
                        ColumnKey {
                            x: page.x,
                            z: page.z,
                        },
                        self.tick,
                    );
                    self.peers
                        .get_mut(&connection)
                        .expect("listed peer")
                        .geometry_requests
                        .push_back((page, known));
                }
                submitted += 1;
                continue;
            }
            // No unsealed resident fallback exists. A missing certified owner
            // image remains unavailable, including in generation-disabled saves.
            output.push(Outgoing::reliable(
                connection,
                ServerMessage::GeometryUnavailable(page),
            ));
        }
        output
    }
}
