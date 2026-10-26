//! Initial game admission uses two actually completed submissions. CPU scene
//! readiness only qualifies a real HDR submission; clear/cache frames cannot.
/// Opening the same Death/Game screen deliberately bumps UiApp's revision.
/// A temporary Loading paint must never satisfy UiLayer's cached upload stamp.
pub fn prepare_game_ui(app: &mut ui::app::UiApp) {
    let target = match app.screen {
        ui::app::Screen::Loading | ui::app::Screen::Game => ui::app::Screen::Game,
        ui::app::Screen::Death => ui::app::Screen::Death,
        _ => return,
    };
    app.set_status("");
    app.open(target);
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    World,
    WorldPending(u64),
    GameUi(u64),
    GameUiPending { world: u64, ui: u64 },
    Ready,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    PrepareGameUi,
    AdmitInput,
}
#[derive(Clone, Copy, Debug)]
pub struct PresentationGate {
    phase: Phase,
    last_submitted: u64,
    floor: u64,
}
impl Default for PresentationGate {
    fn default() -> Self {
        Self {
            phase: Phase::World,
            last_submitted: 0,
            floor: 0,
        }
    }
}
impl PresentationGate {
    /// IDs remain monotone across reconnects. Retired work at/below this floor
    /// can finish later, but cannot qualify the new world's presentation.
    pub fn reset(&mut self) {
        self.floor = self.last_submitted;
        self.phase = Phase::World;
    }
    /// Called only after actual queue submission AND surface presentation.
    /// `world` requires an encoded HDR scene with the normal source/content
    /// readiness; `game_ui` requires cached HDR plus the prepared Game HUD.
    pub fn submitted(&mut self, id: u64, world: bool, game_ui: bool) {
        if id <= self.last_submitted {
            return;
        }
        self.last_submitted = id;
        match self.phase {
            Phase::World if world && id > self.floor => self.phase = Phase::WorldPending(id),
            Phase::GameUi(first) if game_ui && id > first => {
                self.phase = Phase::GameUiPending {
                    world: first,
                    ui: id,
                };
            }
            _ => {}
        }
    }
    pub fn completed(&mut self, id: u64) -> Option<Event> {
        match self.phase {
            Phase::WorldPending(first) if id >= first => {
                self.phase = Phase::GameUi(first);
                Some(Event::PrepareGameUi)
            }
            Phase::GameUiPending { ui, .. } if id >= ui => {
                self.phase = Phase::Ready;
                Some(Event::AdmitInput)
            }
            _ => None,
        }
    }
    pub fn needs_game_ui(&self) -> bool {
        matches!(self.phase, Phase::GameUi(_) | Phase::GameUiPending { .. })
    }
    pub fn input_admitted(&self) -> bool {
        self.phase == Phase::Ready
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cold_hdr_and_cached_game_hud_complete_before_any_input() {
        let mut gate = PresentationGate::default();
        for id in 1..100 {
            gate.submitted(id, false, false); // real Loading clear frames
            assert_eq!(gate.completed(id), None);
            assert!(!gate.input_admitted());
        }
        gate.submitted(100, true, false);
        for _ in 0..1000 {
            // CPU continuations during the cold GPU frame
            assert_eq!(gate.completed(99), None);
            assert!(!gate.needs_game_ui());
        }
        assert_eq!(gate.completed(100), Some(Event::PrepareGameUi));
        assert!(gate.needs_game_ui());
        gate.submitted(101, false, false); // a new scene cannot replace cached-HUD proof
        assert_eq!(gate.completed(101), None);
        gate.submitted(102, false, true);
        assert!(!gate.input_admitted());
        assert_eq!(gate.completed(101), None);
        assert_eq!(gate.completed(102), Some(Event::AdmitInput));
        assert!(gate.input_admitted());
        assert_eq!(gate.completed(102), None);
    }
    #[test]
    fn reset_rejects_retired_callbacks_and_reused_submission_ids() {
        let mut gate = PresentationGate::default();
        gate.submitted(10, true, false);
        gate.reset();
        assert_eq!(gate.completed(10), None);
        gate.submitted(10, true, true);
        assert_eq!(gate.completed(10), None);
        gate.submitted(11, true, false);
        assert_eq!(gate.completed(10), None);
        assert_eq!(gate.completed(11), Some(Event::PrepareGameUi));
        gate.submitted(12, false, true);
        gate.reset();
        assert_eq!(gate.completed(12), None);
        gate.submitted(13, false, true);
        assert_eq!(gate.completed(13), None);
        assert!(!gate.input_admitted());
    }
    #[test]
    fn unchanged_death_forces_the_actual_ui_upload_stamp_to_miss() {
        let mut app = ui::app::UiApp::new().unwrap();
        app.open(ui::app::Screen::Death);
        app.set_status("");
        while app.take_intent().is_some() {}
        // This is the exact revision/extent tuple used by UiLayer::needs_redraw.
        let loading_upload = (app.revision(), 780, 437);
        prepare_game_ui(&mut app);
        assert_eq!(app.screen, ui::app::Screen::Death);
        assert_ne!(loading_upload, (app.revision(), 780, 437));
        assert_eq!(
            app.take_intent(),
            Some(ui::app::Intent::PointerCapture(false))
        );
        assert!(app.take_intent().is_none());
    }
}
