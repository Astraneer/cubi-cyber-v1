use super::{ClientView, SettingsApplication};
use render::terrain_gpu::UploadBudget;
use ui::settings::{Setting, Settings};
impl ClientView {
    pub fn requested_near_blocks(&self) -> Option<u16> {
        self.requested_settings
            .as_ref()
            .map(|settings| settings.get(Setting::Near) as u16)
    }
    pub fn near_settings_error(&self) -> Option<crate::near_profile::Error> {
        let settings = self.requested_settings.as_ref()?;
        crate::near_profile::NearProfile::plan(
            settings.get(Setting::Near) as u16,
            Default::default(),
        )
        .err()
    }
    pub fn effective_near_blocks(&self) -> u16 {
        u16::from(self.scene.config().horizontal_radius) * 16
    }
    pub fn apply_settings(&mut self, settings: &Settings) -> SettingsApplication {
        self.requested_settings = Some(settings.clone());
        let fov = settings.get(Setting::Fov) as f32;
        let bob = settings.get(Setting::Bob) != 0;
        if self.field_of_view != fov || self.view_bob != bob {
            self.field_of_view = fov;
            self.view_bob = bob;
            self.invalidate();
        }
        use crate::celestial_clock::PersonalTime;
        let personal = match settings.get(Setting::ViewTime) {
            1 => Some(PersonalTime::Day),
            2 => Some(PersonalTime::Night),
            3 => Some(PersonalTime::Fixed(0)),
            4 => Some(PersonalTime::Fixed(12000)),
            _ => None,
        };
        self.set_personal_time(personal)
            .expect("validated settings clock choice");
        // A failed/unfinished selection keeps its previous committed cover.
        // Its receipt remains pending until the exact new front is committed.
        let _ = self.set_horizon_settings(
            settings.get(Setting::Horizon) as u32,
            settings.get(Setting::LodError) as f64,
        );
        self.settings_application()
    }
    /// A receipt comes from current CPU/GPU state. Saving a requested value
    /// alone does not make its backend effective.
    pub fn settings_application(&self) -> SettingsApplication {
        let Some(settings) = &self.requested_settings else {
            return SettingsApplication::default();
        };
        let mut applied = (1 << Setting::Fov as u32) | (1 << Setting::Bob as u32);
        let mut reload_required = 0;
        if let Ok(profile) = crate::near_profile::NearProfile::plan(
            settings.get(Setting::Near) as u16,
            Default::default(),
        ) {
            if profile.matches_scene(&self.scene.config()) {
                if self.is_ready() {
                    applied |= 1 << Setting::Near as u32;
                }
            } else {
                reload_required |= 1 << Setting::Near as u32;
            }
        }
        if self.horizon_setting_applied(settings.get(Setting::Horizon) as u32) {
            applied |= 1 << Setting::Horizon as u32;
        }
        if self.lod_error_setting_applied(settings.get(Setting::LodError) as f64) {
            applied |= 1 << Setting::LodError as u32;
        }
        if self.render_scale() == settings.get(Setting::Scale) as u32 {
            applied |= 1 << Setting::Scale as u32;
        } else {
            reload_required |= 1 << Setting::Scale as u32;
        }
        // Permission for adaptive scaling is a controller policy, not another
        // render target. Its saved value is consumed by AutoQuality.
        applied |= 1 << Setting::AllowResolution as u32;
        let samples = settings.get(Setting::Msaa) as u32;
        if self.render_sample_count() == samples && self.smaa_prepared() == (samples == 1) {
            applied |= 1 << Setting::Msaa as u32;
        } else {
            reload_required |= 1 << Setting::Msaa as u32;
        }
        if let Some(lighting) = &self.lighting {
            for (setting, effective) in [
                (
                    Setting::Shadows,
                    i32::from(lighting.owner.shadows().enabled()),
                ),
                (
                    Setting::Cascades,
                    lighting.owner.shadows().active_cascades() as i32,
                ),
            ] {
                if effective == settings.get(setting) {
                    applied |= 1 << setting as u32;
                } else {
                    reload_required |= 1 << setting as u32;
                }
            }
            if lighting.owner.shadows().cache().config().resolution
                == settings.get(Setting::ShadowResolution) as u32
            {
                applied |= 1 << Setting::ShadowResolution as u32;
            } else {
                reload_required |= 1 << Setting::ShadowResolution as u32;
            }
        }
        if let Some(environment) = &self.environment {
            let ssao = environment
                .owner
                .gpu
                .contact_ao()
                .map_or(0, |ao| i32::from(ao.config().quality));
            if ssao == settings.get(Setting::Ssao) {
                applied |= 1 << Setting::Ssao as u32;
            } else {
                reload_required |= 1 << Setting::Ssao as u32;
            }
            if environment.cloud_quality == settings.get(Setting::Clouds) {
                applied |= 1 << Setting::Clouds as u32;
            } else {
                reload_required |= 1 << Setting::Clouds as u32;
            }
            if environment.reflection_quality == settings.get(Setting::Reflections) {
                applied |= 1 << Setting::Reflections as u32;
            } else {
                reload_required |= 1 << Setting::Reflections as u32;
            }
            if environment.gi_quality == settings.get(Setting::Gi) {
                if environment.gi_quality == 0 || environment.owner.stats.gi_started {
                    applied |= 1 << Setting::Gi as u32;
                }
            } else {
                reload_required |= 1 << Setting::Gi as u32;
            }
        }
        if self.environment.as_ref().is_some_and(|environment| {
            environment.owner.gpu.post().compensation()
                == settings.get(Setting::Exposure) as f32 / 100.
        }) {
            applied |= 1 << Setting::Exposure as u32;
        }
        if let Some(automatic) = self.exposure_automatic() {
            if automatic == (settings.get(Setting::ExposureMode) == 0) {
                applied |= 1 << Setting::ExposureMode as u32;
            } else {
                reload_required |= 1 << Setting::ExposureMode as u32;
            }
        }
        if let Some(styles) = self.celestial_styles() {
            let expected = match settings.get(Setting::SkyStyle) {
                0 => [0, 0, 0],
                1 => [1, 0, 1],
                _ => [2, 1, 2],
            };
            if styles == expected {
                applied |= 1 << Setting::SkyStyle as u32;
            } else {
                reload_required |= 1 << Setting::SkyStyle as u32;
            }
        }
        if self.environment.is_some() && self.sun_clock().is_some() {
            applied |= 1 << Setting::ViewTime as u32;
        }
        let pending_render = ui::settings::pending_graphics_mask(applied);
        SettingsApplication {
            applied,
            pending_render,
            reload_required,
        }
    }
    pub fn smaa_prepared(&self) -> bool {
        self.environment
            .as_ref()
            .is_some_and(|environment| environment.owner.gpu.post().smaa().is_some())
    }
    pub fn smaa_reserved_bytes(&self) -> u64 {
        self.environment
            .as_ref()
            .and_then(|environment| environment.owner.gpu.post().smaa())
            .map_or(0, |smaa| smaa.reserved_bytes())
    }
    pub(super) fn flush_post_settings(
        &mut self,
        queue: &wgpu::Queue,
        budget: &mut UploadBudget,
    ) -> Result<(), String> {
        let (Some(settings), Some(environment)) = (&self.requested_settings, &mut self.environment)
        else {
            return Ok(());
        };
        // Keep the complete existing4KiB frame allowance. A tiny budget keeps
        // the old effective setting and its pending receipt, without a write.
        if budget.remaining_bytes < 4096 + 64 {
            return Ok(());
        }
        let bytes = environment
            .owner
            .gpu
            .post_mut()
            .set_compensation(queue, settings.get(Setting::Exposure) as f32 / 100., budget)
            .map_err(|error| error.to_string())?;
        self.retained_foreground_work.record(telemetry::WorkCounts {
            upload_bytes: bytes,
            ..telemetry::WorkCounts::ZERO
        });
        Ok(())
    }
}
