//! Private window state is staged with the owning player and World transaction,
//! frozen behind the WAL boundary, then mirrored only to that owner.
use super::*;
use net::window::{WindowGuard, WindowIntent};
fn chest(bound: &windows::BoundWindow) -> Option<BlockPos> {
    (!bound.state.closed() && matches!(bound.state.kind(), ui::WindowKind::Chest(_)))
        .then(|| bound.binding.map(|b| b.anchor))
        .flatten()
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Stamp {
    guard: WindowGuard,
    component: Option<Version>,
    target: Version,
    anchor: Version,
    read_only: bool,
}
fn stamp(world: &World, peer: &Peer, showcase: Option<&spawn_showcase::Owner>) -> Stamp {
    let guard = peer.window.state.guard(&peer.inventory);
    let (component, target, anchor) =
        peer.window
            .binding
            .map_or((None, Version(0), Version(0)), |b| {
                (
                    b.component
                        .and_then(|id| world.component(id).map(|c| c.version)),
                    world.page_version(b.target.page()),
                    world.page_version(b.anchor.page()),
                )
            });
    Stamp {
        guard: WindowGuard {
            id: guard.id,
            revision: guard.revision,
            inventory_revision: guard.inventory_revision,
        },
        component,
        target,
        anchor,
        read_only: !peer.window.state.closed()
            && peer.window.binding.is_some_and(|binding| {
                showcase.is_some_and(|owner| owner.contains_window(world, binding.anchor))
            }),
    }
}
pub(super) fn initial(
    bytes: &[u8],
    inventory: &rules::survival::Inventory,
) -> Result<windows::BoundWindow, ServerError> {
    let mut bound = windows::BoundWindow::decode(bytes)
        .map_err(|_| ServerError::Invalid("private window codec"))?;
    if bound.state.id() == 0 {
        // WND1's historical default had epoch0. Migrate only the validated
        // identity field, preserving cursor/grid/container/owned items exactly.
        let mut migrated = bound.encode();
        migrated[4..12].copy_from_slice(&1u64.to_le_bytes());
        bound = windows::BoundWindow::decode(&migrated)
            .map_err(|_| ServerError::Invalid("window epoch migration"))?;
    }
    if bytes.is_empty() {
        let close = bound
            .state
            .plan_close(inventory, bound.state.guard(inventory))
            .map_err(|_| ServerError::Invalid("initial window close"))?;
        let mut unchanged = *inventory;
        close
            .apply(&mut bound.state, &mut unchanged)
            .map_err(|_| ServerError::Invalid("initial window state"))?;
    }
    Ok(bound)
}
fn reason(error: windows::Error) -> RejectReason {
    match error {
        windows::Error::Unknown => RejectReason::Unknown,
        windows::Error::Occluded => RejectReason::Occluded,
        windows::Error::OutOfReach => RejectReason::OutOfReach,
        windows::Error::Protected => RejectReason::Protected,
        windows::Error::Spectator => RejectReason::Unauthorized,
        windows::Error::Stale => RejectReason::Stale,
        windows::Error::UnsupportedTypedItem => RejectReason::Unsupported,
        windows::Error::Capacity => RejectReason::Capacity,
        windows::Error::Window(ui::WindowError::Stale) => RejectReason::Stale,
        windows::Error::Window(ui::WindowError::Capacity) => RejectReason::Capacity,
        _ => RejectReason::InvalidCell,
    }
}
impl Server {
    pub(super) fn window_action(
        &mut self,
        peer: &mut Peer,
        id: ActionId,
        intent: WindowIntent,
        batch: &mut Batch,
    ) -> Result<(), RejectReason> {
        if !peer.window.state.closed()
            && peer.window.binding.is_some()
            && !matches!(intent, WindowIntent::Close { .. })
        {
            peer.window.refresh(&self.world).map_err(reason)?;
        }
        let role = self.identities[&peer.identity].role;
        let position = match intent {
            WindowIntent::OpenBlock { position, .. } => Some(position),
            _ => peer.window.binding.map(|b| b.anchor),
        };
        let display_bound = peer
            .window
            .binding
            .is_some_and(|binding| self.showcase_window(binding.anchor));
        if display_bound
            && !matches!(
                intent,
                WindowIntent::OpenInventory
                    | WindowIntent::OpenBlock { .. }
                    | WindowIntent::Close { .. }
            )
        {
            return Err(RejectReason::Protected);
        }
        let permitted = position.is_some_and(|p| self.showcase_window(p))
            || position
                .is_none_or(|p| p.space != SpaceId(0) || !self.protected([p.x, p.y, p.z], role));
        let pending = self.payload_delta(batch)?;
        let view = pending
            .as_ref()
            .map(|delta| self.runtimes.payloads.view(delta))
            .transpose()
            .map_err(|_| RejectReason::InvalidCell)?;
        let resolver: &dyn rules::item_payload::Resolver = if let Some(view) = view.as_ref() {
            view
        } else {
            &self.runtimes.payloads
        };
        let context = windows::Context {
            player: &peer.player,
            space: peer.space,
            inventory: &peer.inventory,
            permitted,
            payloads: Some(resolver),
        };
        let fresh = if matches!(
            intent,
            WindowIntent::OpenInventory | WindowIntent::OpenBlock { .. }
        ) {
            peer.window
                .state
                .id()
                .checked_add(1)
                .ok_or(RejectReason::Capacity)?
        } else {
            peer.window.state.id()
        };
        let mut plan = windows::plan(
            &self.world,
            &context,
            &peer.window,
            intent,
            fresh,
            self.tick,
            &self.recipe_book,
        )
        .map_err(reason)?;
        plan.validate(&self.world, &peer.window, &peer.inventory)
            .map_err(reason)?;
        let after = plan.window_after().clone();
        let before_chest = chest(&peer.window);
        let after_chest = chest(&after);
        let inventory = *plan.inventory_after();
        let drops = plan.drops().to_vec();
        let payload_record = self.plan_payloads(batch, plan.take_payloads())?;
        let mut tx = plan.take_transaction();
        if !drops.is_empty() {
            let p = peer.player.pos.map(|v| v.floor() as i32);
            let anchor = BlockPos {
                space: peer.space,
                x: p[0],
                y: p[1],
                z: p[2],
            };
            let drop = survival::plan_drops(
                &self.world,
                anchor,
                peer.identity.0,
                id.0,
                self.tick,
                &drops,
                40,
                None,
            )
            .map_err(|_| RejectReason::Capacity)?;
            drop.append_to(tx.get_or_insert_with(|| Transaction::new(self.tick, peer.space)));
        }
        if let Some(tx) = tx {
            batch
                .changes
                .push(self.world.commit(tx).map_err(|_| RejectReason::Stale)?);
        }
        Self::stage_payloads(batch, payload_record);
        // Values remain private/unpublished while the owner waits for this
        // same batch's durable completion; no further action can observe them.
        peer.window = after;
        peer.inventory = inventory;
        peer.window_sent = None;
        if before_chest != after_chest {
            for (kind, position) in [
                (net::audio_cue::Kind::ChestClose, before_chest),
                (net::audio_cue::Kind::ChestOpen, after_chest),
            ] {
                if let Some(position) = position {
                    self.commit_owned_audio(
                        batch,
                        peer.identity,
                        net::audio_cue::Cue {
                            kind,
                            actor: runtime::player_entity(peer.identity).0,
                            sequence: id.0,
                            tick: self.tick,
                            space: position.space,
                            position: [position.x, position.y, position.z]
                                .map(|v| f64::from(v) + 0.5),
                        },
                    );
                }
            }
        }
        Ok(())
    }
    pub(super) fn maintain_window(
        &mut self,
        peer: &mut Peer,
        batch: &mut Batch,
    ) -> Result<(), RejectReason> {
        if peer.window.state.closed() {
            return Ok(());
        }
        let Some(binding) = peer.window.binding else {
            return Ok(());
        };
        let eye = peer.player.eye(1.0);
        let center = [
            f64::from(binding.anchor.x) + 0.5,
            f64::from(binding.anchor.y) + 0.5,
            f64::from(binding.anchor.z) + 0.5,
        ];
        let distance = eye
            .iter()
            .zip(center)
            .map(|(a, b)| (a - b) * (a - b))
            .sum::<f64>()
            .sqrt();
        let changed_space = peer.space != binding.anchor.space;
        let reach = if peer.player.mode == GameMode::Creative || peer.player.noclip {
            5.0
        } else {
            4.5
        };
        let invalid = changed_space
            || distance >= reach + 0.87
            || peer.player.health <= 0.0
            || peer.window.refresh(&self.world).is_err();
        if invalid {
            let guard = peer.window.state.guard(&peer.inventory);
            let intent = WindowIntent::Close {
                guard: WindowGuard {
                    id: guard.id,
                    revision: guard.revision,
                    inventory_revision: guard.inventory_revision,
                },
            };
            // Unknown/drop-capacity backpressure retains owned cursor/grid
            // values and retries when their destination becomes available.
            let _ = self.window_action(
                peer,
                ActionId(u64::MAX.saturating_sub(self.tick.0)),
                intent,
                batch,
            );
        }
        Ok(())
    }
    pub(super) fn publish_windows(&mut self) -> Vec<Outgoing> {
        let mut output = Vec::new();
        let showcase = self.spawn_showcase.as_ref();
        for (connection, peer) in self.peers.iter_mut().filter(|(_, p)| p.ready) {
            let current = stamp(&self.world, peer, showcase);
            if peer.window_sent == Some(current) {
                continue;
            }
            if peer.window.binding.is_some()
                && !peer.window.state.closed()
                && peer.window.refresh(&self.world).is_err()
            {
                continue;
            }
            let Ok(mut snapshot) = windows::snapshot(
                &self.world,
                &peer.window,
                &peer.inventory,
                &self.recipe_book,
            ) else {
                continue;
            };
            let published = stamp(&self.world, peer, showcase);
            snapshot.read_only = published.read_only;
            peer.window_sent = Some(published);
            output.push(Outgoing::reliable(
                *connection,
                ServerMessage::Window(Box::new(snapshot)),
            ));
        }
        output
    }
}
