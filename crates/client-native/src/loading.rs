//! The pipeline worker owns all heavy GPU/asset preparation before handing one
//! heap-owned shared view to the window thread. No frame loop compiles shaders.
use client::{
    lighting::ClientLightingConfig,
    view::{ClientView, NearRasterization},
};
use render::terrain_gpu::{AtlasImage, TerrainGpu};

#[derive(Clone)]
pub struct Preparation {
    pub format: wgpu::TextureFormat,
    pub width: u32,
    pub height: u32,
    pub scene: client::SceneConfig,
    pub raster: NearRasterization,
    pub entities: bool,
    pub props: bool,
    pub lighting: Option<ClientLightingConfig>,
    pub materials: bool,
    pub environment: bool,
    pub horizon: bool,
    pub settings: Option<ui::settings::Settings>,
}

pub fn prepare(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    preparation: Preparation,
) -> Result<Box<ClientView>, String> {
    let mut scene = preparation.scene;
    let settings = preparation.settings.clone().unwrap_or_default();
    if scene.authoritative_remote && preparation.environment {
        client::near_profile::NearProfile::plan(
            settings.get(ui::settings::Setting::Near) as u16,
            Default::default(),
        )
        .map_err(|error| error.to_string())?
        .apply_scene(&mut scene);
    }
    let atlas = AtlasImage::donor()?;
    let sample_count = preparation.settings.as_ref().map_or(4, |settings| {
        settings.get(ui::settings::Setting::Msaa) as u32
    });
    let terrain = TerrainGpu::new_with_samples(
        device,
        queue,
        preparation.format,
        preparation.width,
        preparation.height,
        render::OutputTransfer::Attachment,
        client::view::terrain_gpu_config(),
        &atlas,
        sample_count,
    )
    .map_err(|error| error.to_string())?;
    let mut view = ClientView::new_with_rasterization(terrain, scene, preparation.raster)?;
    if scene.authoritative_remote && preparation.environment {
        view.apply_settings(&settings);
    } else if let Some(settings) = &preparation.settings {
        view.apply_settings(settings);
    }
    if let Some(mut config) = preparation.lighting {
        if let Some(settings) = &preparation.settings {
            config.shadows = config
                .shadows
                .with_resolution(settings.get(ui::settings::Setting::ShadowResolution) as u32)
                .and_then(|shadows| {
                    shadows.with_cascades(settings.get(ui::settings::Setting::Cascades) as u32)
                })
                .map_err(|error| format!("{error:?}"))?
                .with_enabled(settings.get(ui::settings::Setting::Shadows) != 0);
        }
        config.shadows.dynamic_casters |=
            preparation.entities || preparation.props || preparation.horizon;
        view.prepare_lighting(device, queue, config)?;
    }
    if preparation.environment {
        let settings = preparation.settings.as_ref();
        let profile = client::view::EnvironmentProfile::product()
            .with_quality(
                settings.map_or(1, |settings| {
                    settings.get(ui::settings::Setting::Clouds) as u8
                }),
                settings.map_or(1, |settings| settings.get(ui::settings::Setting::Gi) as u8),
            )
            .and_then(|profile| {
                profile.with_reflections(settings.map_or(1, |settings| {
                    settings.get(ui::settings::Setting::Reflections) as u8
                }))
            })
            .and_then(|profile| {
                profile.with_contact_ao(settings.map_or(1, |settings| {
                    settings.get(ui::settings::Setting::Ssao) as u8
                }))
            })
            .and_then(|profile| profile.with_antialiasing(sample_count))
            .and_then(|profile| {
                profile.with_visual_choices(
                    settings.map_or(0, |settings| {
                        settings.get(ui::settings::Setting::ExposureMode) as u8
                    }),
                    settings.map_or(0, |settings| {
                        settings.get(ui::settings::Setting::SkyStyle) as u8
                    }),
                )
            })
            .map_err(|error| error.to_string())?;
        view.prepare_environment(
            device,
            queue,
            preparation.width,
            preparation.height,
            render::OutputTransfer::Attachment,
            profile,
            &atlas,
        )?;
        view.prepare_water(Default::default())?;
    }
    if preparation.entities {
        view.prepare_entities(device, queue)?;
    }
    if preparation.props {
        view.prepare_props(device, queue, Default::default())?;
    }
    if preparation.horizon {
        view.prepare_horizon(device, queue, client::view::horizon::Config::default())?;
        view.prepare_far_water(Default::default())?;
        view.prepare_distant_shadows(device)?;
    }
    if preparation.materials {
        let prepared =
            render::material_pack::PreparedMaterials::prepare(&atlas, &[], 0, Default::default())
                .map_err(|error| format!("material defaults: {error:?}"))?;
        let pack = render::material_pack::MaterialPackGpu::upload(device, queue, &prepared)
            .map_err(|error| format!("material GPU defaults: {error:?}"))?;
        view.renderer_mut()
            .prepare_material_pack(device, queue, pack, false)
            .map_err(|error| error.to_string())?;
    }
    if let Some(settings) = &preparation.settings {
        view.apply_settings(settings);
    }
    Ok(Box::new(view))
}

/// Explicit startup profile: server LIGHT is authoritative and both shadow
/// targets fit the selected 2048 resolution. Runtime quality does not change it.
pub fn product_lighting() -> ClientLightingConfig {
    ClientLightingConfig {
        source: client::lighting::LightSource::Authoritative,
        shadows: render::shadows::ShadowConfig {
            resolution: 2048,
            sun_step_radians: 1e-6,
            max_updates_per_frame: 3,
            projected_bounds: true,
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Waiting happens only on the loading worker. Shutdown/cancel can finish even
/// when the window loop will no longer poll the old GPU completion callback.
pub fn wait_retirement(
    receiver: &std::sync::mpsc::Receiver<()>,
    cancelled: &std::sync::atomic::AtomicU32,
) -> Result<(), String> {
    loop {
        if cancelled.load(std::sync::atomic::Ordering::Acquire) != 0 {
            return Err("world loading cancelled".into());
        }
        match receiver.recv_timeout(std::time::Duration::from_millis(16)) {
            Ok(()) => return Ok(()),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err("GPU retirement callback lost".into());
            }
        }
    }
}
#[cfg(test)]
mod retirement_tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
        mpsc,
    };
    #[test]
    fn cancelling_loading_finishes_without_any_gpu_callback_or_device_poll() {
        let (_callback_sender, receiver) = mpsc::sync_channel(1);
        let cancel = Arc::new(AtomicU32::new(0));
        let worker_cancel = cancel.clone();
        let (finished_sender, finished_receiver) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            let result = wait_retirement(&receiver, &worker_cancel);
            finished_sender.send(result).unwrap();
        });
        cancel.store(1, Ordering::Release);
        assert!(
            finished_receiver
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap()
                .is_err()
        );
        worker.join().unwrap();
        let (sender, receiver) = mpsc::sync_channel(1);
        sender.send(()).unwrap();
        assert!(wait_retirement(&receiver, &AtomicU32::new(0)).is_ok());
        drop(sender);
        assert!(wait_retirement(&receiver, &AtomicU32::new(0)).is_err());
    }
}
