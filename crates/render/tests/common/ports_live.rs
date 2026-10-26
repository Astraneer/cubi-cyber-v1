use super::*;
use render::{
    light_volume::PackedLight,
    shadows::{DynamicShadowCaster, ShadowConfig, ShadowGpu},
};
#[test]
fn confirmed_machine_ports_color_actual_models_in_world_orientation_without_shadow_updates() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let mut terrain = terrain(&gpu);
        let shadows = ShadowGpu::new(
            &gpu.device,
            &gpu.queue,
            terrain.geometry(),
            ShadowConfig {
                resolution: 64,
                ..ShadowConfig::default()
            },
            [0.; 3],
            [0.35, 0.8, 0.2],
        )
        .unwrap();
        let mut models = BlockModelsGpu::new(
            &gpu.device,
            &gpu.queue,
            terrain.geometry(),
            BlockConfig {
                max_instances: 4,
                atlas_layers: 186,
            },
            Some(&shadows),
        )
        .unwrap();
        models.take_work();
        let catalog = read_model(&gpu, &models);
        let camera = Camera::perspective(
            [0., 0.6, 3.],
            [0., 0.6, 0.],
            [0., 1., 0.],
            0.8,
            256. / 192.,
            0.1,
            20.,
        );
        let mut row = BlockInstance {
            kind: kind::INFUSER,
            light: PackedLight::new(0, 0, [15; 3]).unwrap().raw(),
            ..BlockInstance::default()
        };
        let mut budget = UploadBudget::new(100_000);
        let mut changed = 0;
        for yaw in 0..4 {
            row.yaw = yaw as f32 * std::f32::consts::FRAC_PI_2;
            row.reserved[3] = 0x80000000;
            models.upload(&gpu.queue, &[row], &mut budget).unwrap();
            let baseline = frame(&gpu, &mut terrain, Some(&mut models), camera).0;
            let shadow_revision = models.shadow_revision();
            for mode in 1..6 {
                // Only World South differs: all other confirmed sides remain gray.
                row.reserved[3] = 0x80000000 | (mode << 9);
                assert_eq!(
                    models
                        .update(&gpu.queue, 0, row, &mut budget)
                        .unwrap()
                        .upload_bytes,
                    64
                );
                assert_eq!(
                    models.shadow_revision(),
                    shadow_revision,
                    "ports are albedo, not caster geometry"
                );
                let image = frame(&gpu, &mut terrain, Some(&mut models), camera).0;
                let pixels = image
                    .chunks_exact(4)
                    .zip(baseline.chunks_exact(4))
                    .filter(|(a, b)| a[..3] != b[..3])
                    .count();
                assert!(
                    pixels > 20,
                    "World South port not visible after yaw{yaw} mode{mode}: {pixels}"
                );
                changed += pixels;
            }
        }
        row.light = 0;
        let shadow_revision = models.shadow_revision();
        models.update(&gpu.queue, 0, row, &mut budget).unwrap();
        assert_eq!(models.shadow_revision(), shadow_revision);
        let dark = frame(&gpu, &mut terrain, Some(&mut models), camera).0;
        let dark_pixels = dark
            .chunks_exact(4)
            .filter(|p| p[..3] == [0; 3] && p[3] == 255)
            .count();
        assert!(
            dark_pixels > 100,
            "ports/models leaked energy in certified darkness: {dark_pixels}"
        );
        // A public annotation cannot borrow a machine's emissive texture
        // energy. Modes must not change any dark image, including glow assets.
        for machine in kind::INFUSER..=kind::PUMP {
            row.kind = machine;
            row.yaw = 0.;
            row.reserved[3] = 0x80000000;
            models.update(&gpu.queue, 0, row, &mut budget).unwrap();
            let none = frame(&gpu, &mut terrain, Some(&mut models), camera).0;
            row.reserved[3] = 0x80000000 | (0..6).map(|f| 1 << (3 * f)).sum::<u32>();
            models.update(&gpu.queue, 0, row, &mut budget).unwrap();
            let input = frame(&gpu, &mut terrain, Some(&mut models), camera).0;
            assert_eq!(
                none, input,
                "machine{machine} ports borrowed emissive energy in darkness"
            );
        }
        assert_eq!(
            catalog,
            read_model(&gpu, &models),
            "port edits must not rewrite model vertices"
        );
        let idle = models.update(&gpu.queue, 0, row, &mut budget).unwrap();
        assert_eq!(idle.upload_bytes, 0);
        assert_eq!(models.take_work().pipeline_compilations, 0);
        println!(
            "M7 actualMachinePorts yaw4*mode5 visibleChanged{changed}px, dark{dark_pixels}px, updates64B/cpuVertices0/catalogBITWISEsame/shadowRevisionUnchanged/runtimePipelines0"
        );
    });
}
