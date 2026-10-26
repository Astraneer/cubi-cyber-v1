//! Bounded courier for public water certificates. GPU ownership remains in
//! ViewHorizon; the socket cache is never proof that a replacement is resident.
use crate::network::{Connection, NetworkError};
use core_types::PageId;
use net::water::{Request, WaterHalo};
use std::{collections::BTreeMap, sync::Arc};

pub(super) struct Courier {
    wanted: Vec<Request>,
    interests: BTreeMap<PageId, Request>,
    restore_cursor: usize,
}
fn same(a: Request, b: Request) -> bool {
    a.page == b.page
        && a.geometry_source == b.geometry_source
        && a.geometry_view == b.geometry_view
        && a.column_source == b.column_source
}
fn matches(request: Request, image: &WaterHalo) -> bool {
    same(
        request,
        Request {
            page: image.page,
            geometry_source: image.geometry_source,
            geometry_view: image.geometry_view,
            column_source: image.column_source,
            acknowledged: None,
        },
    )
}
impl Courier {
    pub(super) fn new() -> Self {
        Self {
            wanted: Vec::with_capacity(net::MAX_PUBLIC_GEOMETRY_PAGES),
            interests: BTreeMap::new(),
            restore_cursor: 0,
        }
    }
    pub(super) fn wanted(&mut self, requests: impl Iterator<Item = Request>) -> Result<(), String> {
        self.wanted.clear();
        for mut request in requests {
            if self.wanted.len() == net::MAX_PUBLIC_GEOMETRY_PAGES {
                return Err("public water interest capacity".into());
            }
            request
                .validate()
                .map_err(|e| format!("public water binding: {e:?}"))?;
            request.acknowledged = None;
            self.wanted.push(request);
        }
        self.wanted.sort_unstable_by_key(|r| r.page);
        if self.wanted.windows(2).any(|p| p[0].page == p[1].page) {
            return Err("duplicate public water page".into());
        }
        Ok(())
    }
    fn current(&self, page: PageId) -> Option<Request> {
        self.wanted
            .binary_search_by_key(&page, |r| r.page)
            .ok()
            .map(|i| self.wanted[i])
    }
    pub(super) fn pump(
        &mut self,
        connection: &mut Connection,
        now: u64,
        mut admit: impl FnMut(Arc<WaterHalo>) -> Result<bool, String>,
    ) -> Result<u64, String> {
        if connection.welcome().is_none() {
            return Ok(0);
        }
        let mut retired = [None; 4];
        for (out, page) in retired.iter_mut().zip(
            self.interests
                .keys()
                .filter(|p| self.current(**p).is_none()),
        ) {
            *out = Some(*page);
        }
        for page in retired.into_iter().flatten() {
            match connection.release_water(page) {
                Ok(()) => {
                    self.interests.remove(&page);
                }
                Err(NetworkError::Capacity) => break,
                Err(e) => return Err(super::error(e)),
            }
        }
        let mut requested = 0;
        for request in self.wanted.iter().copied() {
            if self
                .interests
                .get(&request.page)
                .is_some_and(|&old| same(old, request))
            {
                continue;
            }
            if requested == 4 {
                break;
            }
            match connection.request_water(request, now) {
                Ok(()) => {
                    self.interests.insert(request.page, request);
                    requested += 1;
                }
                Err(NetworkError::Capacity) => break,
                Err(e) => return Err(super::error(e)),
            }
        }
        let mut installed = 0;
        for _ in 0..4 {
            let Some(image) = connection.take_water_update() else {
                break;
            };
            if self.current(image.page).is_some_and(|r| matches(r, &image)) {
                installed += u64::from(admit(image)?);
            }
        }
        // A slower owner or an entry recreated after cover retirement can need
        // a retained image even though its transport notification was drained.
        // Re-admission is idempotent and does no voxel decoding on this thread.
        for _ in 0..self.wanted.len().min(4) {
            self.restore_cursor %= self.wanted.len();
            let request = self.wanted[self.restore_cursor];
            self.restore_cursor += 1;
            if let Some(image) = connection
                .water_image(request.page)
                .filter(|i| matches(request, i))
            {
                installed += u64::from(admit(Arc::clone(image))?);
            }
        }
        Ok(installed)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use core_types::{SpaceId, Tick, Version};
    use net::{ActionId, ClientMessage, PlayerId, PlayerState, Role, ServerMessage, Welcome};
    fn request(x: i32) -> Request {
        Request {
            page: PageId {
                space: SpaceId(0),
                x,
                y: 0,
                z: 0,
            },
            geometry_source: Version(7),
            geometry_view: 11,
            column_source: 13,
            acknowledged: None,
        }
    }
    fn connection() -> Connection {
        let mut c = Connection::new(crate::network::NetworkConfig::default()).unwrap();
        let identity = PlayerId([1; 16]);
        c.receive(
            ServerMessage::Welcome(Welcome {
                session: 1,
                identity,
                new_credential: None,
                role: Role::Player,
                tick: Tick(0),
                next_action: ActionId(1),
                public_seed: 1,
                generator_version: terrain_gen::GENERATOR_VERSION,
                rules_version: net::RULES_VERSION,
                player: PlayerState {
                    identity,
                    health: 20.,
                    position: [0.5, 1., 0.5],
                    ..PlayerState::default()
                },
            }),
            0,
        )
        .unwrap();
        c.take_outgoing(256);
        c
    }
    fn halo(r: Request, water_view: u64) -> WaterHalo {
        WaterHalo {
            page: r.page,
            geometry_source: r.geometry_source,
            geometry_view: r.geometry_view,
            column_source: r.column_source,
            water_view,
            cells: Box::new([0; 5832]),
            tints: [0xffff; 256],
        }
    }
    #[test]
    fn actual_cache_credit_rebind_and_slow_owner_repair_use_only_current_public_binding() {
        let mut c = connection();
        let mut courier = Courier::new();
        let a = request(0);
        courier.wanted([a].into_iter()).unwrap();
        courier.pump(&mut c, 1, |_| panic!("no image yet")).unwrap();
        assert!(matches!(c.take_outgoing(256).as_slice(),[ClientMessage::RequestWater(r)]if *r==a));
        c.receive(ServerMessage::WaterHalo(Box::new(halo(a, 1))), 2)
            .unwrap();
        let mut seen = Vec::new();
        courier
            .pump(&mut c, 3, |image| {
                seen.push(image.geometry_view);
                Ok(true)
            })
            .unwrap();
        assert!(seen.iter().all(|&v| v == 11));
        // Restore without a new packet, as required by a recreated GPU owner.
        seen.clear();
        courier
            .pump(&mut c, 4, |image| {
                seen.push(image.geometry_view);
                Ok(true)
            })
            .unwrap();
        assert_eq!(seen, [11]);
        let b = Request {
            geometry_view: 12,
            ..a
        };
        courier.wanted([b].into_iter()).unwrap();
        seen.clear();
        courier
            .pump(&mut c, 5, |image| {
                seen.push(image.geometry_view);
                Ok(true)
            })
            .unwrap();
        assert!(seen.is_empty());
        assert!(c.water_image(a.page).is_none());
        c.receive(ServerMessage::WaterHalo(Box::new(halo(a, 2))), 6)
            .unwrap();
        assert!(c.water_image(a.page).is_none());
        c.receive(ServerMessage::WaterHalo(Box::new(halo(b, 1))), 7)
            .unwrap();
        courier
            .pump(&mut c, 8, |image| {
                assert_eq!(image.geometry_view, 12);
                Ok(true)
            })
            .unwrap();
        courier.wanted(std::iter::empty()).unwrap();
        courier
            .pump(&mut c, 9, |_| panic!("retired image"))
            .unwrap();
        assert_eq!(c.water_queue_depths(), (0, 0, 0));
    }
    #[test]
    fn each_owner_step_requests_and_retires_at_most_four_pages_and_never_requests_before_welcome() {
        let mut c = Connection::new(crate::network::NetworkConfig::default()).unwrap();
        let mut courier = Courier::new();
        courier.wanted((0..10).map(request)).unwrap();
        courier.pump(&mut c, 0, |_| Ok(false)).unwrap();
        assert!(c.take_outgoing(256).is_empty());
        let mut c = connection();
        courier.pump(&mut c, 1, |_| Ok(false)).unwrap();
        assert_eq!(c.water_queue_depths().0, 4);
        assert_eq!(c.take_outgoing(256).len(), 4);
        courier.pump(&mut c, 2, |_| Ok(false)).unwrap();
        assert_eq!(c.water_queue_depths().0, 8);
        c.take_outgoing(256);
        courier.wanted(std::iter::empty()).unwrap();
        courier.pump(&mut c, 3, |_| Ok(false)).unwrap();
        assert_eq!(c.water_queue_depths().0, 4);
        assert_eq!(c.take_outgoing(256).len(), 4);
    }
}
