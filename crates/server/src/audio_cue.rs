//! Causal egg cues are committed with the actual egg, then published after fsync.
use super::*;
use net::audio_cue::{Cue, Kind};
pub(crate) const EGG_EVENT: u16 = 0x8501;
pub(crate) const MAX_EVENTS: usize = 64;
const MAX_PER_PEER: usize = 8;
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
    pub(super) fn publish_audio_cues(&mut self, changes: &[WorldChange]) -> Vec<Outgoing> {
        let mut cues = Vec::new();
        let mut seen = BTreeSet::new();
        for (change, event) in changes
            .iter()
            .flat_map(|change| change.events.iter().map(move |event| (change, event)))
            .filter(|(_, event)| event.kind == EGG_EVENT)
        {
            let Ok(cue) = Cue::decode(&event.payload) else {
                continue;
            };
            if cue.tick != change.tick || cue.space != change.space {
                continue;
            }
            if cues.len() == MAX_EVENTS {
                self.counters.audio_cues_clipped += 1;
                continue;
            }
            if seen.insert((cue.actor, cue.sequence)) {
                cues.push(cue);
            }
        }
        let mut output = Vec::new();
        if cues.is_empty() {
            return output;
        }
        for (connection, peer) in self.peers.iter().filter(|(_, p)| p.ready) {
            let mut sent = 0;
            for &cue in &cues {
                if peer.space != cue.space
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
