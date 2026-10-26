use super::{ClientView, SettingsApplication};
use render::terrain_gpu::UploadBudget;
use ui::settings::{Section, Setting, Settings};
impl ClientView {
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
        if self.render_sample_count() == settings.get(Setting::Msaa) as u32 {
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
        if self.environment.is_some() && self.sun_clock().is_some() {
            applied |= 1 << Setting::ViewTime as u32;
        }
        let mut pending_render = 0;
        for definition in ui::settings::SCHEMA {
            let bit = 1 << definition.id as u32;
            if definition.section == Section::Graphics && applied & bit == 0 {
                pending_render |= bit;
            }
        }
        SettingsApplication {
            applied,
            pending_render,
            reload_required,
        }
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
