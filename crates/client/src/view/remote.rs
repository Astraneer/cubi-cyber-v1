//! Узкие hooks сетевого режима. Standalone physics, camera и GPU publication
//! остаются в исходном View; этот модуль принимает состояние авторитета.
use super::{ClientView, InputKey};
use core_types::{BlockPos, SpaceId, Tick};
use sim::prediction::PredictionState;

#[derive(Clone, Copy, Debug, Default)]
pub struct RemoteInput {
    pub keys: u8,
    pub yaw: f32,
    pub pitch: f32,
    pub placing: bool,
    pub place_pressed: bool,
    pub place_sneaking: bool,
}
impl ClientView {
    pub fn authoritative_remote(&self) -> bool {
        self.scene.config().authoritative_remote
    }
    pub fn remote_input(&self) -> RemoteInput {
        let mut bits = 0;
        for (key, bit) in [
            (InputKey::Forward, net::InputFrame::FORWARD),
            (InputKey::Back, net::InputFrame::BACK),
            (InputKey::Left, net::InputFrame::LEFT),
            (InputKey::Right, net::InputFrame::RIGHT),
            (InputKey::Jump, net::InputFrame::JUMP),
            (InputKey::Sneak, net::InputFrame::SNEAK),
            (InputKey::Sprint, net::InputFrame::SPRINT),
            (InputKey::Attack, net::InputFrame::ATTACK),
        ] {
            if self.keys[key as usize] {
                bits |= bit;
            }
        }
        let [yaw, pitch] = self
            .remote_look
            .unwrap_or([self.player.yaw, self.player.pitch]);
        RemoteInput {
            keys: bits,
            yaw,
            pitch,
            placing: self.keys[InputKey::Place as usize],
            place_pressed: self.place_pressed,
            place_sneaking: self.place_sneaking,
        }
    }
    /// A short mouse click survives keyup and one transport sampling boundary.
    /// Held repetition remains an independent level-triggered input.
    pub fn take_remote_input(&mut self) -> RemoteInput {
        let input = self.remote_input();
        self.place_pressed = false;
        self.place_sneaking = false;
        input
    }
    pub fn take_remote_control(&mut self) -> Option<InputKey> {
        self.remote_controls.pop_front()
    }
    /// Direction is captured on N's rising edge, so a quick Shift+N remains
    /// backward even if both keys are released before network sampling.
    pub fn take_remote_tool_mode(&mut self) -> Option<bool> {
        self.remote_tool_modes.pop_front()
    }
    pub fn install_remote_player(
        &mut self,
        player: &sim::Player,
        tick: Tick,
    ) -> Result<(), String> {
        if !self.authoritative_remote() {
            return Err("remote player requires authoritative scene".into());
        }
        let state = PredictionState::capture(player).map_err(str::to_string)?;
        let look = self.remote_look.unwrap_or([player.yaw, player.pitch]);
        let signature = state.hash()
            ^ u64::from(look[0].to_bits()).rotate_left(7)
            ^ u64::from(look[1].to_bits()).rotate_left(39);
        state
            .apply_authoritative(&mut self.player)
            .map_err(str::to_string)?;
        self.player.self_body = player.self_body;
        self.player.yaw = look[0];
        self.player.pitch = look[1];
        self.remote_look = Some(look);
        self.tick = Tick(self.tick.0.max(tick.0));
        self.active = true;
        self.activation_requested = true;
        let position = self.player.pos.map(|v| v.floor() as i32);
        let center = BlockPos {
            space: SpaceId(0),
            x: position[0],
            y: position[1],
            z: position[2],
        };
        if self.scene.config().center.page() != center.page() {
            self.scene.set_center(center).map_err(|e| e.to_string())?;
        }
        if self.remote_signature != Some(signature) {
            self.remote_signature = Some(signature);
            self.invalidate();
        }
        Ok(())
    }
    /// Передать ровно те клеточные изменения, которые вернул authoritative
    /// import/overlay; первоначальная загрузка страницы не считается H1 edit.
    pub fn note_remote_change(&mut self, change: &world::WorldChange) -> Result<(), String> {
        if !self.authoritative_remote() {
            return Err("remote change requires authoritative scene".into());
        }
        self.note_edit_change(change)
    }
}
