//! Bounded worker packing of actual public world geometry. Results publish only
//! at the durable world frontier and only when their captured source is current.
use crate::{Outgoing, Server, lod};
use core_types::{PageId, Version};
use net::{PublicGeometry, PublicSurface, PublicSurfaceKind, ServerMessage};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};
use world::Page;

pub(super) fn channels() -> core_types::ChannelMask {
    core_types::ChannelMask::SHAPE
        | core_types::ChannelMask::OPACITY
        | core_types::ChannelMask::FLUID
}
pub(super) fn source_version(page: &Page) -> Version {
    page.channel_version(channels())
}

type Pool = jobs::NativePool<(PageId, Arc<Page>), (PublicGeometry, u64)>;
pub(super) struct GeometryWorker {
    pool: Pool,
    inflight: BTreeMap<jobs::JobId, (u64, PageId, Version, core_types::Tick)>,
    ready: VecDeque<(u64, PublicGeometry, core_types::Tick)>,
    delay: u64,
    next_job: u64,
    next_peer: u64,
}
impl GeometryWorker {
    pub(super) fn new(terrain: r#gen::Generator, delay: u8) -> Result<Self, crate::ServerError> {
        let pool = Pool::new(
            2,
            8,
            move |(page, data), cancel: &jobs::CancellationToken| {
                if cancel.is_cancelled() {
                    return Err("cancelled public geometry".into());
                }
                let tints = if page.space == core_types::SpaceId(0) {
                    let mut tints = [0; 256];
                    for z in 0..16 {
                        for x in 0..16 {
                            tints[x + 16 * z] = terrain
                                .column_info(page.x * 16 + x as i32, page.z * 16 + z as i32)
                                .tint;
                        }
                    }
                    Some(tints)
                } else {
                    None
                };
                if cancel.is_cancelled() {
                    return Err("cancelled public geometry".into());
                }
                let packed = lod::geometry_from_page(&data, tints.as_ref());
                let geometry = PublicGeometry {
                    page,
                    source_version: source_version(&data),
                    view_version: 0,
                    known: packed.known,
                    occupied: packed.occupied,
                    index_bits: packed.index_bits,
                    indices: packed.indices,
                    palette: packed
                        .palette
                        .into_iter()
                        .map(|p| PublicSurface {
                            atlas_layer: p.atlas_layer,
                            tint: p.tint,
                            kind: match p.kind {
                                lod::PublicSurfaceKind::Opaque => PublicSurfaceKind::Opaque,
                                lod::PublicSurfaceKind::Cutout => PublicSurfaceKind::Cutout,
                                lod::PublicSurfaceKind::Water => PublicSurfaceKind::Water,
                            },
                        })
                        .collect(),
                };
                Ok((geometry, packed.scanned_cells))
            },
        )
        .map_err(|_| crate::ServerError::Invalid("public geometry worker"))?;
        Ok(Self {
            pool,
            inflight: BTreeMap::new(),
            ready: VecDeque::new(),
            delay: u64::from(delay),
            next_job: 1,
            next_peer: 0,
        })
    }
}
impl Server {
    pub(super) fn publish_geometry(&mut self) -> Vec<Outgoing> {
        let mut output = Vec::new();
        for _ in 0..2 {
            if self.geometry.ready.len() >= 8 {
                break;
            }
            let Some(completion) = self.geometry.pool.try_recv() else {
                break;
            };
            let Some((connection, page, version, submitted)) =
                self.geometry.inflight.remove(&completion.id)
            else {
                continue;
            };
            let jobs::JobOutcome::Success((geometry, cells)) = completion.outcome else {
                continue;
            };
            self.counters.geometry_cells += cells;
            if page.space == core_types::SpaceId(0) {
                self.counters.geometry_tint_samples += 256;
            }
            if geometry.page != page || geometry.source_version != version {
                self.counters.geometry_discarded += 1;
                continue;
            }
            self.geometry.ready.push_back((
                connection,
                geometry,
                core_types::Tick(submitted.0.saturating_add(self.geometry.delay)),
            ));
            self.counters.geometry_held_peak = self
                .counters
                .geometry_held_peak
                .max(self.geometry.ready.len());
        }
        for _ in 0..2 {
            let Some(index) = self
                .geometry
                .ready
                .iter()
                .position(|(_, _, due)| *due <= self.tick)
            else {
                break;
            };
            let (connection, mut geometry, _) = self
                .geometry
                .ready
                .remove(index)
                .expect("held result index");
            let page = geometry.page;
            let version = geometry.source_version;
            let Some(peer) = self
                .peers
                .get_mut(&connection)
                .filter(|p| p.ready && p.geometry_subscriptions.contains_key(&page))
            else {
                continue;
            };
            if self.world.page(page).is_none()
                || self.world.input_version(page, channels()) != version
            {
                self.counters.geometry_discarded += 1;
                if !peer.geometry_requests.iter().any(|(p, _)| *p == page) {
                    peer.geometry_requests.push_back((page, None));
                }
                continue;
            }
            if peer.geometry_subscriptions.get(&page) == Some(&version)
                && let Some(view) = peer.geometry_views.get(&page)
            {
                geometry.view_version = *view;
            } else {
                peer.geometry_view = peer.geometry_view.saturating_add(1);
                geometry.view_version = peer.geometry_view;
            }
            peer.geometry_subscriptions.insert(page, version);
            peer.geometry_views.insert(page, geometry.view_version);
            output.push(Outgoing::reliable(
                connection,
                ServerMessage::Geometry(Box::new(geometry)),
            ));
            self.counters.geometry_published += 1;
        }
        // A persistent cursor prevents one low connection ID from consuming all
        // public geometry slots while other peers wait.
        let mut connections: Vec<_> = self.peers.keys().copied().collect();
        connections.sort_by_key(|connection| (*connection <= self.geometry.next_peer, *connection));
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
            let Some(snapshot) = self.world.snapshot(page) else {
                self.request_generation(page);
                output.push(Outgoing::reliable(
                    connection,
                    ServerMessage::GeometryUnavailable(page),
                ));
                continue;
            };
            let version = source_version(&snapshot);
            if known == Some(version) {
                continue;
            }
            if self
                .geometry
                .inflight
                .values()
                .any(|(c, p, v, _)| *c == connection && *p == page && *v == version)
                || self.geometry.ready.iter().any(|(c, g, _)| {
                    *c == connection && g.page == page && g.source_version == version
                })
            {
                continue;
            }
            let id = jobs::JobId(self.geometry.next_job);
            self.geometry.next_job = self.geometry.next_job.saturating_add(1);
            let spec = jobs::JobSpec::derived(
                jobs::JobKey { page, kind: 62 },
                jobs::Priority::Normal,
                jobs::WorkCost {
                    cells: 4352,
                    quads: 0,
                    bytes: 40_000,
                    rays: 0,
                },
                64 * 1024,
                self.tick,
            );
            match self.geometry.pool.try_submit(jobs::Dispatch {
                id,
                spec,
                cancellation: jobs::CancellationToken::new(),
                payload: (page, snapshot),
            }) {
                Ok(()) => {
                    self.geometry
                        .inflight
                        .insert(id, (connection, page, version, self.tick));
                    self.counters.geometry_submitted += 1;
                    submitted += 1;
                }
                Err(_) => {
                    self.peers
                        .get_mut(&connection)
                        .expect("listed peer")
                        .geometry_requests
                        .push_front((page, known));
                    break;
                }
            }
        }
        output
    }
}
