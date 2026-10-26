use super::*;
use render::{
    light_volume::PackedLight,
    shadows::{DynamicShadowCaster, ShadowConfig, ShadowGpu},
};
#[test]
fn colored_connector_glass_is_transparent_record_only_and_does_not_cast_a_shadow() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let mut terrain = terrain(&gpu);
        let shadow = ShadowGpu::new(
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
            Some(&shadow),
        )
        .unwrap();
        models.take_work();
        let catalog = read_model(&gpu, &models);
        let camera = Camera::perspective(
            [1.3, 1.3, 2.8],
            [0., 0.5, 0.],
            [0., 1., 0.],
            0.8,
            256. / 192.,
            0.1,
            20.,
        );
        let mut row = BlockInstance {
            kind: kind::TRANSPORTER,
            connections: 0x555,
            light: PackedLight::new(0, 0, [15; 3]).unwrap().raw(),
            ..BlockInstance::default()
        };
        let mut budget = UploadBudget::new(10000);
        models.upload(&gpu.queue, &[row], &mut budget).unwrap();
        let baseline = frame(&gpu, &mut terrain, Some(&mut models), camera).0;
        let revision = models.shadow_revision();
        let mut images = std::collections::BTreeSet::new();
        images.insert(baseline.clone());
        let mut changed_total = 0;
        for color in 1..=18 {
            row.reserved[3] = 0x40000000 | color;
            assert_eq!(
                models
                    .update(&gpu.queue, 0, row, &mut budget)
                    .unwrap()
                    .upload_bytes,
                64
            );
            assert_eq!(models.shadow_revision(), revision);
            let image = frame(&gpu, &mut terrain, Some(&mut models), camera).0;
            let changed = image
                .chunks_exact(4)
                .zip(baseline.chunks_exact(4))
                .filter(|(a, b)| a != b)
                .count();
            assert!(
                changed > 20,
                "connector color{color} did not draw glass: {changed}"
            );
            changed_total += changed;
            assert!(images.insert(image), "connector colors collided{color}");
        }
        row.reserved[3] = 0;
        models.update(&gpu.queue, 0, row, &mut budget).unwrap();
        assert!(
            baseline == frame(&gpu, &mut terrain, Some(&mut models), camera).0,
            "color0 must preserve old pixels"
        );
        row.light = 0;
        models.update(&gpu.queue, 0, row, &mut budget).unwrap();
        let dark = frame(&gpu, &mut terrain, Some(&mut models), camera).0;
        row.reserved[3] = 0x4000000f;
        models.update(&gpu.queue, 0, row, &mut budget).unwrap();
        let dark_colored = frame(&gpu, &mut terrain, Some(&mut models), camera).0;
        let brighter = dark_colored
            .chunks_exact(4)
            .zip(dark.chunks_exact(4))
            .filter(|(a, b)| (0..3).any(|c| a[c] > b[c]))
            .count();
        assert_eq!(
            brighter, 0,
            "zero-light glass may absorb background, never add radiance"
        );
        row.reserved[3] = 0x40000001;
        models.update(&gpu.queue, 0, row, &mut budget).unwrap();
        assert!(
            dark_colored == frame(&gpu, &mut terrain, Some(&mut models), camera).0,
            "zero-light colors must have identical optical coverage"
        );
        assert_eq!(catalog, read_model(&gpu, &models));
        assert_eq!(models.take_work().pipeline_compilations, 0);
        println!(
            "M7 connectorGlass19uniqueImages changed{changed_total}px color0BITWISEsame darknessNoAddedRadiance/coloredCoverageBITWISEsame,64B/update verticesUnchanged shadowRevisionUnchanged runtimeCompile0"
        );
    });
}
