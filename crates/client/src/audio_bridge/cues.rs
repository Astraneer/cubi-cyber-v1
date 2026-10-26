//! Causal public audio uses durable server events, never nearby item inference.
use super::BlockCue;
use crate::network::{Connection, audio_cues::AudioCues};
use core_types::{SpaceId, Tick};
use net::PlayerId;
const PER_PHASE: usize = 16;
const MAX_AGE: u64 = 40;
#[derive(Default)]
pub struct CausalSounds {
    session: Option<(u64, PlayerId)>,
    cursor: u64,
    pub emitted: u64,
    pub lost: u64,
}
impl CausalSounds {
    pub fn clear(&mut self) {
        self.session = None;
        self.cursor = 0;
    }
    pub fn visit(
        &mut self,
        network: &Connection,
        listener: [f64; 3],
        emit: impl FnMut(BlockCue),
    ) -> usize {
        let Some(welcome) = network.welcome() else {
            self.clear();
            return 0;
        };
        if network.player().is_none() {
            return 0;
        }
        let session = Some((welcome.session, welcome.identity));
        if self.session != session {
            self.clear();
            self.session = session;
        }
        self.visit_cache(
            network.audio_cues(),
            welcome.player.space,
            network.acknowledged_tick(),
            listener,
            emit,
        )
    }
    fn visit_cache(
        &mut self,
        cache: &AudioCues,
        space: SpaceId,
        now: Tick,
        listener: [f64; 3],
        mut emit: impl FnMut(BlockCue),
    ) -> usize {
        if self.cursor == cache.revision() || listener.iter().any(|v| !v.is_finite()) {
            return 0;
        }
        let mut played = 0;
        for entry in cache.after(self.cursor).take(PER_PHASE) {
            self.lost = self
                .lost
                .saturating_add(entry.revision.saturating_sub(self.cursor).saturating_sub(1));
            self.cursor = entry.revision;
            let cue = entry.cue;
            if cue.space != space
                || now.0.saturating_sub(cue.tick.0) > MAX_AGE
                || cue
                    .position
                    .iter()
                    .zip(listener)
                    .map(|(&a, b)| (a - b) * (a - b))
                    .sum::<f64>()
                    > 32. * 32.
            {
                continue;
            }
            let event = match cue.kind {
                net::audio_cue::Kind::ChickenEgg => "entity.chicken.egg",
                net::audio_cue::Kind::ChestOpen => "chest.open",
                net::audio_cue::Kind::ChestClose => "chest.close",
                net::audio_cue::Kind::ItemPickup => "item.pickup",
                net::audio_cue::Kind::AttackWeak => "player.attack_weak",
                net::audio_cue::Kind::AttackStrong => "player.attack_strong",
                net::audio_cue::Kind::AttackCrit => "player.attack_crit",
                net::audio_cue::Kind::AttackKnockback => "player.attack_knockback",
                net::audio_cue::Kind::BucketFill => "bucket.fill",
                net::audio_cue::Kind::BucketEmpty => "bucket.empty",
                net::audio_cue::Kind::Eat => "player.eat",
                net::audio_cue::Kind::EatDone => "player.eat_done",
                net::audio_cue::Kind::BlockBreak(block) => {
                    let Some((event, _)) =
                        super::block_material_event(block.material, super::BlockSound::Break)
                    else {
                        continue;
                    };
                    event
                }
                net::audio_cue::Kind::BlockPlace(block) => {
                    let Some((event, _)) =
                        super::block_material_event(block.material, super::BlockSound::Place)
                    else {
                        continue;
                    };
                    event
                }
            };
            emit(BlockCue {
                event,
                gain: 1.,
                position: cue.position,
                emitter: audio::EmitterId(
                    cue.actor ^ 0x4547_4700_0000_0000 ^ (u64::from(cue.kind.tag()) << 48),
                ),
                seed: cue.sequence ^ cue.actor ^ u64::from(cue.kind.tag()),
            });
            self.emitted = self.emitted.saturating_add(1);
            played += 1;
        }
        played
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use net::audio_cue::{Cue, Kind};
    fn cue(actor: u64, tick: u64) -> Cue {
        Cue {
            kind: Kind::ChickenEgg,
            actor,
            sequence: tick,
            tick: Tick(tick),
            space: SpaceId(0),
            position: [1., 2., 3.],
        }
    }
    #[test]
    fn causal_events_are_once_bounded_and_never_cross_space_or_replay_old_items() {
        let mut cache = AudioCues::default();
        let mut sounds = CausalSounds::default();
        let mut cues = Vec::new();
        assert_eq!(
            sounds.visit_cache(&cache, SpaceId(0), Tick(100), [0.; 3], |cue| cues.push(cue)),
            0
        );
        cache
            .push(Cue::decode(&cue(1, 100).encode().unwrap()).unwrap())
            .unwrap();
        assert_eq!(
            sounds.visit_cache(&cache, SpaceId(0), Tick(100), [0.; 3], |cue| cues.push(cue)),
            1
        );
        assert_eq!(cues[0].event, "entity.chicken.egg");
        assert_eq!(
            sounds.visit_cache(&cache, SpaceId(0), Tick(100), [0.; 3], |_| panic!(
                "duplicate"
            )),
            0
        );
        for actor in 2..=64 {
            cache.push(cue(actor, 100)).unwrap();
        }
        assert_eq!(
            sounds.visit_cache(&cache, SpaceId(0), Tick(100), [0.; 3], |_| {}),
            16
        );
        assert_eq!(
            sounds.visit_cache(&cache, SpaceId(1), Tick(100), [0.; 3], |_| panic!(
                "foreign space"
            )),
            0
        );
        assert_eq!(
            sounds.visit_cache(&cache, SpaceId(0), Tick(200), [0.; 3], |_| panic!("stale")),
            0
        );
        assert_eq!(
            sounds.visit_cache(&cache, SpaceId(0), Tick(200), [0.; 3], |_| panic!("stale")),
            0
        );
        cache
            .push(Cue {
                position: [100., 0., 0.],
                ..cue(1, 201)
            })
            .unwrap();
        assert_eq!(
            sounds.visit_cache(&cache, SpaceId(0), Tick(201), [0.; 3], |_| panic!(
                "out of range"
            )),
            0
        );
        cache.push(cue(1, 202)).unwrap();
        assert_eq!(
            sounds.visit_cache(&cache, SpaceId(0), Tick(202), [0.; 3], |_| {}),
            1
        );
        assert_eq!(sounds.emitted, 18);
    }
}
