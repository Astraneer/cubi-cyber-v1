#![cfg(not(target_arch = "wasm32"))]
//! Real prepared owners and graph work determine visual-setting receipts.
use client::{
    SceneConfig,
    view::{ClientView, EnvironmentProfile},
};
use render::{
    OutputTransfer,
    atmosphere::AtmosphereConfig,
    environment_gpu::EnvironmentConfig,
    terrain_gpu::{AtlasImage, GpuConfig, TerrainGpu, UploadBudget},
};
use ui::settings::{Setting, Settings};

#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
#[test]
fn exposure_and_celestial_receipts_follow_prepared_owners_not_saved_requests() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    let atlas = AtlasImage::donor().unwrap();
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("visual choices actual graph target"),
        size: wgpu::Extent3d {
            width: 64,
            height: 64,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let output = texture.create_view(&Default::default());
    let bits = (1u32 << Setting::ExposureMode as u32) | (1u32 << Setting::SkyStyle as u32);
    for (mode, style, expected, samples) in [
        (0, 0, [0, 0, 0], 4),
        (1, 2, [2, 1, 2], 4),
        (1, 1, [1, 0, 1], 1),
    ] {
        let renderer = TerrainGpu::new_with_samples(
            &gpu.device,
            &gpu.queue,
            wgpu::TextureFormat::Rgba8Unorm,
            64,
            64,
            OutputTransfer::Attachment,
            GpuConfig {
                max_pages: 32,
                max_quads: 262144,
                occlusion: true,
            },
            &atlas,
            samples,
        )
        .unwrap();
        let mut view = ClientView::new(
            renderer,
            SceneConfig {
                horizontal_radius: 0,
                vertical_radius: 0,
                max_loaded_pages: 32,
                authoritative_remote: true,
                ..SceneConfig::forest()
            },
        )
        .unwrap();
        let mut settings = Settings::default();
        settings.set(Setting::ExposureMode, mode).unwrap();
        settings.set(Setting::SkyStyle, style).unwrap();
        settings.set(Setting::Msaa, samples as i32).unwrap();
        let before = view.apply_settings(&settings);
        assert_eq!(
            before.applied & bits,
            0,
            "saving preferences cannot attach GPU owners"
        );
        let mut profile = EnvironmentProfile {
            graphics: EnvironmentConfig {
                atmosphere: AtmosphereConfig {
                    transmittance_size: [16, 8],
                    sky_size: [16, 8],
                    transmittance_steps: 16,
                    sky_steps: 16,
                },
                ..Default::default()
            },
            aerial: None,
            multiple_scattering: None,
            ..Default::default()
        }
        .with_visual_choices(mode as u8, style as u8)
        .unwrap()
        .with_antialiasing(samples)
        .unwrap();
        profile.graphics.post.bloom = None;
        profile.celestials.as_mut().unwrap().stars = 64;
        view.prepare_environment(
            &gpu.device,
            &gpu.queue,
            64,
            64,
            OutputTransfer::Attachment,
            profile,
            &atlas,
        )
        .unwrap();
        assert_eq!(view.smaa_prepared(), samples == 1);
        assert_eq!(
            view.smaa_reserved_bytes(),
            if samples == 1 {
                64 * 64 * 18 + 20824
            } else {
                0
            }
        );
        assert_ne!(
            view.settings_application().applied & (1 << Setting::Msaa as u32),
            0
        );
        assert_eq!(view.exposure_automatic(), Some(mode == 0));
        assert_eq!(view.celestial_styles(), Some(expected));
        assert_eq!(view.settings_application().applied & bits, bits);
        assert_eq!(view.settings_application().reload_required & bits, 0);
        view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
            .unwrap();
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        view.encode(&gpu.queue, &mut encoder, &output, 64, 64)
            .unwrap();
        gpu.queue.submit([encoder.finish()]);
        gpu.device.poll(wgpu::PollType::Wait).unwrap();
        let work = view.environment_stats().unwrap().gpu.post;
        assert_eq!(work.tone_pixels, 4096);
        if mode == 0 {
            assert!(work.histogram_samples > 0);
        } else {
            assert_eq!(work.histogram_samples, 0);
        }
        assert!(
            !view.is_ready(),
            "an actual empty sky graph is still not a ready world"
        );
        if samples == 1 {
            assert_eq!(work.aa_pixels, 12288);
            assert_eq!(work.passes, 4);
            assert_eq!(view.stats().frame_pipeline_compilations, 0);
            view.advance_input(0.).unwrap();
            view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
                .unwrap();
            let _ = view.take_frame_work();
            let scope = telemetry::AllocationScope::begin().unwrap();
            for _ in 0..1000 {
                view.advance_input(0.).unwrap();
                view.upload_ready(&gpu.queue, &mut UploadBudget::new(2 * 1024 * 1024))
                    .unwrap();
            }
            assert_eq!(scope.finish(), telemetry::AllocationCounts::default());
            assert_eq!(view.take_frame_work(), telemetry::WorkCounts::ZERO);
        } else {
            assert_eq!(work.aa_pixels, 0);
        }
        settings.set(Setting::ExposureMode, 1 - mode).unwrap();
        settings.set(Setting::SkyStyle, (style + 1) % 3).unwrap();
        let requested = view.apply_settings(&settings);
        assert_eq!(requested.applied & bits, 0);
        assert_eq!(requested.reload_required & bits, bits);
        assert_eq!(view.exposure_automatic(), Some(mode == 0));
        assert_eq!(view.celestial_styles(), Some(expected));
    }
    assert!(
        EnvironmentProfile::default()
            .with_visual_choices(2, 0)
            .is_err()
    );
    assert!(
        EnvironmentProfile::default()
            .with_visual_choices(0, 3)
            .is_err()
    );
}
