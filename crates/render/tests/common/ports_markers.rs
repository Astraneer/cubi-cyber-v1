use super::*;
use render::{
    block_models::PORT_MARKER_ONLY,
    light_volume::PackedLight,
    shadows::{DynamicShadowCaster, ShadowConfig, ShadowGpu},
    terrain_mesh::{CellMaterial, MeshScratch, PageHalo, PageMesh, mesh_page},
};
#[test]
fn cube_machine_annotations_have_no_body_or_shadow_and_preserve_terrain() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let mut terrain = terrain(&gpu);
        let camera = Camera::perspective(
            [0.5, 0.5, 3.5],
            [0.5, 0.5, 0.5],
            [0., 1., 0.],
            0.8,
            256. / 192.,
            0.1,
            20.,
        );
        let empty = frame(&gpu, &mut terrain, None, camera).0;
        let mut halo = PageHalo::filled(0);
        halo.set(0, 0, 0, 1);
        let mut mesh = PageMesh::default();
        mesh_page(
            &halo,
            |v| {
                if v == 0 {
                    CellMaterial::EMPTY
                } else {
                    CellMaterial::opaque(3)
                }
            },
            &mut MeshScratch::new(),
            &mut mesh,
        );
        terrain
            .upload_page(
                &gpu.queue,
                None,
                1,
                [
                    [1., 0., 0., 0.],
                    [0., 1., 0., 0.],
                    [0., 0., 1., 0.],
                    [0., 0., 0., 1.],
                ],
                &mesh,
                &mut UploadBudget::new(100000),
            )
            .unwrap();
        let body = frame(&gpu, &mut terrain, None, camera).0;
        let body_pixels = body
            .chunks_exact(4)
            .zip(empty.chunks_exact(4))
            .filter(|(a, b)| a != b)
            .count();
        assert!(body_pixels > 1000);
        let mut shadow = ShadowGpu::new(
            &gpu.device,
            &gpu.queue,
            terrain.geometry(),
            ShadowConfig {
                resolution: 64,
                half_widths: [4., 8., 16.],
                depth_half_extent: 32.,
                dynamic_casters: true,
                ..ShadowConfig::default()
            },
            [0.5, 0.5, 0.5],
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
        models
            .reserve_slots(&gpu.queue, 4, &mut UploadBudget::new(256))
            .unwrap();
        models.take_work();
        let catalog = read_model(&gpu, &models);
        let mut row = BlockInstance {
            position: [0.5, 0., 0.5],
            kind: kind::CUBE_MACHINE,
            flags: PORT_MARKER_ONLY,
            light: PackedLight::new(0, 0, [15; 3]).unwrap().raw(),
            ..BlockInstance::default()
        };
        let mut budget = UploadBudget::new(10000);
        models.upload(&gpu.queue, &[row], &mut budget).unwrap();
        assert_eq!(models.shadow_revision(), 0);
        let absent = frame(&gpu, &mut terrain, Some(&mut models), camera).0;
        assert_eq!(body, absent, "absent hint must draw no marker/body");
        for _ in 0..3 {
            let mut encoder = gpu.device.create_command_encoder(&Default::default());
            let work = shadow
                .encode_dynamic(&gpu.queue, &mut encoder, &mut models)
                .unwrap();
            assert_eq!(work.dynamic_draw_calls, 0);
            assert_eq!(work.dynamic_cull_dispatches, 0);
            gpu.queue.submit([encoder.finish()]);
        }
        let depth = shadow_depths(&gpu, shadow.texture(), 64);
        row.reserved[3] = 0x80000000 | (1 << 9);
        models.update(&gpu.queue, 0, row, &mut budget).unwrap();
        let annotated = frame(&gpu, &mut terrain, Some(&mut models), camera).0;
        let changed = annotated
            .chunks_exact(4)
            .zip(body.chunks_exact(4))
            .filter(|(a, b)| a != b)
            .count();
        assert!(
            changed > 20 && changed < body_pixels / 3,
            "marker duplicated body: changed{changed}/body{body_pixels}"
        );
        let mut poses = vec![row];
        for step in 0..4 {
            match step {
                0 => poses[0].position[0] += 1.,
                1 => poses[0].flags |= render::block_models::HIDDEN,
                2 => poses.push(row),
                _ => poses.clear(),
            }
            models.upload(&gpu.queue, &poses, &mut budget).unwrap();
            assert_eq!(
                models.shadow_revision(),
                0,
                "annotation edit/add/remove invalidated shadows"
            );
            let mut encoder = gpu.device.create_command_encoder(&Default::default());
            let work = shadow
                .encode_dynamic(&gpu.queue, &mut encoder, &mut models)
                .unwrap();
            assert_eq!(work.dynamic_restore_pixels, 0);
            assert_eq!(work.dynamic_draw_calls, 0);
            assert_eq!(work.caster_dispatches, 0);
            gpu.queue.submit([encoder.finish()]);
        }
        assert_eq!(
            depth,
            shadow_depths(&gpu, shadow.texture(), 64),
            "annotations altered terrain shadow"
        );
        assert_eq!(catalog, read_model(&gpu, &models));
        println!(
            "M7 classicCubeMarkers actualTerrainBody{body_pixels}px markerOnly{changed}px absentBITWISEsame/shadowDepthBITWISEsame/revision0/noNewVertices/noNewPipelines"
        );
    });
}
