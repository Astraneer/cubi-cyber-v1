//! Causal cues share the gameplay WAL batch and publish only after fsync.
use super::*;
use net::audio_cue::{Cue, Kind};
pub(crate) const EGG_EVENT: u16 = 0x8501;
const OWNED_EVENT: u16 = 0x8502;
pub(crate) const MAX_EVENTS: usize = 64;
const MAX_PER_PEER: usize = 8;
/// The recipient is private WAL routing data, never part of the public cue.
pub(crate) fn owned_event(
    owner: PlayerId,
    cue: Cue,
) -> Result<world::GameplayEvent, net::audio_cue::Error> {
    let encoded = cue.encode()?;
    let mut payload = Vec::with_capacity(16 + encoded.len());
    payload.extend_from_slice(&owner.0);
    payload.extend_from_slice(&encoded);
    Ok(world::GameplayEvent {
        kind: OWNED_EVENT,
        subject: None,
        position: Some(BlockPos {
            space: cue.space,
            x: cue.position[0].floor() as i32,
            y: cue.position[1].floor() as i32,
            z: cue.position[2].floor() as i32,
        }),
        payload,
    })
}
pub(crate) fn attack_kind(strike: rules::entities::combat::Strike) -> Kind {
    // Approved donor server/pvp.rs::Strike::sfx, including sprint priority.
    if strike.sprint_knockback {
        Kind::AttackKnockback
    } else if strike.critical {
        Kind::AttackCrit
    } else if strike.strong {
        Kind::AttackStrong
    } else {
        Kind::AttackWeak
    }
}
pub(crate) fn egg_event(
    actor: u64,
    tick: Tick,
    space: SpaceId,
    position: [f64; 3],
) -> Result<world::GameplayEvent, rules::entities::EntityError> {
    let cue = Cue {
        kind: Kind::ChickenEgg,
        actor,
        sequence: tick.0,
        tick,
        space,
        position,
    };
    let payload = cue
        .encode()
        .map_err(|_| rules::entities::EntityError::InvalidEncoding)?
        .to_vec();
    let p = position.map(|p| p.floor() as i32);
    Ok(world::GameplayEvent {
        kind: EGG_EVENT,
        subject: None,
        position: Some(BlockPos {
            space,
            x: p[0],
            y: p[1],
            z: p[2],
        }),
        payload,
    })
}
impl Server {
    /// A transient sound must not reject gameplay which already passed every
    /// owner guard. A missing cue under pressure is counted, never inferred later.
    pub(super) fn commit_owned_audio(&mut self, batch: &mut Batch, owner: PlayerId, cue: Cue) {
        let count = batch
            .changes
            .iter()
            .flat_map(|c| &c.events)
            .filter(|e| matches!(e.kind, EGG_EVENT | OWNED_EVENT))
            .count();
        if count >= MAX_EVENTS {
            self.counters.audio_cues_clipped += 1;
            return;
        }
        let Ok(event) = owned_event(owner, cue) else {
            self.counters.audio_cues_clipped += 1;
            return;
        };
        let mut tx = Transaction::new(cue.tick, cue.space);
        tx.event(event);
        match self.world.commit(tx) {
            Ok(change) => batch.changes.push(change),
            Err(_) => self.counters.audio_cues_clipped += 1,
        }
    }
    pub(super) fn publish_audio_cues(&mut self, changes: &[WorldChange]) -> Vec<Outgoing> {
        let mut cues = Vec::new();
        let mut seen = BTreeSet::new();
        for (change, event) in changes
            .iter()
            .flat_map(|change| change.events.iter().map(move |event| (change, event)))
            .filter(|(_, event)| matches!(event.kind, EGG_EVENT | OWNED_EVENT))
        {
            let (owner, payload) = if event.kind == OWNED_EVENT {
                let Some((owner, payload)) = event.payload.split_at_checked(16) else {
                    continue;
                };
                (
                    Some(PlayerId(owner.try_into().expect("recipient length"))),
                    payload,
                )
            } else {
                (None, event.payload.as_slice())
            };
            let Ok(cue) = Cue::decode(payload) else {
                continue;
            };
            if owner.is_none() && cue.kind != Kind::ChickenEgg {
                continue;
            }
            if cue.tick != change.tick || cue.space != change.space {
                continue;
            }
            if cues.len() == MAX_EVENTS {
                self.counters.audio_cues_clipped += 1;
                continue;
            }
            if seen.insert((owner, cue.tick, cue.kind as u8, cue.actor, cue.sequence)) {
                cues.push((owner, cue));
            }
        }
        let mut output = Vec::new();
        if cues.is_empty() {
            return output;
        }
        for (connection, peer) in self.peers.iter().filter(|(_, p)| p.ready) {
            let mut sent = 0;
            for &(owner, cue) in &cues {
                if owner.is_some_and(|owner| owner != peer.identity)
                    || peer.space != cue.space
                    || (0..3)
                        .map(|i| (peer.player.pos[i] - cue.position[i]).powi(2))
                        .sum::<f64>()
                        > 32. * 32.
                {
                    continue;
                }
                if sent == MAX_PER_PEER {
                    self.counters.audio_cues_clipped += 1;
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
        output
    }
}
