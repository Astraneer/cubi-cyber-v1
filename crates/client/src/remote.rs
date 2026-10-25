//! Shared real-session pump. Platform sockets carry bounded binary frames;
//! authority imports, prediction and visible overlays have one scene owner.
use crate::{
    ClientScene, SceneConfig, SceneError,
    network::{Connection, NetworkConfig, NetworkError},
    view::{ClientView, InputKey, RemoteInput},
};
use core_types::{BlockPos, Cell, Tick};
use net::{Action, ActionId, PageUpdate, ReplicaRead};
use sim::{QueryWorld, RayResult};
use std::collections::BTreeSet;
use world::{CellRead, WorldChange};

#[derive(Clone, Copy, Debug, Default)]
pub struct RemoteCounters {
    pub imported_pages: u64,
    pub imported_cells: u64,
    pub prediction_changes: u64,
}
pub struct RemoteSession {
    connection: Connection,
    configured: bool,
    next_input_ms: u64,
    target: Tick,
    lead: u64,
    predicted_positions: BTreeSet<BlockPos>,
    mining: Option<(BlockPos, Cell, ActionId)>,
    next_place_ms: u64,
    counters: RemoteCounters,
}
impl RemoteSession {
    pub fn new(config: NetworkConfig) -> Result<Self, String> {
        Ok(Self {
            connection: Connection::new(config).map_err(error)?,
            configured: false,
            next_input_ms: 0,
            target: Tick(0),
            lead: config.input_lead_ticks,
            predicted_positions: BTreeSet::new(),
            mining: None,
            next_place_ms: 0,
            counters: RemoteCounters::default(),
        })
    }
    pub fn scene_config() -> SceneConfig {
        SceneConfig {
            authoritative_remote: true,
            ..SceneConfig::forest()
        }
    }
    pub fn connection(&self) -> &Connection {
        &self.connection
    }
    pub fn connection_mut(&mut self) -> &mut Connection {
        &mut self.connection
    }
    pub fn counters(&self) -> RemoteCounters {
        self.counters
    }
    pub fn receive_frame(&mut self, bytes: &[u8], now_ms: u64) -> Result<(), String> {
        let message = net::decode_server(bytes).map_err(|e| e.to_string())?;
        self.connection.receive(message, now_ms).map_err(error)
    }
    pub fn take_outgoing(&mut self) -> Option<Vec<u8>> {
        self.connection
            .take_outgoing(1)
            .pop()
            .map(|message| net::encode_client(&message).expect("validated outgoing frame"))
    }
    /// A reconnect uses a new session, scene and worker pool. Never install a
    /// second world's seed over retained meshes or restart scheduler IDs in an
    /// old platform pool.
    pub fn pump(&mut self, view: &mut ClientView, now_ms: u64) -> Result<(), String> {
        if !view.authoritative_remote() {
            return Err("remote session requires a fresh authoritative scene".into());
        }
        while let Some(control) = view.take_remote_control() {
            if let Some(player) = self.connection.player() {
                let action = match control {
                    InputKey::Flight => Action::Flight(!player.flying),
                    InputKey::Noclip => Action::DebugNoclip(!player.noclip),
                    _ => continue,
                };
                self.connection
                    .action(action, false, now_ms)
                    .map_err(error)?;
            }
        }
        let input = view.remote_input();
        let changes = self.pump_scene(&mut view.scene, input, now_ms)?;
        for change in changes {
            view.note_remote_change(&change)?;
        }
        if let Some(player) = self.connection.player() {
            view.install_remote_player(player, self.connection.predicted_tick())?;
        }
        Ok(())
    }
    /// GPU-independent owner boundary, also exercised by real socket tests.
    /// At most eight full images are installed per call; normal deltas scan
    /// only their changed cells. Unknown pages never invoke terrain generation.
    pub fn pump_scene(
        &mut self,
        scene: &mut ClientScene,
        input: RemoteInput,
        now_ms: u64,
    ) -> Result<Vec<WorldChange>, String> {
        if !scene.config().authoritative_remote {
            return Err("remote session requires authoritative scene".into());
        }
        let mut changes = Vec::new();
        if !self.configured {
            if let Some(welcome) = self.connection.welcome() {
                let p = welcome.player.position.map(|v| v.floor() as i32);
                scene
                    .configure_authority(
                        welcome.public_seed,
                        BlockPos {
                            space: welcome.player.space,
                            x: p[0],
                            y: p[1],
                            z: p[2],
                        },
                    )
                    .map_err(|e| e.to_string())?;
                self.configured = true;
            } else {
                self.connection.poll(now_ms).map_err(error)?;
                return Ok(changes);
            }
        }
        self.connection
            .advance_to(self.connection.acknowledged_tick())
            .map_err(error)?;
        if let Some(player) = self.connection.player() {
            let p = player.pos.map(|v| v.floor() as i32);
            let space = self.connection.welcome().expect("configured").player.space;
            let center = BlockPos {
                space,
                x: p[0],
                y: p[1],
                z: p[2],
            };
            if center.page() != scene.config().center.page() {
                scene.set_center(center).map_err(|e| e.to_string())?;
            }
        }
        for page in self.connection.take_page_removals(16) {
            if scene.world().page(page).is_some() || scene.authoritative_version(page).is_some() {
                let removed = scene
                    .remove_authoritative_page(page, self.connection.acknowledged_tick())
                    .map_err(|e| e.to_string())?;
                if let Some(change) = removed {
                    changes.push(change)
                }
            }
        }
        let wanted: BTreeSet<_> = scene.requested_pages().collect();
        for update in self.connection.take_page_updates(4) {
            let page = match &update {
                PageUpdate::Snapshot(p) => p.page,
                PageUpdate::Delta(p) => p.page,
            };
            if !wanted.contains(&page) {
                continue;
            }
            match scene.import_authoritative_page(&update, self.connection.acknowledged_tick()) {
                Ok(import) => {
                    self.counters.imported_pages += u64::from(import.applied);
                    self.counters.imported_cells += import.cells_scanned as u64;
                    if let Some(change) = import.change {
                        changes.push(change)
                    }
                }
                Err(SceneError::MissingAuthoritativeBase { .. }) => {}
                Err(e) => return Err(e.to_string()),
            }
        }
        // Retained replica pages may re-enter the rendering AOI without a new
        // wire event. Rehydrate only missing/stale scene images from that exact
        // confirmed cache, with a separate bounded fallback budget.
        let mut restored = 0;
        for page in wanted {
            let Some(image) = self.connection.replica().page(page) else {
                continue;
            };
            if scene
                .authoritative_version(page)
                .is_some_and(|v| v.view == image.version)
            {
                continue;
            }
            if restored == 4 {
                break;
            }
            let import = scene
                .import_replica_page(image, self.connection.acknowledged_tick())
                .map_err(|e| e.to_string())?;
            restored += 1;
            self.counters.imported_pages += u64::from(import.applied);
            self.counters.imported_cells += import.cells_scanned as u64;
            if let Some(change) = import.change {
                changes.push(change)
            }
        }
        let positions: BTreeSet<_> = self.connection.predicted_positions().collect();
        let reconcile: BTreeSet<_> = self
            .predicted_positions
            .union(&positions)
            .copied()
            .collect();
        for position in reconcile {
            let (CellRead::Known(current), ReplicaRead::Known(desired)) =
                (scene.world().read(position), self.connection.read(position))
            else {
                continue;
            };
            if current != desired {
                let change = scene
                    .apply_remote_prediction(
                        position,
                        current,
                        desired,
                        self.connection.acknowledged_tick(),
                    )
                    .map_err(|e| e.to_string())?;
                self.counters.prediction_changes += 1;
                changes.push(change)
            }
        }
        self.predicted_positions = positions;
        let next_target = Tick(
            self.connection
                .estimated_server_tick(now_ms)
                .0
                .saturating_add(self.lead)
                .max(self.target.0.saturating_add(1)),
        );
        // A stalled authority cannot authorize an unbounded future. Keep the
        // render owner responsive and retry pending inputs while new sampling
        // pauses at the protocol's explicit64-tick prediction boundary.
        if self.connection.player().is_some()
            && now_ms >= self.next_input_ms
            && self.connection.pending_inputs() < 128
            && next_target.0 <= self.connection.acknowledged_tick().0.saturating_add(64)
        {
            let frame = self
                .connection
                .schedule_input(next_target, input.keys, input.yaw, input.pitch)
                .map_err(error)?;
            self.target = frame.target_tick;
            self.next_input_ms = now_ms.saturating_add(50);
            self.interactions(input, now_ms)?;
        }
        let target = Tick(
            self.target
                .0
                .max(
                    self.connection
                        .estimated_server_tick(now_ms)
                        .0
                        .saturating_add(self.lead),
                )
                .min(self.connection.acknowledged_tick().0.saturating_add(64)),
        );
        self.connection.advance_to(target).map_err(error)?;
        self.connection.update_interest(now_ms).map_err(error)?;
        self.connection.poll(now_ms).map_err(error)?;
        Ok(changes)
    }
    fn interactions(&mut self, input: RemoteInput, now_ms: u64) -> Result<(), String> {
        if self
            .mining
            .is_some_and(|(_, _, id)| !self.connection.action_pending(id))
        {
            self.mining = None;
        }
        let player = self.connection.player().expect("sampled player");
        let mut pose = sim::prediction::PredictionState::capture(player)
            .map_err(str::to_string)?
            .to_player()
            .map_err(str::to_string)?;
        pose.yaw = input.yaw;
        pose.pitch = input.pitch;
        let space = self.connection.welcome().expect("joined").player.space;
        let query = RemoteQuery {
            connection: &self.connection,
            space,
        };
        let hit = match sim::raycast(
            &query,
            pose.eye(1.0),
            pose.look(),
            if pose.mode == sim::GameMode::Creative || pose.noclip {
                5.0
            } else {
                4.5
            },
        ) {
            RayResult::Hit(hit) => Some(hit),
            _ => None,
        };
        let target = hit.map(|h| {
            (
                BlockPos {
                    space,
                    x: h.cell[0],
                    y: h.cell[1],
                    z: h.cell[2],
                },
                h.block,
            )
        });
        let attack = input.keys & net::InputFrame::ATTACK != 0;
        if self
            .mining
            .is_some_and(|(p, c, _)| !attack || target != Some((p, c)))
        {
            self.connection
                .aimed_action(Action::StopMining, false, now_ms, input.yaw, input.pitch)
                .map_err(error)?;
            self.mining = None;
        }
        if attack
            && self.mining.is_none()
            && let Some((position, expected)) = target
        {
            let id = self
                .connection
                .aimed_action(
                    Action::Mine {
                        position,
                        expected,
                        tool: 0,
                    },
                    false,
                    now_ms,
                    input.yaw,
                    input.pitch,
                )
                .map_err(error)?;
            self.mining = Some((position, expected, id));
        }
        if input.placing
            && now_ms >= self.next_place_ms
            && let Some(hit) = hit
        {
            let Some(inventory) = self.connection.predicted_inventory() else {
                return Ok(());
            };
            let hand = inventory.hand();
            let rules::items::ItemKind::Block(block) = rules::items::idef(hand.item()).kind else {
                return Ok(());
            };
            if hand.is_empty() {
                return Ok(());
            }
            let offset = rules::cell::DIRS[usize::from(hit.face)];
            let position = BlockPos {
                space,
                x: hit.cell[0] + offset[0],
                y: hit.cell[1] + offset[1],
                z: hit.cell[2] + offset[2],
            };
            if let ReplicaRead::Known(expected) = self.connection.read(position) {
                self.connection
                    .aimed_action(
                        Action::Place {
                            position,
                            expected,
                            cell: Cell::from_parts(block, 0),
                        },
                        true,
                        now_ms,
                        input.yaw,
                        input.pitch,
                    )
                    .map_err(error)?;
                self.next_place_ms = now_ms.saturating_add(200);
            }
        }
        Ok(())
    }
}
fn error(error: NetworkError) -> String {
    format!("network authority: {error:?}")
}
struct RemoteQuery<'a> {
    connection: &'a Connection,
    space: core_types::SpaceId,
}
impl QueryWorld for RemoteQuery<'_> {
    fn cell(&self, p: [i32; 3]) -> CellRead {
        match self.connection.read(BlockPos {
            space: self.space,
            x: p[0],
            y: p[1],
            z: p[2],
        }) {
            ReplicaRead::Known(c) => CellRead::Known(c),
            ReplicaRead::Unknown => CellRead::Unknown,
        }
    }
}
