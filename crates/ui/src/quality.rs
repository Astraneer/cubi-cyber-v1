//! Fixed-size view of confirmed automatic reductions; no platform/runtime owner.
use crate::{
    paint::{Painter, TextBuffer},
    settings::Setting,
};
use std::fmt::Write;
pub const LEVERS: [Setting; 10] = [
    Setting::Reflections,
    Setting::Clouds,
    Setting::Gi,
    Setting::Ssao,
    Setting::Near,
    Setting::Scale,
    Setting::LodError,
    Setting::Horizon,
    Setting::ShadowResolution,
    Setting::Cascades,
];
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QualityHud {
    pub attached: bool,
    pub active: bool,
    pub saved: [i32; LEVERS.len()],
    pub confirmed: [i32; LEVERS.len()],
    pub savings_us: [Option<u32>; LEVERS.len()],
    pub pending: Option<Setting>,
    pub windows: u64,
    pub gpu_p95_us: u32,
    pub frame_p95_us: u32,
}
impl QualityHud {
    pub fn paint(&self, p: &mut Painter<'_>, _width: f32, height: f32) {
        if !self.attached {
            return;
        }
        let mut text = TextBuffer::default();
        self.write_text(&mut text);
        p.text(
            16.,
            (height - 30.).clamp(8., 406.),
            text.as_str(),
            0xffc7d69e,
        );
    }
    pub fn write_text(&self, text: &mut TextBuffer) {
        text.clear();
        let _ = write!(text, "Авто{}:", if self.active { "" } else { " пауза" });
        let mut changed = false;
        for (index, label) in [
            "SSR",
            "обл",
            "GI",
            "AO",
            "блоки",
            "%",
            "LOD",
            "даль",
            "тени",
            "каск",
        ]
        .into_iter()
        .enumerate()
        {
            if self.saved[index] == self.confirmed[index] {
                continue;
            }
            changed = true;
            let _ = write!(
                text,
                " {label}{}→{}",
                self.saved[index], self.confirmed[index]
            );
            if let Some(us) = self.savings_us[index] {
                let _ = write!(text, " {}.{:02}мс", us / 1000, us % 1000 / 10);
            } else {
                let _ = write!(text, " ?мс");
            }
        }
        if !changed {
            let _ = write!(text, " без изменений");
        }
        if let Some(setting) = self.pending {
            let _ = write!(text, " · готовится {setting:?}");
        }
        if self.windows > 0 {
            let _ = write!(
                text,
                " · p95 кадр/GPU {}.{:02}/{}.{:02}мс",
                self.frame_p95_us / 1000,
                self.frame_p95_us % 1000 / 10,
                self.gpu_p95_us / 1000,
                self.gpu_p95_us % 1000 / 10
            );
        } else {
            let _ = write!(text, " · p95 не измерено");
        }
    }
}
