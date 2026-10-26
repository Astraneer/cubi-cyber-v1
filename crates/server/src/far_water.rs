//! Public exact-column water: bounded workers, sampled source certificates and
//! confirmed per-page image credit. No worker may mutate/generate the World.
pub mod capture;
mod owner;
use crate::{Outgoing, Peer, Server};
use core_types::{PageId, SpaceId};
use net::{ServerMessage, height::HeightKey, water::Request};
pub(super) use owner::Owner;
pub use owner::Stats;
impl Server {
    pub fn far_water_stats(&self) -> Stats {
        self.far_water.stats(self.tick)
    }
    pub(super) fn water_allowed(&self, connection: u64, peer: &Peer, request: Request) -> bool {
        allowed(
            &self.world,
            self.runtimes.height.index(),
            self.geometry.projection.as_ref(),
            &self.height_heads,
            connection,
            peer,
            request,
        )
    }
    pub(super) fn publish_far_water(&mut self) -> Vec<Outgoing> {
        if self.pending.is_some() || self.failed {
            return Vec::new();
        }
        let mut output = Vec::new();
        self.far_water
            .retain_peers(|id| self.peers.contains_key(&id));
        self.far_water.begin_tick(self.tick);
        for _ in 0..2 {
            let Some(ready) = self.far_water.poll(&self.world) else {
                continue;
            };
            if !self
                .peers
                .get(&ready.key.0)
                .is_some_and(|p| self.water_allowed(ready.key.0, p, ready.request))
            {
                // A replacement source must request a fresh binding. Its old
                // public front remains on the client until the pair is ready.
                continue;
            }
            let connection = ready.key.0;
            if let Ok(halo) = self.far_water.publish(ready, self.tick) {
                output.push(Outgoing::reliable(
                    connection,
                    ServerMessage::WaterHalo(halo),
                ));
            }
        }
        self.far_water.wake_retries(self.tick);
        for _ in 0..4 {
            let Some((key, request)) = self.far_water.next_request() else {
                break;
            };
            if !self
                .peers
                .get(&key.0)
                .is_some_and(|p| self.water_allowed(key.0, p, request))
            {
                output.push(Outgoing::reliable(
                    key.0,
                    ServerMessage::WaterUnavailable(key.1),
                ));
                continue;
            }
            if self.geometry.uses_cold(key.1) {
                let owner = self.geometry.projection.as_mut().expect("cold owner");
                if let Some(body) = owner.water(key.1, &self.world, self.runtimes.height.index()) {
                    if let Ok(Some(halo)) = self.far_water.publish_projected(
                        key,
                        request,
                        body,
                        self.height_heads
                            .private_source(key.0, column_key(key.1))
                            .expect("allowed published column"),
                    ) {
                        output.push(Outgoing::reliable(key.0, ServerMessage::WaterHalo(halo)));
                    }
                } else {
                    let _ = owner.request(
                        crate::cold_projection::ColumnKey {
                            x: key.1.x,
                            z: key.1.z,
                        },
                        self.tick,
                    );
                    self.far_water.defer(key);
                }
                continue;
            }
            // Retry only a bounded handful of coalesced canonical requests.
            // Unavailable out-of-profile pages remain Unknown, never air.
            let missing: Vec<_> = self
                .far_water
                .missing_inputs(key)
                .filter(|p| loadable(*p) && self.world.page(*p).is_none())
                .collect();
            for page in missing {
                if !self.far_water.admit_generation_attempt() {
                    break;
                }
                self.request_generation(page);
            }
            if self.far_water.skip_unchanged(key, &self.world) {
                continue;
            }
            let Ok(snapshot) = capture::Snapshot::capture(&self.world, key.1) else {
                output.push(Outgoing::reliable(
                    key.0,
                    ServerMessage::WaterUnavailable(key.1),
                ));
                continue;
            };
            for page in snapshot.missing().filter(|p| loadable(*p)) {
                if !self.far_water.admit_generation_attempt() {
                    break;
                }
                self.request_generation(page);
            }
            if !self.far_water.submit(key, request, snapshot, self.tick) {
                break;
            }
        }
        output
    }
}
fn loadable(page: PageId) -> bool {
    page.space == SpaceId(0) && (0..8).contains(&page.y)
}

fn column_key(page: PageId) -> HeightKey {
    HeightKey {
        space: page.space.0,
        level: 0,
        x: page.x,
        z: page.z,
    }
}
pub(super) fn allowed(
    world: &world::World,
    index: &crate::height_index::HeightEditIndex,
    projection: Option<&crate::cold_projection_owner::Owner>,
    height_heads: &crate::height_heads::HeightHeads,
    connection: u64,
    peer: &Peer,
    request: Request,
) -> bool {
    if !peer.ready
        || request.validate().is_err()
        || request.page.space != SpaceId(0)
        || peer.space != SpaceId(0)
    {
        return false;
    }
    let key = HeightKey {
        space: 0,
        level: 0,
        x: request.page.x,
        z: request.page.z,
    };
    let authority = crate::height_index::TileKey {
        space: SpaceId(0),
        level: 0,
        x: request.page.x,
        z: request.page.z,
    };
    peer.height_subscriptions.get(&key) == Some(&request.column_source)
        && height_heads.public_source(connection, key) == Some(request.column_source)
        && height_heads.private_source(connection, key) == index.source(authority).ok()
        && peer.geometry_subscriptions.get(&request.page) == Some(&request.geometry_source)
        && peer.geometry_views.get(&request.page) == Some(&request.geometry_view)
        && (if let Some(owner) = projection {
            owner.head(request.page, world, index).is_some_and(|head| {
                head.source == request.geometry_source && head.view == request.geometry_view
            })
        } else {
            world
                .page(request.page)
                .is_some_and(|p| crate::geometry::source_version(p) == request.geometry_source)
        })
        && net::aoi_contains(
            crate::position_page(peer.space, peer.player.pos),
            request.page,
            1024,
            1024,
            false,
        )
}

#[cfg(test)]
mod tests;
