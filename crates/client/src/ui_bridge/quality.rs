//! Shared quality transaction adapter for native/browser presentation shells.
use crate::{
    quality::{QualityRequest, QualityRuntime},
    view::ClientView,
};
use ui::{
    app::{Screen, UiApp},
    quality::{LEVERS, QualityHud},
    settings::{Setting, Settings},
};
pub struct QualityBridge {
    pub runtime: Box<QualityRuntime>,
    saved: Settings,
    active: bool,
}
impl QualityBridge {
    pub fn new(saved: &Settings) -> Self {
        Self {
            runtime: Box::new(QualityRuntime::new(saved)),
            saved: saved.clone(),
            active: false,
        }
    }
    pub fn target(&self) -> &Settings {
        self.runtime.target()
    }
    pub fn update_saved(&mut self, saved: &Settings, now_ms: u64) {
        self.saved = saved.clone();
        self.runtime.update_saved(saved, now_ms);
    }
    pub fn cancel_pending(&mut self, now_ms: u64) {
        if let Some(id) = self.runtime.pending().map(|request| request.id) {
            self.runtime.failed(id, now_ms);
        }
        self.active = false;
        self.runtime.set_active(false, now_ms);
    }
    pub fn hud(&self) -> QualityHud {
        let stats = self.runtime.stats();
        let mut savings = [None; LEVERS.len()];
        for reduction in self.runtime.reductions() {
            if let Some(index) = LEVERS
                .iter()
                .position(|setting| *setting == reduction.setting)
            {
                savings[index] = reduction
                    .saving_measured
                    .then_some(reduction.saved_microseconds);
            }
        }
        QualityHud {
            attached: true,
            active: self.active,
            saved: LEVERS.map(|setting| self.saved.get(setting)),
            confirmed: LEVERS.map(|setting| self.runtime.confirmed(setting)),
            savings_us: savings,
            pending: self
                .runtime
                .pending()
                .map(|request| request.reduction.setting),
            windows: stats.windows,
            gpu_p95_us: stats.gpu_p95_us,
            frame_p95_us: stats.frame_p95_us,
        }
    }
    pub fn advance(
        &mut self,
        app: &UiApp,
        view: &mut ClientView,
        now_ms: u64,
        benchmark: bool,
    ) -> Option<QualityRequest> {
        self.active = self.saved.get(Setting::Hold60) != 0
            && app.screen == Screen::Game
            && app.snapshot.connected
            && view.is_ready()
            && view.has_committed_far_front()
            && !benchmark;
        self.runtime.set_active(self.active, now_ms);
        if let Some(id) = self.runtime.pending().map(|request| request.id) {
            self.runtime
                .acknowledge(id, now_ms, view.settings_application().applied);
        }
        let mut supported = 0;
        if view.environment_stats().is_some() {
            supported |= 1 << Setting::Clouds as u32
                | 1 << Setting::Gi as u32
                | 1 << Setting::Ssao as u32
                | 1 << Setting::Scale as u32;
            if view.water_stats().is_some() {
                supported |= 1 << Setting::Reflections as u32;
            }
        }
        if view.horizon_stats().is_some() && view.has_committed_far_front() {
            supported |= 1 << Setting::LodError as u32 | 1 << Setting::Horizon as u32;
        }
        if view.lighting_stats().is_some() {
            supported |= 1 << Setting::Cascades as u32 | 1 << Setting::ShadowResolution as u32;
        }
        let request = self.runtime.poll(now_ms, supported)?;
        view.apply_settings(&request.target);
        self.runtime
            .acknowledge(request.id, now_ms, view.settings_application().applied);
        Some(request)
    }
}
