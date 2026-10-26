//! Authoritative use-on-block rules from the approved donor game/mek_tool.rs.
//! Hit geometry comes from the server ray; immutable item facts and every
//! affected World/component/item row share the action's durable boundary.
use super::*;
use core_types::ChannelMask;
use rules::{
    blocks::{BeKind, MekKind},
    item_payload::ConfiguratorMode,
    machines::{Face, Machine, Slot, sides},
    stack::ItemStack,
};

pub(crate) struct BlockTool {
    pub id: ActionId,
    pub position: BlockPos,
    pub expected: Cell,
    pub sneaking: bool,
    pub inventory: net::InventoryGuard,
}
fn transmitter(id: u16) -> bool {
    matches!(
        id,
        rules::ids::UNIVERSAL_CABLE
            | rules::ids::LOGISTICAL_TRANSPORTER
            | rules::ids::MECHANICAL_PIPE
            | rules::ids::PRESSURIZED_TUBE
    )
}
fn upgrade(item: u16) -> Option<usize> {
    match item {
        rules::items::I_UPGRADE_SPEED => Some(0),
        rules::items::I_UPGRADE_ENERGY => Some(1),
        rules::items::I_UPGRADE_GAS => Some(2),
        _ => None,
    }
}
fn arm(cell: Cell, hit: sim::BlockHit) -> Option<usize> {
    let (lo, hi) = if rules::blocks::bdef(cell.id()).shape == rules::blocks::Shape::Large {
        (4., 12.)
    } else {
        (5., 11.)
    };
    for (axis, face) in [(1, 0), (2, 2), (0, 4)] {
        let local = (hit.point[axis] - f64::from(hit.cell[axis])) * 16.;
        if local < lo - 1e-4 {
            return Some(face);
        }
        if local > hi + 1e-4 {
            return Some(face + 1);
        }
    }
    None
}
fn clockwise(face: u8) -> u8 {
    match face {
        2 => 5,
        5 => 3,
        3 => 4,
        _ => 2,
    }
}
fn guard(peer: &Peer, g: net::InventoryGuard) -> Result<(), RejectReason> {
    let hand = peer.inventory.hand();
    if g.revision != peer.inventory.revision()
        || usize::from(g.selected) != peer.inventory.selected()
        || g.item != hand.item()
        || g.count != hand.count()
        || g.damage != hand.damage()
        || g.payload != hand.payload().map_or(0, |p| p.0)
    {
        Err(RejectReason::Stale)
    } else {
        Ok(())
    }
}
fn known(world: &World, p: BlockPos) -> Result<Cell, RejectReason> {
    match world.read(p) {
        CellRead::Known(c) => Ok(c),
        CellRead::Unknown => Err(RejectReason::Unknown),
    }
}
impl Server {
    /// One bounded connector microbatch can precede each ordinary tick. Its
    /// ACK persists the config before resources observe it; a crash between
    /// this boundary and the following tick cannot omit any recipe step.
    pub(crate) fn finish_block_tool_boundary(&mut self) -> Result<bool, ServerError> {
        if self.tool_boundary_tick == Some(self.tick) {
            return Ok(false);
        }
        let candidate = self.peers.iter().find_map(|(&connection, peer)| {
            if !peer.ready || self.identities[&peer.identity].banned {
                return None;
            }
            let mut id = peer.next_action_to_start;
            while self
                .dedup
                .get(&peer.identity)
                .is_some_and(|d| d.replies.iter().any(|r| r.id.0 == id))
            {
                id = id.checked_add(1)?;
            }
            match peer.actions.get(&ActionId(id)) {
                Some((Action::UseBlockTool { expected, .. }, _)) if transmitter(expected.id()) => {
                    Some((connection, ActionId(id)))
                }
                _ => None,
            }
        });
        let Some((connection, id)) = candidate else {
            return Ok(false);
        };
        let mut peer = self.peers.remove(&connection).expect("selected live peer");
        let role = self.identities[&peer.identity].role;
        if !role.can_admin() && peer.player.mode != GameMode::Survival {
            peer.player.set_mode(GameMode::Survival);
        }
        if !(self.config.server_debug && role.can_admin()) {
            peer.player.revoke_debug_permission();
        }
        let (action, aim) = peer.actions.remove(&id).expect("selected queued action");
        peer.next_action_to_start = id.0.saturating_add(1);
        if let Some([yaw, pitch]) = aim {
            peer.player.yaw = yaw.rem_euclid(360.);
            peer.player.pitch = pitch;
        }
        let mut batch = Batch::empty(self.tick);
        let mut deferred = Vec::new();
        self.tool_boundary_tick = Some(self.tick);
        self.tool_boundary_active = true;
        self.perform_action(connection, &mut peer, id, action, &mut batch, &mut deferred);
        self.tool_boundary_active = false;
        let saved = runtime::capture_peer(&peer).map_err(ServerError::Invalid)?;
        self.saved_players.insert(peer.identity, saved.clone());
        batch.players.push(saved);
        self.peers.insert(connection, peer);
        self.submit_batch(batch, deferred);
        Ok(true)
    }
    pub(crate) fn use_block_tool(
        &mut self,
        peer: &mut Peer,
        request: BlockTool,
        batch: &mut Batch,
    ) -> Result<Option<TxId>, RejectReason> {
        if !peer.health.alive() || peer.player.mode == GameMode::Spectator {
            return Err(RejectReason::Unauthorized);
        }
        guard(peer, request.inventory)?;
        if self.tick.0 < peer.place_ready_at.0 {
            return Err(RejectReason::RateLimited);
        }
        let role = self.identities[&peer.identity].role;
        self.precheck_edit(peer, request.position, None, role)
            .map_err(edit_reason)?;
        let reach = if peer.player.mode == GameMode::Creative || peer.player.noclip {
            5.
        } else {
            4.5
        };
        let sim::RayResult::Hit(hit) = sim::raycast(
            &WorldQuery::new(&self.world, peer.space),
            peer.player.eye(1.),
            peer.player.look(),
            reach,
        ) else {
            return Err(RejectReason::Occluded);
        };
        if hit.cell != [request.position.x, request.position.y, request.position.z] {
            return Err(RejectReason::Occluded);
        }
        if hit.block != request.expected {
            return Err(RejectReason::Stale);
        }
        let xyz = rules::blocks::main_of(hit.block.raw(), hit.cell);
        let anchor = BlockPos {
            space: peer.space,
            x: xyz[0],
            y: xyz[1],
            z: xyz[2],
        };
        if self.protected(xyz, role) && peer.space == SpaceId(0) {
            return Err(RejectReason::Protected);
        }
        let cell = known(&self.world, anchor)?;
        let hand = peer.inventory.hand();
        let pending = self.payload_delta(batch)?;
        let view = pending
            .as_ref()
            .map(|d| self.runtimes.payloads.view(d))
            .transpose()
            .map_err(|_| RejectReason::InvalidCell)?;
        let resolver: &dyn rules::item_payload::Resolver = match view.as_ref() {
            Some(v) => v,
            None => &self.runtimes.payloads,
        };
        let mode = if hand.item() == rules::items::I_CONFIGURATOR {
            Some(
                rules::item_payload::resolve_configurator(hand, resolver)
                    .map_err(|_| RejectReason::InvalidCell)?,
            )
        } else if upgrade(hand.item()).is_some() && hand.payload().is_none() {
            None
        } else {
            return Err(RejectReason::Unauthorized);
        };
        if transmitter(cell.id()) {
            if !self.tool_boundary_active {
                return Err(RejectReason::RateLimited);
            }
            if !request.sneaking || mode == Some(ConfiguratorMode::Wrench) {
                return Err(RejectReason::Unsupported);
            }
            if mode.is_none() {
                return Err(RejectReason::Unauthorized);
            }
            if batch
                .runtimes
                .iter()
                .any(|r| r.kind == save::RUNTIME_MACHINES)
            {
                return Err(RejectReason::Capacity);
            }
            let mut config = self.runtimes.machines.transmitter_config(anchor).unwrap_or(
                machines::TransmitterConfig {
                    position: anchor,
                    block: cell.id(),
                    modes: 0,
                    color: 0,
                },
            );
            let side = arm(cell, hit);
            if side.is_none() && cell.id() == rules::ids::LOGISTICAL_TRANSPORTER {
                config.color = (config.color + 1) % 19;
            } else {
                let side = side.unwrap_or(usize::from(hit.face));
                let shift = 2 * side;
                let next = ((config.modes >> shift) + 1) & 3;
                config.modes = (config.modes & !(3 << shift)) | (next << shift);
            }
            let plan = self
                .runtimes
                .machines
                .plan_connections(&self.world, config, self.tick, machines::Budget::default())
                .map_err(|e| match e {
                    machines::MachineError::Unknown => RejectReason::Unknown,
                    machines::MachineError::Bounds => RejectReason::Capacity,
                    _ => RejectReason::InvalidCell,
                })?;
            self.runtimes
                .machines
                .validate(&self.world, &plan)
                .map_err(|_| RejectReason::Stale)?;
            let bytes = plan.delta.encode().map_err(|_| RejectReason::Capacity)?;
            let mut last = None;
            for tx in plan.transactions {
                let change = self.world.commit(tx).map_err(|_| RejectReason::Stale)?;
                last = Some(change.tx);
                batch.changes.push(change);
            }
            batch.runtimes.push(save::RuntimeRecord {
                kind: save::RUNTIME_MACHINES,
                payload: bytes,
            });
            peer.player.swing_hand();
            peer.place_ready_at = Tick(self.tick.0.saturating_add(4));
            return Ok(last);
        }
        let Some(BeKind::Mek(kind)) = rules::blocks::bdef(cell.id()).be else {
            return Err(RejectReason::Unsupported);
        };
        if !request.sneaking && mode != Some(ConfiguratorMode::Rotate) {
            self.window_action(
                peer,
                request.id,
                net::window::WindowIntent::OpenBlock {
                    position: request.position,
                    expected: request.expected,
                },
                batch,
            )?;
            return Ok(None);
        }
        let component = self
            .world
            .page(anchor.page())
            .ok_or(RejectReason::Unknown)?
            .component(anchor.local_index());
        let (component, mut machine) = match component {
            Some(id) => {
                let r = self.world.component(id).ok_or(RejectReason::InvalidCell)?;
                if r.data.anchor != anchor || r.data.kind != machines::MACHINE_COMPONENT {
                    return Err(RejectReason::InvalidCell);
                }
                let value =
                    Machine::decode(&r.data.payload).map_err(|_| RejectReason::InvalidCell)?;
                if value.kind != kind {
                    return Err(RejectReason::InvalidCell);
                }
                (id, value)
            }
            None => (
                windows::fresh_component(&self.world, anchor, machines::MACHINE_COMPONENT)
                    .map_err(|_| RejectReason::Capacity)?,
                Machine::new(kind),
            ),
        };
        let mut tx = Transaction::new(self.tick, peer.space);
        let mut inventory = peer.inventory;
        let mut payloads = Vec::new();
        let mut drops = Vec::new();
        let mut write_machine = true;
        let mut channels = ChannelMask::CONTENTS;
        let face = Face::ALL[usize::from(hit.face)];
        match mode {
            None => {
                let amount = machine.install(
                    upgrade(hand.item()).expect("checked upgrade"),
                    hand.count().min(255) as u8,
                );
                if amount > 0 && peer.player.mode != GameMode::Creative {
                    inventory
                        .set_trusted(
                            inventory.selected(),
                            if u16::from(amount) == hand.count() {
                                ItemStack::EMPTY
                            } else {
                                hand.with_count(hand.count() - u16::from(amount))
                                    .map_err(|_| RejectReason::InvalidCell)?
                            },
                        )
                        .map_err(|_| RejectReason::Capacity)?;
                }
            }
            Some(ConfiguratorMode::Items) => {
                let relative = sides::relative(
                    Face::ALL[rules::blocks::facing_of(cell.raw()) as usize],
                    face,
                ) as usize;
                machine.sides[relative] = sides::cycle(kind, machine.sides[relative], false);
                channels |= ChannelMask::CONNECTIONS;
            }
            Some(ConfiguratorMode::Gas) => {
                if kind != MekKind::Separator {
                    return Ok(None);
                }
                let relative = sides::relative(
                    Face::ALL[rules::blocks::facing_of(cell.raw()) as usize],
                    face,
                ) as usize;
                machine.gas_sides[relative] = (machine.gas_sides[relative] + 1) % 3;
                channels |= ChannelMask::CONNECTIONS;
            }
            Some(ConfiguratorMode::Rotate) => {
                let facing = rules::blocks::facing_of(cell.raw());
                let to = if request.sneaking {
                    hit.face ^ 1
                } else {
                    clockwise(facing)
                };
                if (2..=5).contains(&to) && to != facing {
                    tx.set_cell(
                        anchor,
                        Cell::from_parts(cell.id(), (cell.state() & !7) | u16::from(to)),
                    );
                }
                write_machine = false;
            }
            Some(ConfiguratorMode::Empty) => {
                let out = sides::offset(xyz, face).ok_or(RejectReason::OutOfReach)?;
                for slot in machine
                    .slots
                    .iter_mut()
                    .take(rules::machines::roles(kind).len())
                {
                    let (stack, payload) = slot.to_item().map_err(|_| RejectReason::InvalidCell)?;
                    if !stack.is_empty() {
                        drops.push((stack, Some(out)));
                    }
                    if let Some(payload) = payload {
                        payloads.push(payload);
                    }
                    *slot = Slot::default();
                }
            }
            Some(ConfiguratorMode::Wrench) => {
                let contents = block_loot::prepare_for_break(&self.world, anchor, true)
                    .map_err(|_| RejectReason::InvalidCell)?;
                let stack = if contents.replace_block_drop {
                    *contents.stacks.first().ok_or(RejectReason::InvalidCell)?
                } else {
                    ItemStack::new(rules::items::block_item(cell.id()), 1)
                        .map_err(|_| RejectReason::InvalidCell)?
                };
                payloads.extend(contents.payloads);
                let pickup = inventory
                    .plan_pickup(stack)
                    .map_err(|_| RejectReason::Capacity)?;
                let remainder = pickup.remainder();
                inventory = *pickup.after();
                if !remainder.is_empty() {
                    drops.push((remainder, None));
                }
                tx.set_cell(anchor, Cell::AIR);
                for dy in rules::blocks::multiblock_parts(cell.id()) {
                    let part = BlockPos {
                        y: anchor.y.checked_add(dy).ok_or(RejectReason::OutOfReach)?,
                        ..anchor
                    };
                    if known(&self.world, part)?.id() == rules::ids::BOUNDING {
                        tx.set_cell(part, Cell::AIR);
                    }
                }
                if let Some(id) = contents.remove {
                    tx.remove_component(id, ChannelMask::CONTENTS | ChannelMask::CONNECTIONS);
                }
                write_machine = false;
            }
        }
        if write_machine {
            tx.set_component(
                component,
                machines::machine_component(anchor, &machine),
                channels,
            );
        }
        // Both bounded ledgers admit the entire result before any owner is changed.
        let payload_record = self.plan_payloads(batch, payloads)?;
        let item_record = self.plan_tool_drops(peer, request.id, drops, batch)?;
        let change = self.world.commit(tx).map_err(|_| RejectReason::Stale)?;
        let id = change.tx;
        batch.changes.push(change);
        Self::stage_payloads(batch, payload_record);
        if let Some(record) = item_record {
            if let Some(old) = batch
                .runtimes
                .iter_mut()
                .find(|r| r.kind == save::RUNTIME_ITEMS)
            {
                *old = record;
            } else {
                batch.runtimes.push(record);
            }
        }
        peer.inventory = inventory;
        peer.player.swing_hand();
        peer.place_ready_at = Tick(self.tick.0.saturating_add(4));
        Ok(Some(id))
    }
    fn plan_tool_drops(
        &self,
        peer: &Peer,
        action: ActionId,
        drops: Vec<(ItemStack, Option<[i32; 3]>)>,
        batch: &Batch,
    ) -> Result<Option<save::RuntimeRecord>, RejectReason> {
        if drops.is_empty() {
            return Ok(None);
        }
        let mut rows = Vec::with_capacity(drops.len());
        for (ordinal, (stack, block)) in drops.into_iter().enumerate() {
            let mut item = self
                .mob_item(
                    mob_owner::DropOrigin {
                        actor: runtime::player_entity(peer.identity),
                        action: action.0,
                        space: peer.space,
                        position: peer.player.pos,
                        delay: if block.is_some() { 10 } else { 40 },
                    },
                    ordinal,
                    stack,
                )
                .map_err(|_| RejectReason::Capacity)?;
            let mut rng = core_types::DeterministicRng::from_key(
                self.drop_seed,
                0x544f4f4c44524f50,
                item.id.0,
                0,
            );
            if let Some(block) = block {
                let mut unit = || f64::from(rules::survival::loot::unit(&mut rng));
                item.position = [
                    f64::from(block[0]) + 0.25 + unit() * 0.5,
                    f64::from(block[1]) + 0.125 + unit() * 0.5,
                    f64::from(block[2]) + 0.25 + unit() * 0.5,
                ];
            } else {
                item.position = peer.player.eye(1.);
                item.position[1] -= f64::from(0.3_f32);
                item.thrown = true;
                let unit =
                    |rng: &mut core_types::DeterministicRng| rules::survival::loot::unit(rng);
                let rad = std::f32::consts::PI / 180.;
                let (g, h) = (
                    libm::sinf(peer.player.pitch * rad),
                    libm::cosf(peer.player.pitch * rad),
                );
                let (i, j) = (
                    libm::sinf(peer.player.yaw * rad),
                    libm::cosf(peer.player.yaw * rad),
                );
                let k = unit(&mut rng) * std::f32::consts::TAU;
                let l = 0.02 * unit(&mut rng);
                let (r1, r2) = (unit(&mut rng), unit(&mut rng));
                item.velocity = [
                    f64::from(-i * h * 0.3) + libm::cos(f64::from(k)) * f64::from(l),
                    f64::from(-g * 0.3 + 0.1 + (r1 - r2) * 0.1),
                    f64::from(j * h * 0.3) + libm::sin(f64::from(k)) * f64::from(l),
                ];
            }
            item.previous = item.position;
            rows.push(item);
        }
        let delta = batch
            .runtimes
            .iter()
            .find(|r| r.kind == save::RUNTIME_ITEMS)
            .map(|r| entities::items::ItemDelta::decode(&r.payload, 8192))
            .transpose()
            .map_err(|_| RejectReason::InvalidCell)?
            .map_or_else(|| self.runtimes.items.plan_activity(&[]), Ok)
            .map_err(|_| RejectReason::Capacity)?;
        let delta = self
            .runtimes
            .items
            .extend_spawn(delta, rows, true)
            .map_err(|e| {
                if e == rules::entities::EntityError::Capacity {
                    RejectReason::Capacity
                } else {
                    RejectReason::InvalidCell
                }
            })?;
        Ok(Some(save::RuntimeRecord {
            kind: save::RUNTIME_ITEMS,
            payload: delta.encode().map_err(|_| RejectReason::Capacity)?,
        }))
    }
}
