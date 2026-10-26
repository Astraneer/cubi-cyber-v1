//! Block-action cues share their edit transaction. Recipient witnesses stay in
//! the private WAL; the wire carries only an exact actor/action and sound family.
use super::*;
use net::audio_cue::{BlockActor, BlockMaterial, Cue, Kind};
pub(super) const EVENT: u16 = 0x8503;
const MAX_RECIPIENTS: usize = 32;
const MAX_PER_PEER: usize = 8;
const CUE_BYTES: usize = net::audio_cue::MAX_BYTES;
const RECIPIENT_BYTES: usize = 32;
pub(super) struct ActionSound {
    pub id: ActionId,
    pub position: BlockPos,
    pub breaking: bool,
}
pub(super) fn family(cell: Cell) -> Option<BlockMaterial> {
    use rules::blocks::Snd;
    Some(match rules::block(cell.id())?.sound {
        Snd::None => return None,
        Snd::Stone => BlockMaterial::Stone,
        Snd::Wood => BlockMaterial::Wood,
        Snd::Gravel => BlockMaterial::Gravel,
        Snd::Grass => BlockMaterial::Grass,
        Snd::Sand => BlockMaterial::Sand,
        Snd::Snow => BlockMaterial::Snow,
        Snd::Wool => BlockMaterial::Wool,
        Snd::Glass => BlockMaterial::Glass,
        Snd::Metal => BlockMaterial::Metal,
        Snd::Ladder => BlockMaterial::Ladder,
    })
}
fn near(peer: &Peer, position: BlockPos) -> bool {
    peer.ready
        && peer.space == position.space
        && [position.x, position.y, position.z]
            .iter()
            .enumerate()
            .map(|(i, &p)| (peer.player.pos[i] - f64::from(p) - 0.5).powi(2))
            .sum::<f64>()
            <= 32. * 32.
}
fn visible(world: &World, peer: &Peer, position: BlockPos) -> (bool, u64, u64) {
    let reads = std::cell::Cell::new(0u64);
    let query = |p: [i32; 3]| {
        reads.set(reads.get() + 1);
        world.read(BlockPos {
            space: position.space,
            x: p[0],
            y: p[1],
            z: p[2],
        })
    };
    let mut work = visibility::ViewWork::default();
    let result = visibility::target_visible(&query, position, Some(peer.player.eye(1.)), &mut work);
    (result, work.segments, reads.get())
}
impl Server {
    /// The caller has completed the real edit guards. Own visibility is that
    /// validated action witness, independent of asynchronous Raw ACK timing.
    pub(super) fn stage_block_audio(
        &mut self,
        batch: &Batch,
        tx: &mut Transaction,
        actor: &Peer,
        action: ActionSound,
    ) {
        let CellRead::Known(before) = self.world.read(action.position) else {
            return;
        };
        let after = tx
            .cell_updates()
            .find_map(|(p, c)| (p == action.position).then_some(c));
        let Some(after) = after else {
            return;
        };
        if !actor.ready
            || action.id.0 == 0
            || before == after
            || if action.breaking {
                before == Cell::AIR || after != Cell::AIR
            } else {
                after == Cell::AIR
            }
        {
            return;
        }
        let Some(material) = family(if action.breaking { before } else { after }) else {
            return;
        };
        let count = batch
            .changes
            .iter()
            .flat_map(|c| &c.events)
            .filter(|e| {
                matches!(
                    e.kind,
                    audio_cue::EGG_EVENT | audio_cue::OWNED_EVENT | EVENT
                )
            })
            .count();
        if count >= audio_cue::MAX_EVENTS {
            self.counters.audio_cues_clipped += 1;
            return;
        }
        let block = BlockActor {
            player: actor.identity,
            material,
        };
        let cue = Cue {
            kind: if action.breaking {
                Kind::BlockBreak(block)
            } else {
                Kind::BlockPlace(block)
            },
            actor: net::player_entity_id(actor.identity),
            sequence: action.id.0,
            tick: self.tick,
            space: action.position.space,
            position: [action.position.x, action.position.y, action.position.z]
                .map(|v| f64::from(v) + 0.5),
        };
        let Ok(encoded) = cue.encode() else {
            self.counters.audio_cues_clipped += 1;
            return;
        };
        let mut recipients = Vec::with_capacity(MAX_RECIPIENTS);
        recipients.push((actor.identity, actor.session, 0));
        for peer in self.peers.values() {
            if peer.identity == actor.identity
                || !near(peer, action.position)
                || !peer.pages.contains(&action.position.page())
            {
                continue;
            }
            let Some(image) = peer.history.confirmed_page(action.position.page()) else {
                continue;
            };
            if !peer.raw_view.validates(
                action.position.page(),
                image.version,
                self.raw_sources.source(action.position.page()),
            ) || image.cells[usize::from(action.position.local_index().get())] != before
            {
                continue;
            }
            if recipients.len() == MAX_RECIPIENTS {
                self.counters.audio_cues_clipped += 1;
                continue;
            }
            let (allowed, segments, reads) = visible(&self.world, peer, action.position);
            self.counters.block_audio_rays += 1;
            self.counters.block_audio_segments += segments;
            self.counters.block_audio_cell_reads += reads;
            if allowed {
                recipients.push((peer.identity, peer.session, image.version));
            }
        }
        let mut payload = Vec::with_capacity(1 + CUE_BYTES + RECIPIENT_BYTES * recipients.len());
        payload.push(recipients.len() as u8);
        payload.extend_from_slice(&encoded);
        for (identity, session, view) in recipients {
            payload.extend_from_slice(&identity.0);
            payload.extend_from_slice(&session.to_le_bytes());
            payload.extend_from_slice(&view.to_le_bytes());
        }
        tx.event(world::GameplayEvent {
            kind: EVENT,
            subject: None,
            position: Some(action.position),
            payload,
        });
    }
    /// Called only by finish_durability. Never reconstruct cues from generic
    /// World changes, inventory differences, a reconnect or recovered history.
    pub(super) fn publish_block_audio(
        &mut self,
        changes: &[WorldChange],
        output: &mut Vec<Outgoing>,
    ) {
        let remaining = audio_cue::MAX_EVENTS.saturating_sub(
            changes
                .iter()
                .flat_map(|c| &c.events)
                .filter(|e| matches!(e.kind, audio_cue::EGG_EVENT | audio_cue::OWNED_EVENT))
                .count(),
        );
        let mut cues = Vec::new();
        let mut seen = BTreeSet::new();
        for (change, event) in changes
            .iter()
            .flat_map(|c| c.events.iter().map(move |e| (c, e)))
            .filter(|(_, e)| e.kind == EVENT)
        {
            let Some((position, cue, recipients)) = decode_event(change, event) else {
                continue;
            };
            let block = cue.kind.block().expect("validated block cue");
            if !seen.insert((block.player, cue.sequence, cue.kind.tag())) {
                continue;
            }
            if cues.len() == remaining {
                self.counters.audio_cues_clipped += 1;
                continue;
            }
            cues.push((position, cue, recipients));
        }
        if cues.is_empty() {
            return;
        }
        for (connection, peer) in self.peers.iter().filter(|(_, p)| p.ready) {
            let mut sent = output
                .iter()
                .filter(|o| {
                    o.connection == *connection && matches!(o.message, ServerMessage::AudioCue(_))
                })
                .count();
            for &(position, cue, recipients) in &cues {
                let recipient = recipients.chunks_exact(RECIPIENT_BYTES).find(|r| {
                    r[..16] == peer.identity.0 && r[16..24] == peer.session.to_le_bytes()
                });
                let Some(recipient) = recipient else {
                    continue;
                };
                if !near(peer, position) {
                    continue;
                }
                if sent >= MAX_PER_PEER {
                    self.counters.audio_cues_clipped += 1;
                    continue;
                }
                let actor = cue.kind.block().expect("validated block cue").player;
                if actor != peer.identity {
                    let view =
                        u64::from_le_bytes(recipient[24..32].try_into().expect("fixed recipient"));
                    if !peer.pages.contains(&position.page())
                        || peer
                            .history
                            .confirmed_page(position.page())
                            .is_none_or(|p| p.version != view)
                    {
                        continue;
                    }
                }
                let (allowed, segments, reads) = visible(&self.world, peer, position);
                self.counters.block_audio_rays += 1;
                self.counters.block_audio_segments += segments;
                self.counters.block_audio_cell_reads += reads;
                if !allowed {
                    continue;
                }
                output.push(Outgoing::reliable(
                    *connection,
                    ServerMessage::AudioCue(cue),
                ));
                sent += 1;
                self.counters.audio_cues_sent += 1;
            }
        }
    }
}
fn decode_event<'a>(
    change: &WorldChange,
    event: &'a world::GameplayEvent,
) -> Option<(BlockPos, Cue, &'a [u8])> {
    let position = event.position?;
    let count = usize::from(*event.payload.first()?);
    if count == 0
        || count > MAX_RECIPIENTS
        || event.payload.len() != 1 + CUE_BYTES + count * RECIPIENT_BYTES
    {
        return None;
    }
    let cue = Cue::decode(&event.payload[1..1 + CUE_BYTES]).ok()?;
    let block = cue.kind.block()?;
    if cue.tick != change.tick
        || cue.space != change.space
        || cue.space != position.space
        || cue.position != [position.x, position.y, position.z].map(|v| f64::from(v) + 0.5)
    {
        return None;
    }
    let cell = change
        .cells
        .iter()
        .find(|c| c.page == position.page() && c.index == position.local_index())?;
    let actual = if matches!(cue.kind, Kind::BlockBreak(_)) {
        if cell.new != Cell::AIR {
            return None;
        }
        cell.old
    } else {
        cell.new
    };
    if family(actual) != Some(block.material) {
        return None;
    }
    let recipients = &event.payload[1 + CUE_BYTES..];
    let mut owners = 0;
    for (i, recipient) in recipients.chunks_exact(RECIPIENT_BYTES).enumerate() {
        let session = u64::from_le_bytes(recipient[16..24].try_into().ok()?);
        let view = u64::from_le_bytes(recipient[24..32].try_into().ok()?);
        let owner = recipient[..16] == block.player.0;
        if session == 0
            || (owner && view != 0)
            || (!owner && view == 0)
            || recipients[..i * RECIPIENT_BYTES]
                .chunks_exact(RECIPIENT_BYTES)
                .any(|old| old[..16] == recipient[..16])
        {
            return None;
        }
        owners += usize::from(owner);
    }
    if owners != 1 {
        return None;
    }
    Some((position, cue, &event.payload[1 + CUE_BYTES..]))
}
#[cfg(test)]
mod tests;
