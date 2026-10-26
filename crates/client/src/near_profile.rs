//! Loading-time near-view selection. Requested values are minimum horizontal
//! block radii; page quantization and all raw/GPU reservations remain explicit.
use crate::{SceneConfig, network::NetworkConfig};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub raw_pages: usize,
    pub gpu_objects: usize,
    /// Includes retained edit patches and the legacy six-part self fallback.
    pub auxiliary_objects: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            raw_pages: 256,
            gpu_objects: 170,
            auxiliary_objects: 70,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Range,
    RawCapacity,
    GpuCapacity,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}
impl std::error::Error for Error {}
impl Error {
    pub const fn message(self) -> &'static str {
        match self {
            Self::Range => "Ближняя область: выберите от 4 до 64 блоков.",
            Self::RawCapacity => {
                "Сейчас доступны ближние области до 32 блоков. Выберите 32 или меньше."
            }
            Self::GpuCapacity => "Ближняя область не помещается в текущем графическом профиле.",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NearProfile {
    pub requested_blocks: u16,
    pub effective_blocks: u16,
    pub horizontal_radius: u8,
    pub vertical_radius: u8,
    pub visible_pages: usize,
    pub raw_pages: usize,
    pub reserved_objects: usize,
}
impl NearProfile {
    pub fn plan(requested_blocks: u16, limits: Limits) -> Result<Self, Error> {
        if !(4..=64).contains(&requested_blocks) {
            return Err(Error::Range);
        }
        let horizontal_radius = requested_blocks.div_ceil(16) as u8;
        let vertical_radius = 1;
        let horizontal = 2 * usize::from(horizontal_radius) + 1;
        let visible_pages = horizontal * horizontal * 3;
        let raw_pages = (horizontal + 2) * (horizontal + 2) * 5;
        // The current confirmed-page protocol/owner has a hard256-page cap.
        // A larger render arena alone cannot authorize a wider raw interest.
        if raw_pages > limits.raw_pages.min(256) {
            return Err(Error::RawCapacity);
        }
        let reserved_objects = visible_pages.saturating_add(limits.auxiliary_objects);
        if reserved_objects > limits.gpu_objects {
            return Err(Error::GpuCapacity);
        }
        Ok(Self {
            requested_blocks,
            effective_blocks: u16::from(horizontal_radius) * 16,
            horizontal_radius,
            vertical_radius,
            visible_pages,
            raw_pages,
            reserved_objects,
        })
    }
    pub fn apply_scene(self, scene: &mut SceneConfig) {
        scene.horizontal_radius = self.horizontal_radius;
        scene.vertical_radius = self.vertical_radius;
    }
    pub fn apply_network(self, network: &mut NetworkConfig) {
        network.near_radius = u16::from(self.horizontal_radius) + 1;
        network.near_vertical_radius = Some(u16::from(self.vertical_radius) + 1);
        network.retain_radius = network.near_radius + 1;
        network.retain_vertical_radius = Some(u16::from(self.vertical_radius) + 2);
    }
    pub fn matches_scene(self, scene: &SceneConfig) -> bool {
        scene.horizontal_radius == self.horizontal_radius
            && scene.vertical_radius == self.vertical_radius
            && scene.max_loaded_pages >= self.raw_pages
    }
    pub fn matches_network(self, network: NetworkConfig) -> bool {
        network.interest_radii(false)
            == [
                u16::from(self.horizontal_radius) + 1,
                u16::from(self.vertical_radius) + 1,
                u16::from(self.horizontal_radius) + 1,
            ]
            && network.maximum_pages >= self.raw_pages
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_types::{PageId, SpaceId};
    #[test]
    fn thirty_two_blocks_reserve_anisotropic_halo_without_requesting_a_cube() {
        let profile = NearProfile::plan(32, Limits::default()).unwrap();
        assert_eq!(
            (
                profile.visible_pages,
                profile.raw_pages,
                profile.reserved_objects
            ),
            (75, 245, 145)
        );
        let mut scene = SceneConfig::forest();
        profile.apply_scene(&mut scene);
        assert!(profile.matches_scene(&scene));
        let mut network = NetworkConfig::remote_scene();
        profile.apply_network(&mut network);
        assert_eq!(network.requested_pages(), 245);
        assert!(crate::network::Connection::new(network).is_ok());
        let center = PageId {
            space: SpaceId(0),
            x: -13,
            y: 5,
            z: 7,
        };
        let mut count = 0;
        for y in -4..=4 {
            for z in -4..=4 {
                for x in -4..=4 {
                    let page = PageId {
                        x: center.x + x,
                        y: center.y + y,
                        z: center.z + z,
                        ..center
                    };
                    assert_eq!(
                        network.contains(center, page, false),
                        x.abs() <= 3 && z.abs() <= 3 && y.abs() <= 2
                    );
                    count += usize::from(network.contains(center, page, false));
                }
            }
        }
        assert_eq!(count, 245);
        network.near_vertical_radius = None;
        assert!(matches!(
            crate::network::Connection::new(network),
            Err(crate::network::NetworkError::Capacity)
        ));
    }
    #[test]
    fn quantization_and_unsupported_capacity_are_reported_before_loading() {
        for requested in [4, 8, 12, 16] {
            let profile = NearProfile::plan(requested, Limits::default()).unwrap();
            assert_eq!(
                (
                    profile.effective_blocks,
                    profile.visible_pages,
                    profile.raw_pages
                ),
                (16, 27, 125)
            );
        }
        for requested in [48, 64] {
            assert_eq!(
                NearProfile::plan(requested, Limits::default()),
                Err(Error::RawCapacity)
            );
            assert_eq!(
                NearProfile::plan(
                    requested,
                    Limits {
                        raw_pages: 4096,
                        gpu_objects: 4096,
                        auxiliary_objects: 0
                    }
                ),
                Err(Error::RawCapacity)
            );
        }
        assert_eq!(
            NearProfile::plan(
                32,
                Limits {
                    gpu_objects: 144,
                    ..Limits::default()
                }
            ),
            Err(Error::GpuCapacity)
        );
        assert_eq!(NearProfile::plan(3, Limits::default()), Err(Error::Range));
        assert_eq!(NearProfile::plan(65, Limits::default()), Err(Error::Range));
    }
    #[test]
    fn real_interest_queue_contains_the_full_halo_and_preserves_capacity_while_moving() {
        use crate::network::Connection;
        use net::{ActionId, ClientMessage, PlayerId, PlayerState, Role, ServerMessage, Welcome};
        let mut config = NetworkConfig::remote_scene();
        NearProfile::plan(32, Limits::default())
            .unwrap()
            .apply_network(&mut config);
        let mut connection = Connection::new(config).unwrap();
        let player = PlayerState {
            identity: PlayerId([1; 16]),
            position: [0.5, 80., 0.5],
            health: 20.,
            ..PlayerState::default()
        };
        connection
            .receive(
                ServerMessage::Welcome(Welcome {
                    session: 1,
                    identity: player.identity,
                    new_credential: None,
                    role: Role::Player,
                    tick: core_types::Tick(1),
                    next_action: ActionId(1),
                    public_seed: 1337,
                    generator_version: terrain_gen::GENERATOR_VERSION,
                    rules_version: net::RULES_VERSION,
                    player,
                }),
                0,
            )
            .unwrap();
        let mut authority = sim::Player::new(player.position);
        authority.vel = [0.; 3];
        authority.on_ground = false;
        connection
            .receive(
                ServerMessage::Prediction {
                    tick: core_types::Tick(1),
                    player,
                    continuation: sim::prediction::PredictionState::capture(&authority)
                        .unwrap()
                        .as_bytes()
                        .to_vec(),
                },
                0,
            )
            .unwrap();
        connection.advance_to(core_types::Tick(1)).unwrap();
        connection.update_interest(0).unwrap();
        let initial = connection
            .take_outgoing(256)
            .into_iter()
            .filter_map(|message| {
                if let ClientMessage::RequestPage { page, .. } = message {
                    Some(page)
                } else {
                    None
                }
            })
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(initial.len(), 245);
        assert!(
            initial
                .iter()
                .all(|p| p.x.abs() <= 3 && p.z.abs() <= 3 && (p.y - 5).abs() <= 2)
        );
        // Network I/O continues while a loading GUI consumes no page events.
        // ACK/repair outbox credit is drained independently of that GUI queue.
        let raw = |page, view| {
            ServerMessage::Page(net::PageUpdate::Snapshot(net::PageSnapshot {
                page,
                version: view,
                source_version: core_types::Version(view),
                applied_action: Default::default(),
                cells: Box::new([core_types::Cell::AIR; 4096]),
            }))
        };
        for page in &initial {
            connection.receive(raw(*page, 1), 1).unwrap();
            connection.take_outgoing(256);
        }
        assert_eq!(connection.queue_depths().cells, 245);
        let first = *initial.first().unwrap();
        connection.receive(raw(first, 2), 2).unwrap();
        connection.take_outgoing(256);
        assert_eq!(
            connection.queue_depths().cells,
            245,
            "same-page updates coalesce while the GUI is frozen"
        );
        let outside = PageId { x: 20, ..first };
        connection.receive(raw(outside, 1), 3).unwrap();
        assert!(connection.replica().page(outside).is_none());
        let mut received = connection.take_page_updates(128);
        received.extend(connection.take_page_updates(128));
        assert_eq!(received.len(), 245);
        assert!(received.iter().any(|update| matches!(update, net::PageUpdate::Snapshot(snapshot) if snapshot.page == first && snapshot.version == 2)));
        assert_eq!(connection.queue_depths().cells, 0);
        let identity = connection.welcome().unwrap().identity;
        let predicted =
            sim::prediction::PredictionState::capture(connection.player().unwrap()).unwrap();
        let smaller = NearProfile::plan(16, Limits::default()).unwrap();
        assert!(connection.set_near_profile(smaller).unwrap());
        assert!(connection.near_profile_applied(smaller));
        assert!(
            connection
                .set_near_profile(NearProfile::plan(32, Limits::default()).unwrap())
                .unwrap()
        );
        assert_eq!(connection.welcome().unwrap().identity, identity);
        assert_eq!(
            sim::prediction::PredictionState::capture(connection.player().unwrap()).unwrap(),
            predicted,
            "a near profile change cannot reset the live session/prediction"
        );
        // A delayed Forget can retire the server lease after the camera has
        // re-entered. Its explicit Unavailable echo must restart current interest.
        connection
            .receive(ServerMessage::PageUnavailable(first), 10)
            .unwrap();
        assert!(connection.replica().page(first).is_none());
        connection.poll(600).unwrap();
        assert!(connection.take_outgoing(256).iter().any(|message| matches!(message, ClientMessage::RequestPage { page, acknowledged: None } if *page == first)));
        connection.receive(raw(first, 3), 601).unwrap();
        connection.take_outgoing(256);
        assert_eq!(connection.replica().page(first).unwrap().version, 3);
        assert!(
            matches!(connection.take_page_updates(128).as_slice(), [net::PageUpdate::Snapshot(snapshot)] if snapshot.page == first && snapshot.version == 3)
        );
        authority.pos = [16.5, 80., 0.5];
        authority.prev = authority.pos;
        connection
            .receive(
                ServerMessage::Prediction {
                    tick: core_types::Tick(2),
                    player: PlayerState {
                        position: authority.pos,
                        ..player
                    },
                    continuation: sim::prediction::PredictionState::capture(&authority)
                        .unwrap()
                        .as_bytes()
                        .to_vec(),
                },
                650,
            )
            .unwrap();
        connection.advance_to(core_types::Tick(2)).unwrap();
        connection.update_interest(650).unwrap();
        let added = connection
            .take_outgoing(256)
            .into_iter()
            .filter_map(|message| {
                if let ClientMessage::RequestPage { page, .. } = message {
                    Some(page)
                } else {
                    None
                }
            })
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            added.len(),
            35,
            "one camera-page crossing requests one 5×7 slab"
        );
        assert!(
            added
                .iter()
                .all(|p| p.x == 4 && p.z.abs() <= 3 && (p.y - 5).abs() <= 2)
        );
        assert!(connection.counters().peak_outbox <= 256);
        assert!(
            connection.replica().page(first).is_none(),
            "the farthest old slab member must retire under the fixed256-page cap"
        );
        connection
            .receive(ServerMessage::PageUnavailable(first), 700)
            .unwrap();
        connection.poll(1200).unwrap();
        assert!(!connection.take_outgoing(256).iter().any(
            |message| matches!(message, ClientMessage::RequestPage { page, .. } if *page == first)
        ));
        connection.receive(raw(first, 4), 1201).unwrap();
        assert!(
            connection.replica().page(first).is_none(),
            "late old-lease images cannot repopulate retired interest"
        );
    }
}
