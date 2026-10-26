//! A durable reply owns the audible block effect. Retries, rejection and
//! prediction correction cannot duplicate it or invent a successful action.
use crate::{
    audio_bridge::block_event,
    network::{Connection, audio::PendingBlockSound},
};
use net::{ActionReply, ActionStatus, PlayerId};
const CAPACITY: usize = 128;
#[derive(Clone, Copy, Debug)]
pub struct BlockCue {
    pub event: &'static str,
    pub gain: f32,
    pub position: [f64; 3],
    pub emitter: audio::EmitterId,
    pub seed: u64,
}
pub struct ConfirmedBlockSounds {
    session: Option<(u64, PlayerId)>,
    pending: Vec<PendingBlockSound>,
    played: u64,
    overflow: u64,
}
impl Default for ConfirmedBlockSounds {
    fn default() -> Self {
        Self {
            session: None,
            pending: Vec::with_capacity(CAPACITY),
            played: 0,
            overflow: 0,
        }
    }
}
impl ConfirmedBlockSounds {
    pub fn observe(&mut self, connection: &Connection) {
        let session = connection
            .welcome()
            .map(|welcome| (welcome.session, welcome.identity));
        if session != self.session {
            self.pending.clear();
            self.session = session;
        }
        for action in connection.pending_block_sounds() {
            if self.pending.iter().any(|old| old.id == action.id) {
                continue;
            }
            if self.pending.len() == CAPACITY {
                self.overflow = self.overflow.saturating_add(1);
                continue;
            }
            if block_event(action.cell, action.action).is_some() {
                self.pending.push(action);
            }
        }
    }
    pub fn clear(&mut self) {
        self.pending.clear();
        self.session = None;
    }
    pub fn played(&self) -> u64 {
        self.played
    }
    pub fn overflow(&self) -> u64 {
        self.overflow
    }
    pub fn reply(&mut self, reply: ActionReply) -> Option<BlockCue> {
        if reply.status == ActionStatus::Pending {
            return None;
        }
        let index = self.pending.iter().position(|old| old.id == reply.id)?;
        let action = self.pending.swap_remove(index);
        if reply.status != ActionStatus::Accepted {
            return None;
        }
        let (event, gain) = block_event(action.cell, action.action)?;
        self.played = self.played.saturating_add(1);
        Some(BlockCue {
            event,
            gain,
            position: [
                f64::from(action.position.x) + 0.5,
                f64::from(action.position.y) + 0.5,
                f64::from(action.position.z) + 0.5,
            ],
            emitter: audio::EmitterId(reply.id.0.wrapping_shl(2)),
            seed: reply.id.0 ^ self.session.map_or(0, |session| session.0),
        })
    }
}
