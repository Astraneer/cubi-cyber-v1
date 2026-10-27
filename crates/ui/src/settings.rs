//! One native/browser settings schema. User choices and temporary quality
//! reductions are separate; automatic decisions never rewrite saved settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Graphics,
    Controls,
    Sound,
    Interface,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    Default,
    Heavy,
    Custom,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum Setting {
    Horizon,
    Near,
    LodError,
    Msaa,
    Shadows,
    ShadowResolution,
    Cascades,
    Scale,
    AllowResolution,
    Ssao,
    Reflections,
    Clouds,
    Gi,
    /// Retired CUI1 compatibility slot. Antialiasing is selected by `Msaa`.
    Taa,
    Hold60,
    Fov,
    Exposure,
    Sensitivity,
    InvertY,
    Bob,
    Master,
    Blocks,
    Players,
    Creatures,
    Ambient,
    UiVolume,
    UiScale,
    Hints,
    Animations,
    ViewTime,
    ExposureMode,
    SkyStyle,
}
pub const COUNT: usize = 32;
const LEGACY_COUNT: usize = 29;
impl Setting {
    /// Retained serialized values never request or acknowledge renderer work.
    pub const fn is_retired(self) -> bool {
        matches!(self, Self::Taa)
    }
}

/// Missing active graphics receipts. Compatibility slots are not backends.
pub fn pending_graphics_mask(applied: u32) -> u32 {
    SCHEMA.iter().fold(0, |mask, definition| {
        let bit = 1u32 << definition.id as u32;
        if definition.section == Section::Graphics
            && !definition.id.is_retired()
            && applied & bit == 0
        {
            mask | bit
        } else {
            mask
        }
    })
}
#[derive(Clone, Copy)]
pub enum Domain {
    Range { step: i32, min: i32, max: i32 },
    Choices(&'static [i32]),
}
impl Domain {
    pub fn accepts(self, value: i32) -> bool {
        match self {
            Self::Range { step, min, max } => {
                step > 0 && value >= min && value <= max && (value - min) % step == 0
            }
            Self::Choices(values) => values.contains(&value),
        }
    }
}
#[derive(Clone, Copy)]
pub struct Definition {
    pub id: Setting,
    pub section: Section,
    pub label: &'static str,
    pub domain: Domain,
    pub default: i32,
    pub heavy: i32,
}
macro_rules! choice {
    ($id:ident,$section:ident,$label:literal,$values:expr,$default:expr,$heavy:expr) => {
        Definition {
            id: Setting::$id,
            section: Section::$section,
            label: $label,
            domain: Domain::Choices($values),
            default: $default,
            heavy: $heavy,
        }
    };
}
macro_rules! range {
    ($id:ident,$section:ident,$label:literal,$a:expr,$b:expr,$step:expr,$default:expr,$heavy:expr) => {
        Definition {
            id: Setting::$id,
            section: Section::$section,
            label: $label,
            domain: Domain::Range {
                step: $step,
                min: $a,
                max: $b,
            },
            default: $default,
            heavy: $heavy,
        }
    };
}
pub const SCHEMA: [Definition; COUNT] = [
    choice!(
        Horizon,
        Graphics,
        "Дальность горизонта",
        &[16, 24, 32, 48, 64, 96, 128, 192, 256, 384, 512, 768, 1024],
        256,
        1024
    ),
    choice!(
        Near,
        Graphics,
        "Ближняя область, минимум блоков",
        &[4, 8, 12, 16, 24, 32, 48, 64],
        32,
        64
    ),
    choice!(
        LodError,
        Graphics,
        "Цель ошибки дали, пиксели",
        &[1, 2, 4, 8, 16, 32],
        32,
        4
    ),
    choice!(Msaa, Graphics, "Сглаживание", &[1, 4, 2], 4, 4),
    choice!(Shadows, Graphics, "Тени", &[0, 1], 1, 1),
    choice!(
        ShadowResolution,
        Graphics,
        "Размер карты теней",
        &[512, 1024, 2048, 4096],
        2048,
        4096
    ),
    choice!(Cascades, Graphics, "Каскады теней", &[1, 2, 3], 3, 3),
    range!(Scale, Graphics, "Разрешение, %", 85, 100, 1, 100, 100),
    choice!(
        AllowResolution,
        Graphics,
        "Разрешить временно снижать разрешение",
        &[0, 1],
        0,
        0
    ),
    choice!(Ssao, Graphics, "Контактные тени", &[0, 1, 2], 1, 2),
    choice!(Reflections, Graphics, "Отражения", &[0, 1, 2], 1, 2),
    choice!(Clouds, Graphics, "Облака", &[0, 1, 2], 1, 2),
    choice!(Gi, Graphics, "Отражённый свет", &[0, 1, 2], 1, 2),
    choice!(Taa, Graphics, "Временное сглаживание TAA", &[0, 1], 0, 0),
    choice!(Hold60, Graphics, "Держать 60 кадров/с", &[0, 1], 1, 1),
    range!(Fov, Graphics, "Поле зрения", 50, 110, 1, 70, 70),
    range!(Exposure, Graphics, "Экспозиция, %", 25, 200, 5, 95, 95),
    range!(
        Sensitivity,
        Controls,
        "Чувствительность мыши, %",
        10,
        400,
        5,
        100,
        100
    ),
    choice!(InvertY, Controls, "Инверсия мыши", &[0, 1], 0, 0),
    choice!(Bob, Controls, "Покачивание камеры", &[0, 1], 1, 1),
    range!(Master, Sound, "Общая громкость, %", 0, 100, 5, 100, 100),
    range!(Blocks, Sound, "Блоки и машины, %", 0, 100, 5, 100, 100),
    range!(Players, Sound, "Игроки, %", 0, 100, 5, 100, 100),
    range!(Creatures, Sound, "Существа, %", 0, 100, 5, 100, 100),
    range!(Ambient, Sound, "Окружение, %", 0, 100, 5, 100, 100),
    range!(UiVolume, Sound, "Интерфейс, %", 0, 100, 5, 100, 100),
    choice!(
        UiScale,
        Interface,
        "Размер интерфейса, %",
        &[75, 100, 125, 150, 200],
        100,
        100
    ),
    choice!(Hints, Interface, "Подсказки", &[0, 1], 1, 1),
    choice!(Animations, Interface, "Анимации интерфейса", &[0, 1], 1, 1),
    choice!(
        ViewTime,
        Graphics,
        "Время вида, только у вас",
        &[0, 1, 2, 3, 4],
        0,
        0
    ),
    choice!(ExposureMode, Graphics, "Режим экспозиции", &[0, 1], 0, 0),
    choice!(SkyStyle, Graphics, "Стиль неба", &[0, 1, 2], 0, 0),
];
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    values: [i32; COUNT],
    pub preset: Preset,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            values: std::array::from_fn(|i| SCHEMA[i].default),
            preset: Preset::Default,
        }
    }
}
impl Settings {
    pub fn get(&self, id: Setting) -> i32 {
        self.values[id as usize]
    }
    pub fn set(&mut self, id: Setting, value: i32) -> Result<(), &'static str> {
        if !SCHEMA[id as usize].domain.accepts(value) {
            return Err("setting value outside schema");
        }
        if self.get(id) != value {
            self.values[id as usize] = value;
            if SCHEMA[id as usize].section == Section::Graphics {
                self.preset = Preset::Custom;
            }
        }
        Ok(())
    }
    pub fn apply_preset(&mut self, preset: Preset) {
        if preset != Preset::Custom {
            for def in &SCHEMA {
                if def.section == Section::Graphics {
                    self.values[def.id as usize] = if preset == Preset::Heavy {
                        def.heavy
                    } else {
                        def.default
                    };
                }
            }
        }
        self.preset = preset;
    }
    pub fn reset_section(&mut self, section: Section) {
        for def in &SCHEMA {
            if def.section == section {
                self.values[def.id as usize] = def.default;
            }
        }
        if section == Section::Graphics {
            self.preset = Preset::Default;
        }
    }
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(7 + COUNT * 4);
        bytes.extend_from_slice(b"CUI1");
        bytes.push(match self.preset {
            Preset::Default => 0,
            Preset::Heavy => 1,
            Preset::Custom => 2,
        });
        bytes.extend_from_slice(&(COUNT as u16).to_le_bytes());
        for value in self.values {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() < 7 || &bytes[..4] != b"CUI1" {
            return Err("unsupported settings format");
        }
        let stored = u16::from_le_bytes([bytes[5], bytes[6]]) as usize;
        if ![LEGACY_COUNT, 30, COUNT].contains(&stored) || bytes.len() != 7 + stored * 4 {
            return Err("unsupported settings format");
        }
        let preset = match bytes[4] {
            0 => Preset::Default,
            1 => Preset::Heavy,
            2 => Preset::Custom,
            _ => return Err("invalid preset"),
        };
        let mut value = Self::default();
        for (i, definition) in SCHEMA.iter().enumerate().take(stored) {
            let start = 7 + i * 4;
            let n = i32::from_le_bytes(bytes[start..start + 4].try_into().unwrap());
            if !definition.domain.accepts(n) {
                return Err("invalid stored setting");
            }
            value.values[i] = n;
        }
        if stored == LEGACY_COUNT
            && preset == Preset::Default
            && value.get(Setting::ShadowResolution) == 4096
        {
            value.values[Setting::ShadowResolution as usize] = 2048;
        }
        if preset == Preset::Default && value.get(Setting::LodError) == 4 {
            value.values[Setting::LodError as usize] = 32;
        }
        value.preset = preset;
        Ok(value)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reduction {
    pub setting: Setting,
    pub before: i32,
    pub after: i32,
    pub saved_microseconds: u32,
    pub saving_measured: bool,
}
/// Runtime overrides have a bounded stack and a ten-second decision cooldown.
#[derive(Clone)]
pub struct AutoQuality {
    effective: [i32; COUNT],
    history: [Option<Reduction>; 32],
    len: usize,
    last_decision_ms: u64,
    slow_windows: u8,
    fast_windows: u8,
}
impl AutoQuality {
    pub fn new(settings: &Settings) -> Self {
        Self {
            effective: settings.values,
            history: [None; 32],
            len: 0,
            last_decision_ms: 0,
            slow_windows: 0,
            fast_windows: 0,
        }
    }
    pub fn effective(&self, id: Setting) -> i32 {
        self.effective[id as usize]
    }
    pub fn reductions(&self) -> impl Iterator<Item = &Reduction> {
        self.history[..self.len].iter().flatten()
    }
    pub fn reset(&mut self, settings: &Settings) {
        *self = Self::new(settings);
    }
    /// A prepared backend acknowledges a decision only after its resources and
    /// effective values have changed. Its cooldown starts at that point.
    pub fn acknowledged_at(&mut self, now_ms: u64) {
        self.last_decision_ms = now_ms;
        self.slow_windows = 0;
        self.fast_windows = 0;
    }
    pub fn effective_settings(&self, saved: &Settings) -> Settings {
        let mut result = saved.clone();
        for definition in SCHEMA {
            if definition.section == Section::Graphics {
                result.values[definition.id as usize] = self.effective(definition.id);
            }
        }
        result
    }
    /// Each sample represents a complete one-second window, with enough frames to
    /// reject a single hitch. Savings are measured by the caller's GPU pass timers.
    pub fn observe(
        &mut self,
        settings: &Settings,
        now_ms: u64,
        frames: u16,
        p95_us: u32,
        headroom_us: u32,
    ) -> Option<Reduction> {
        self.observe_supported(settings, now_ms, frames, p95_us, headroom_us, u32::MAX)
    }
    /// Only settings with an actual prepared backend may be proposed. A missing
    /// earlier lever does not prevent the next supported lever from being used.
    pub fn observe_supported(
        &mut self,
        settings: &Settings,
        now_ms: u64,
        frames: u16,
        p95_us: u32,
        headroom_us: u32,
        supported: u32,
    ) -> Option<Reduction> {
        if settings.get(Setting::Hold60) == 0 || frames < 8 {
            return None;
        }
        self.slow_windows = if p95_us > 18000 {
            self.slow_windows.saturating_add(1)
        } else {
            0
        };
        self.fast_windows = if p95_us < 14000 {
            self.fast_windows.saturating_add(1)
        } else {
            0
        };
        if now_ms.saturating_sub(self.last_decision_ms) < 10000 {
            return None;
        }
        if self.fast_windows >= 5 && self.len > 0 {
            let reduction = self.history[self.len - 1]?;
            if supported & (1 << reduction.setting as u32) == 0
                || !reduction.saving_measured
                || headroom_us < reduction.saved_microseconds.saturating_add(1500)
            {
                return None;
            }
            self.len -= 1;
            self.history[self.len] = None;
            self.effective[reduction.setting as usize] = reduction.before;
            self.last_decision_ms = now_ms;
            self.fast_windows = 0;
            return Some(Reduction {
                before: reduction.after,
                after: reduction.before,
                ..reduction
            });
        }
        if self.slow_windows < 3 || self.len == self.history.len() {
            return None;
        }
        let mut next = None;
        // Remove quality above the selected preset ceiling first.
        for id in [Setting::Clouds, Setting::Gi] {
            if supported & (1 << id as u32) == 0 {
                continue;
            }
            let ceiling = if settings.preset == Preset::Heavy {
                SCHEMA[id as usize].heavy
            } else {
                SCHEMA[id as usize].default
            };
            if self.effective(id) > ceiling {
                next = Some((id, self.effective(id) - 1));
                break;
            }
        }
        if next.is_none() {
            for id in [
                Setting::Ssao,
                Setting::Reflections,
                Setting::Near,
                Setting::Scale,
                Setting::LodError,
                Setting::Horizon,
                Setting::ShadowResolution,
                Setting::Cascades,
            ] {
                if supported & (1 << id as u32) == 0 {
                    continue;
                }
                if id == Setting::Scale && settings.get(Setting::AllowResolution) == 0 {
                    continue;
                }
                let old = self.effective(id);
                let new = match id {
                    Setting::Ssao | Setting::Reflections | Setting::Cascades => old - 1,
                    Setting::Scale => old - 5,
                    Setting::LodError => old * 2,
                    _ => {
                        let Domain::Choices(values) = SCHEMA[id as usize].domain else {
                            unreachable!()
                        };
                        let index = values.iter().position(|v| *v == old).unwrap();
                        values[index.saturating_sub(1)]
                    }
                };
                if new != old && SCHEMA[id as usize].domain.accepts(new) {
                    next = Some((id, new));
                    break;
                }
            }
        }
        let (setting, after) = next?;
        let before = self.effective(setting);
        let reduction = Reduction {
            setting,
            before,
            after,
            saved_microseconds: 0,
            saving_measured: false,
        };
        self.effective[setting as usize] = after;
        self.history[self.len] = Some(reduction);
        self.len += 1;
        self.last_decision_ms = now_ms;
        self.slow_windows = 0;
        Some(reduction)
    }
    pub fn measured_saving(&mut self, microseconds: u32) {
        if self.len > 0 {
            let reduction = self.history[self.len - 1].as_mut().unwrap();
            reduction.saved_microseconds = microseconds;
            reduction.saving_measured = true;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schema_values_are_complete_valid_and_section_reset_is_local() {
        let mut settings = Settings::default();
        for (i, def) in SCHEMA.iter().enumerate() {
            assert_eq!(def.id as usize, i);
            assert!(def.domain.accepts(def.default));
            assert!(def.domain.accepts(def.heavy));
        }
        settings.set(Setting::Master, 25).unwrap();
        settings.set(Setting::Fov, 90).unwrap();
        settings.reset_section(Section::Graphics);
        assert_eq!(settings.get(Setting::Master), 25);
        assert_eq!(settings.get(Setting::Fov), 70);
        settings.apply_preset(Preset::Heavy);
        assert_eq!(settings.get(Setting::Horizon), 1024);
        assert_eq!(settings.get(Setting::Master), 25);
        assert_eq!(settings.get(Setting::Msaa), 4);
        assert_eq!(settings.get(Setting::Taa), 0);
    }
    #[test]
    fn storage_rejects_truncation_unknown_versions_and_invalid_values() {
        let mut settings = Settings::default();
        settings.set(Setting::Sensitivity, 150).unwrap();
        let encoded = settings.encode();
        assert_eq!(Settings::decode(&encoded).unwrap(), settings);
        for n in 0..encoded.len() {
            assert!(Settings::decode(&encoded[..n]).is_err());
        }
        let mut bad = encoded;
        bad[7..11].copy_from_slice(&1025_i32.to_le_bytes());
        assert!(Settings::decode(&bad).is_err());
        assert!(settings.set(Setting::Scale, 84).is_err());
    }
    #[test]
    fn automatic_quality_preserves_user_choices_msaa_shadows_and_resolution_permission() {
        let settings = Settings::default();
        let original = settings.clone();
        let mut quality = AutoQuality::new(&settings);
        for now in (10000..=1000000).step_by(1000) {
            quality.observe(&settings, now, 60, 22000, 0);
        }
        assert_eq!(settings, original);
        assert_eq!(quality.effective(Setting::Msaa), 4);
        assert_eq!(quality.effective(Setting::Shadows), 1);
        assert_eq!(quality.effective(Setting::Scale), 100);
        assert!(
            quality
                .reductions()
                .all(|r| r.setting != Setting::Msaa && r.setting != Setting::Shadows)
        );
    }
    #[test]
    fn isolated_hitch_is_ignored_and_recovery_requires_measured_savings() {
        let settings = Settings::default();
        let mut quality = AutoQuality::new(&settings);
        assert!(quality.observe(&settings, 10000, 60, 50000, 0).is_none());
        assert!(quality.observe(&settings, 11000, 60, 12000, 0).is_none());
        for now in [12000, 13000] {
            assert!(quality.observe(&settings, now, 60, 22000, 0).is_none());
        }
        let first = quality.observe(&settings, 14000, 60, 22000, 0).unwrap();
        assert_eq!(first.setting, Setting::Ssao);
        quality.measured_saving(3000);
        for now in (15000..=25000).step_by(1000) {
            assert!(quality.observe(&settings, now, 60, 12000, 4000).is_none());
        }
        let restored = quality.observe(&settings, 26000, 60, 12000, 5000).unwrap();
        assert_eq!(restored.before, first.after);
        assert_eq!(restored.after, first.before);
    }
}
