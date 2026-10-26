//! Read-only, bounded presentation access to existing public prediction states.
//! No credential, private grant, inventory, or cross-space state is exposed.
use super::Connection;
use core_types::Tick;
use sim::prediction::PredictionState;
#[derive(Clone, Copy, Debug)]
pub struct PendingBlockSound {
    pub id: net::ActionId,
    pub position: core_types::BlockPos,
    pub cell: core_types::Cell,
    pub action: crate::audio_bridge::BlockSound,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioHistoryError {
    Range,
}
impl Connection {
    /// Read only the actor's public block edits, never inventory guards or other
    /// spaces. Presentation caches these before the durable final reply arrives.
    pub fn pending_block_sounds(&self) -> impl Iterator<Item = PendingBlockSound> + '_ {
        let space = self.welcome().map(|welcome| welcome.player.space);
        self.actions.iter().filter_map(move |(&id, pending)| {
            let (position, cell, action) = match &pending.action {
                net::Action::Mine {
                    position, expected, ..
                } => (*position, *expected, crate::audio_bridge::BlockSound::Break),
                net::Action::Place { position, cell, .. }
                | net::Action::PlaceOwned { position, cell, .. } => {
                    (*position, *cell, crate::audio_bridge::BlockSound::Place)
                }
                net::Action::UseBucket {
                    position,
                    expected,
                    inventory,
                } if inventory.item != rules::items::I_BUCKET
                    && expected.id() != rules::ids::AIR
                    && rules::survival::fluid::fluid(*expected).is_none() =>
                {
                    (*position, *expected, crate::audio_bridge::BlockSound::Break)
                }
                _ => return None,
            };
            (Some(position.space) == space).then_some(PendingBlockSound {
                id,
                position,
                cell,
                action,
            })
        })
    }
    /// Inclusive range of at most 64 ticks. Borrowed snapshots are never cloned
    /// or regenerated; corrected/replayed ticks can be skipped by the sound owner.
    pub fn audio_history(
        &self,
        start: Tick,
        end: Tick,
    ) -> Result<impl Iterator<Item = (Tick, &PredictionState)>, AudioHistoryError> {
        if end.0 < start.0 || end.0 - start.0 >= 64 {
            return Err(AudioHistoryError::Range);
        }
        Ok(self
            .predicted
            .range(start.0..=end.0)
            .map(|(tick, state)| (Tick(*tick), &state.state)))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_prediction_snapshots_are_borrowed_in_a_bounded_read_only_range() {
        let mut connection = Connection::new(super::super::NetworkConfig::default()).unwrap();
        for tick in 1..=5 {
            let player = sim::Player::new([tick as f64, 70.0, 0.0]);
            let state = PredictionState::capture(&player).unwrap();
            connection.predicted.insert(
                tick,
                super::super::PredictedTick {
                    state,
                    input_sequence: tick,
                },
            );
        }
        let values: Vec<_> = connection
            .audio_history(Tick(2), Tick(4))
            .unwrap()
            .map(|(tick, state)| (tick, state.to_player().unwrap().pos))
            .collect();
        assert_eq!(
            values,
            vec![
                (Tick(2), [2.0, 70.0, 0.0]),
                (Tick(3), [3.0, 70.0, 0.0]),
                (Tick(4), [4.0, 70.0, 0.0])
            ]
        );
        assert!(connection.audio_history(Tick(0), Tick(64)).is_err());
        assert!(connection.audio_history(Tick(4), Tick(3)).is_err());
        let scope = telemetry::AllocationScope::begin().unwrap();
        for _ in 0..10_000 {
            assert_eq!(
                connection.audio_history(Tick(2), Tick(4)).unwrap().count(),
                3
            );
        }
        let counts = scope.finish();
        assert_eq!(counts.allocating_calls(), 0);
        assert_eq!(counts.dealloc_calls, 0);
        assert_eq!(connection.predicted.len(), 5);
    }
}
