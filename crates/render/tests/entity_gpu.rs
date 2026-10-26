#![cfg(not(target_arch = "wasm32"))]
use render::{
    OutputTransfer,
    entity_gpu::{EntityConfig, EntityError, EntityGpu, EntityInstance},
    light_volume::PackedLight,
    terrain_gpu::{AtlasImage, Camera, CullingMode, GpuConfig, TerrainGpu, UploadBudget},
};
#[global_allocator]
static ALLOCATOR: telemetry::CountingAllocator<std::alloc::System> =
    telemetry::CountingAllocator::new(std::alloc::System);
fn terrain(gpu: &render::Gpu) -> TerrainGpu {
    TerrainGpu::new(
        &gpu.device,
        &gpu.queue,
        wgpu::TextureFormat::Rgba8Unorm,
        256,
        192,
        OutputTransfer::Attachment,
        GpuConfig {
            max_pages: 8,
            max_quads: 8192,
            occlusion: false,
        },
        &AtlasImage::donor().unwrap(),
    )
    .unwrap()
}
fn camera() -> Camera {
    Camera::perspective(
        [0., 2.4, 14.],
        [0., 1., 0.],
        [0., 1., 0.],
        1.,
        256. / 192.,
        0.1,
        200.,
    )
}
fn instance(kind: u32) -> EntityInstance {
    EntityInstance {
        position: [(kind as f32 - 3.5) * 1.8, 0., 0.],
        body_yaw: 0.,
        head_yaw: 0.,
        pitch: 0.,
        limb_phase: 0.,
        limb_amount: 0.5,
        kind,
        flags: if kind >= 6 { 32 } else { 0 },
        color: if kind == 2 {
            5
        } else if kind >= 6 {
            3
        } else {
            0
        },
        id_low: kind + 1,
        id_high: 0,
        death_roll: 0.,
        scale: if kind == 6 {
            0.25
        } else if kind == 7 {
            0.5
        } else {
            1.
        },
        reserved: 0.,
    }
}
fn read(gpu: &render::Gpu, buffer: &wgpu::Buffer) -> Vec<u8> {
    let (tx, rx) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    rx.recv().unwrap().unwrap();
    let bytes = buffer.slice(..).get_mapped_range().to_vec();
    buffer.unmap();
    bytes
}
fn read_model(gpu: &render::Gpu, entities: &EntityGpu) -> Vec<u8> {
    let size = entities.model_vertex_buffer().size();
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("immutable entity vertex catalog oracle"),
        size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_buffer_to_buffer(entities.model_vertex_buffer(), 0, &buffer, 0, size);
    gpu.queue.submit([encoder.finish()]);
    read(gpu, &buffer)
}
fn frame(
    gpu: &render::Gpu,
    terrain: &mut TerrainGpu,
    entities: Option<&mut EntityGpu>,
    camera: Camera,
) -> (Vec<u8>, [u32; 8]) {
    let output = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("actual instanced mob HDR composite"),
        size: wgpu::Extent3d {
            width: 256,
            height: 192,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = output.create_view(&Default::default());
    let pixels = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("entity scene image oracle"),
        size: 256 * 192 * 4,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let counts = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("actual entity indirect counts"),
        size: 128,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    if let Some(entities) = entities {
        entities
            .prepare_frame(&gpu.queue, &mut encoder, camera)
            .unwrap();
        let work = terrain
            .encode_frame_with_extra(
                &gpu.queue,
                &mut encoder,
                &view,
                &camera,
                CullingMode::Frustum,
                |pass| entities.draw(pass),
            )
            .unwrap();
        assert!(work.draw_calls >= 8);
        encoder.copy_buffer_to_buffer(entities.indirect_buffer(), 0, &counts, 0, 128);
    } else {
        terrain
            .encode_frame(
                &gpu.queue,
                &mut encoder,
                &view,
                &camera,
                CullingMode::Frustum,
            )
            .unwrap();
    }
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &output,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &pixels,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(256 * 4),
                rows_per_image: Some(192),
            },
        },
        wgpu::Extent3d {
            width: 256,
            height: 192,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([encoder.finish()]);
    let image = read(gpu, &pixels);
    let counts = read(gpu, &counts);
    (
        image,
        std::array::from_fn(|i| {
            u32::from_le_bytes(counts[i * 16 + 4..i * 16 + 8].try_into().unwrap())
        }),
    )
}
#[test]
fn static_catalog_gpu_poses_culls_and_shades_all_mob_item_kinds() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let mut terrain = terrain(&gpu);
        let items = render::entity_models::item_atlas(&vec![255; 16 * 16 * 4 * 4], 4).unwrap();
        let mut entities = EntityGpu::new_with_items(
            &gpu.device,
            &gpu.queue,
            terrain.geometry(),
            EntityConfig {
                max_entities: 1024,
                atlas_layers: 186,
            },
            None,
            &items,
        )
        .unwrap();
        let startup = entities.take_work();
        assert_eq!(startup.startup_vertices, 1776);
        assert_eq!(startup.pipeline_compilations, 3);
        let model_before = read_model(&gpu, &entities);
        let mut instances = (0..8).map(instance).collect::<Vec<_>>();
        let mut hidden = instance(0);
        hidden.position = [1000., 0., 0.];
        instances.push(hidden);
        let work = entities
            .upload(&gpu.queue, &instances, &[], &mut UploadBudget::new(1000000))
            .unwrap();
        assert_eq!(work.instance_records, 9);
        assert_eq!(work.upload_bytes, 9 * 68);
        assert_eq!(work.cpu_vertices, 0);
        let (sky, _) = frame(&gpu, &mut terrain, None, camera());
        let (actual, counts) = frame(&gpu, &mut terrain, Some(&mut entities), camera());
        assert_eq!(
            counts, [1; 8],
            "GPU frustum must hide the distant extra pig"
        );
        let colored = actual
            .chunks_exact(4)
            .zip(sky.chunks_exact(4))
            .filter(|(a, b)| a != b)
            .count();
        assert!(
            colored > 1000,
            "all-kind catalog scene missing: {colored}pixels"
        );
        assert!(actual.chunks_exact(4).all(|p| p[3] == 255));
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/artifacts/m7-entities.png");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let file = std::fs::File::create(path).unwrap();
        let mut png = png::Encoder::new(file, 256, 192);
        png.set_color(png::ColorType::Rgba);
        png.set_depth(png::BitDepth::Eight);
        png.write_header()
            .unwrap()
            .write_image_data(&actual)
            .unwrap();
        instances[2].flags |= 2;
        instances[0].limb_phase = 4.;
        instances[0].head_yaw = 45.;
        instances[4].flags |= 16;
        instances[4].death_roll = 45.;
        entities
            .upload(&gpu.queue, &instances, &[], &mut UploadBudget::new(1000000))
            .unwrap();
        let (posed, _) = frame(&gpu, &mut terrain, Some(&mut entities), camera());
        assert_ne!(
            posed, actual,
            "GPU leg/head/shear/hurt/death pose changes were ignored"
        );
        assert_eq!(
            read_model(&gpu, &entities),
            model_before,
            "a pose update rebuilt static model vertices"
        );
        assert_eq!(entities.take_work().cpu_vertices, 0);
        let mut bad = instances.clone();
        bad[0].position[0] += 1.;
        assert_eq!(
            entities.upload(&gpu.queue, &bad, &[], &mut UploadBudget::new(0)),
            Err(EntityError::UploadBudget)
        );
        assert_eq!(
            frame(&gpu, &mut terrain, Some(&mut entities), camera()).0,
            posed,
            "failed instance budget published a partial update"
        );
        println!(
            "M7 GPUcatalog1776vertices kinds=8 coloredPixels={colored},culledExtraPig=1,poseVertexUploads=0"
        );
    });
}
#[test]
fn thousand_instances_upload_only_records_and_idle_upload_allocates_nothing() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let mut terrain = terrain(&gpu);
        let mut entities = EntityGpu::new(
            &gpu.device,
            &gpu.queue,
            terrain.geometry(),
            EntityConfig {
                max_entities: 1024,
                atlas_layers: 186,
            },
            None,
        )
        .unwrap();
        entities.take_work();
        let instances = (0..1000)
            .map(|index| {
                let mut value = instance(index % 5);
                value.position = [(index % 25) as f32 - 12., 0., (index / 25) as f32 * -2.];
                value.id_low = index + 1;
                value
            })
            .collect::<Vec<_>>();
        let lights = vec![PackedLight::new(15, 0, [0; 3]).unwrap(); 1000];
        let mut budget = UploadBudget::new(1000000);
        let work = entities
            .upload(&gpu.queue, &instances, &lights, &mut budget)
            .unwrap();
        assert_eq!(work.instance_records, 1000);
        assert_eq!(work.upload_bytes, 68000);
        assert_eq!(work.cpu_vertices, 0);
        let (_, visible) = frame(&gpu, &mut terrain, Some(&mut entities), camera());
        assert!(
            visible.iter().sum::<u32>() >= 900,
            "1000-instance GPU draw did not run visible catalog records: {visible:?}"
        );
        let control = telemetry::AllocationScope::begin().unwrap();
        let bytes = vec![1u8; std::hint::black_box(32)];
        std::hint::black_box(&bytes);
        drop(bytes);
        assert!(control.finish().allocating_calls() > 0);
        let scope = telemetry::AllocationScope::begin().unwrap();
        for _ in 0..100 {
            let work = entities
                .upload(&gpu.queue, &instances, &lights, &mut budget)
                .unwrap();
            assert_eq!(work.upload_bytes, 0);
            assert_eq!(work.cpu_vertices, 0);
        }
        assert_eq!(scope.finish().allocating_calls(), 0);
        println!(
            "M7 1000entities=68000Bupload,CPUvertices=0,idle100cyclesuploads=0allocations=0; RTXmillisecondsunmeasured"
        );
    });
}

fn shadow_depths(gpu: &render::Gpu, texture: &wgpu::Texture, resolution: u32) -> Vec<f32> {
    let row = (resolution * 4).div_ceil(256) * 256;
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("independent model shadow depth image"),
        size: u64::from(row) * u64::from(resolution) * 3,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::DepthOnly,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(resolution),
            },
        },
        wgpu::Extent3d {
            width: resolution,
            height: resolution,
            depth_or_array_layers: 3,
        },
    );
    gpu.queue.submit([encoder.finish()]);
    read(gpu, &buffer)
        .chunks_exact(row as usize)
        .flat_map(|row| {
            row[..resolution as usize * 4]
                .chunks_exact(4)
                .map(|v| f32::from_le_bytes(v.try_into().unwrap()))
        })
        .collect()
}
#[test]
fn moving_model_shadows_preserve_static_cache_and_retire_old_silhouettes() {
    pollster::block_on(async {
        use render::shadows::{ShadowConfig, ShadowGpu};
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let terrain = terrain(&gpu);
        let mut shadows = ShadowGpu::new(
            &gpu.device,
            &gpu.queue,
            terrain.geometry(),
            ShadowConfig {
                resolution: 128,
                half_widths: [16., 32., 64.],
                depth_half_extent: 32.,
                max_updates_per_frame: 3,
                dynamic_casters: true,
                ..ShadowConfig::default()
            },
            [0.; 3],
            [0., 1., 0.],
        )
        .unwrap();
        let mut entities = EntityGpu::new(
            &gpu.device,
            &gpu.queue,
            terrain.geometry(),
            EntityConfig {
                max_entities: 8,
                atlas_layers: 186,
            },
            Some(&shadows),
        )
        .unwrap();
        let mut mob = instance(4);
        mob.position = [0.; 3];
        entities
            .upload(&gpu.queue, &[mob], &[], &mut UploadBudget::new(1000000))
            .unwrap();
        let encode = |shadows: &mut ShadowGpu, entities: &mut EntityGpu| {
            let mut encoder = gpu.device.create_command_encoder(&Default::default());
            let work = shadows
                .encode_dynamic(&gpu.queue, &mut encoder, entities)
                .unwrap();
            gpu.queue.submit([encoder.finish()]);
            gpu.device.poll(wgpu::PollType::Wait).unwrap();
            work
        };
        let initial = encode(&mut shadows, &mut entities);
        assert_eq!(initial.cascade_updates, 3);
        assert_eq!(initial.dynamic_draw_calls, 24);
        let static_before = shadow_depths(&gpu, shadows.texture(), 128);
        assert!(static_before.iter().all(|depth| *depth == 0.));
        let before = shadow_depths(&gpu, shadows.sampled_texture(), 128);
        assert!(
            before.iter().filter(|depth| **depth > 0.).count() > 4,
            "actual model caster must write a nonempty shadow"
        );
        mob.position[0] = 3.;
        entities
            .upload(&gpu.queue, &[mob], &[], &mut UploadBudget::new(1000000))
            .unwrap();
        let moved = encode(&mut shadows, &mut entities);
        assert_eq!(
            moved.cascade_updates, 0,
            "moving model re-rendered static terrain"
        );
        assert_eq!(moved.dynamic_copy_bytes, 3 * 128 * 128 * 4);
        let after = shadow_depths(&gpu, shadows.sampled_texture(), 128);
        assert_ne!(before, after);
        assert!(
            before
                .iter()
                .zip(&after)
                .any(|(old, new)| *old > 0. && *new == 0.),
            "old caster silhouette was not cleared"
        );
        assert_eq!(shadow_depths(&gpu, shadows.texture(), 128), static_before);
        let idle = encode(&mut shadows, &mut entities);
        assert_eq!(idle.dynamic_copy_bytes, 0);
        assert_eq!(idle.dynamic_draw_calls, 0);
        assert_eq!(idle.dynamic_cull_dispatches, 0);
        mob.flags |= 16;
        entities
            .upload(&gpu.queue, &[mob], &[], &mut UploadBudget::new(1000000))
            .unwrap();
        assert_eq!(
            encode(&mut shadows, &mut entities).dynamic_copy_bytes,
            0,
            "hurt flash invalidated unchanged shadow geometry"
        );
        entities
            .upload(
                &gpu.queue,
                &[mob],
                &[PackedLight::new(15, 15, [15, 0, 0]).unwrap()],
                &mut UploadBudget::new(1000000),
            )
            .unwrap();
        assert_eq!(
            encode(&mut shadows, &mut entities).dynamic_copy_bytes,
            0,
            "a lamp color change invalidated shadow geometry"
        );
        entities
            .upload(&gpu.queue, &[], &[], &mut UploadBudget::new(1000000))
            .unwrap();
        let removed = encode(&mut shadows, &mut entities);
        assert_eq!(removed.dynamic_draw_calls, 0);
        assert!(removed.dynamic_copy_bytes > 0);
        assert_eq!(
            shadow_depths(&gpu, shadows.sampled_texture(), 128),
            static_before,
            "removed model left its shadow behind"
        );
        println!(
            "M7dynamicshadow moves: staticDraws=0,copyBytes={},draws={},idleCopies=0,idleDraws=0; default4096worstGPUcopy=192MiB",
            moved.dynamic_copy_bytes, moved.dynamic_draw_calls
        );
    });
}
#[test]
fn first_person_human_hides_color_but_keeps_shadow_without_static_redraw() {
    pollster::block_on(async {
        use render::{
            entity_gpu::{CROUCH, PRIMARY_HIDDEN},
            shadows::{ShadowConfig, ShadowGpu},
        };
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let mut terrain = terrain(&gpu);
        let mut shadows = ShadowGpu::new(
            &gpu.device,
            &gpu.queue,
            terrain.geometry(),
            ShadowConfig {
                resolution: 128,
                half_widths: [8., 16., 32.],
                depth_half_extent: 32.,
                max_updates_per_frame: 3,
                dynamic_casters: true,
                ..ShadowConfig::default()
            },
            [0.; 3],
            [0., 1., 0.],
        )
        .unwrap();
        let mut models = EntityGpu::new(
            &gpu.device,
            &gpu.queue,
            terrain.geometry(),
            EntityConfig {
                max_entities: 1,
                atlas_layers: 186,
            },
            Some(&shadows),
        )
        .unwrap();
        let mut human = instance(5);
        human.position = [0.; 3];
        models
            .upload(&gpu.queue, &[human], &[], &mut UploadBudget::new(68))
            .unwrap();
        let encode = |shadows: &mut ShadowGpu, models: &mut EntityGpu| {
            let mut encoder = gpu.device.create_command_encoder(&Default::default());
            let work = shadows
                .encode_dynamic(&gpu.queue, &mut encoder, models)
                .unwrap();
            gpu.queue.submit([encoder.finish()]);
            gpu.device.poll(wgpu::PollType::Wait).unwrap();
            work
        };
        encode(&mut shadows, &mut models);
        let depth = shadow_depths(&gpu, shadows.sampled_texture(), 128);
        assert!(depth.iter().any(|v| *v > 0.));
        let sky = frame(&gpu, &mut terrain, None, camera()).0;
        let visible = frame(&gpu, &mut terrain, Some(&mut models), camera()).0;
        assert_ne!(sky, visible);
        human.flags |= PRIMARY_HIDDEN;
        models
            .upload(&gpu.queue, &[human], &[], &mut UploadBudget::new(64))
            .unwrap();
        let hidden_work = encode(&mut shadows, &mut models);
        assert_eq!(hidden_work.dynamic_copy_bytes, 0);
        assert_eq!(hidden_work.cascade_updates, 0);
        assert_eq!(
            frame(&gpu, &mut terrain, Some(&mut models), camera()).0,
            sky,
            "firstpersonhuman remained visible"
        );
        assert_eq!(
            shadow_depths(&gpu, shadows.sampled_texture(), 128),
            depth,
            "camera-only hiding changed actual shadow"
        );
        human.flags = CROUCH;
        models
            .upload(&gpu.queue, &[human], &[], &mut UploadBudget::new(64))
            .unwrap();
        assert!(encode(&mut shadows, &mut models).dynamic_copy_bytes > 0);
        let crouch = frame(&gpu, &mut terrain, Some(&mut models), camera()).0;
        assert_ne!(crouch, visible);
        assert_ne!(crouch, sky);
        println!(
            "M7 localhuman firstpersoncolorhidden shadowBITWISEsame copies0staticdraws0; crouchGPU changespose"
        );
    });
}
#[test]
fn nonblock_sprites_use_separate_item_array_with_real_cutout_alpha() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let mut terrain = terrain(&gpu);
        let mut icon = instance(7);
        icon.position = [0.; 3];
        icon.scale = 1.;
        icon.color = 0;
        let mut no_icons = EntityGpu::new(
            &gpu.device,
            &gpu.queue,
            terrain.geometry(),
            EntityConfig {
                max_entities: 1,
                atlas_layers: 186,
            },
            None,
        )
        .unwrap();
        assert_eq!(
            no_icons.upload(&gpu.queue, &[icon], &[], &mut UploadBudget::new(68)),
            Err(EntityError::InvalidInstance)
        );
        let mut rgba = vec![0; 16 * 16 * 4];
        for y in 0..16 {
            for x in 0..16 {
                let i = (x + y * 16) * 4;
                rgba[i..i + 4].copy_from_slice(&[
                    255,
                    0,
                    255,
                    if (4..12).contains(&x) && (4..12).contains(&y) {
                        0
                    } else {
                        255
                    },
                ]);
            }
        }
        let atlas = render::entity_models::item_atlas(&rgba, 1).unwrap();
        let mut icons = EntityGpu::new_with_items(
            &gpu.device,
            &gpu.queue,
            terrain.geometry(),
            EntityConfig {
                max_entities: 1,
                atlas_layers: 186,
            },
            None,
            &atlas,
        )
        .unwrap();
        icons
            .upload(&gpu.queue, &[icon], &[], &mut UploadBudget::new(68))
            .unwrap();
        let c = Camera::perspective(
            [0., 0.6, 4.],
            [0., 0.6, 0.],
            [0., 1., 0.],
            1.,
            256. / 192.,
            0.1,
            200.,
        );
        let sky = frame(&gpu, &mut terrain, None, c).0;
        let image = frame(&gpu, &mut terrain, Some(&mut icons), c).0;
        let colored = image
            .chunks_exact(4)
            .zip(sky.chunks_exact(4))
            .filter(|(a, b)| a != b)
            .count();
        assert!(colored > 500);
        let magenta = image
            .chunks_exact(4)
            .filter(|p| {
                u32::from(p[0]) > u32::from(p[1]) * 2 && u32::from(p[2]) > u32::from(p[1]) * 2
            })
            .count();
        assert!(
            magenta > 500,
            "item array was replaced with a terrain block layer"
        );
        let i = (128 + 96 * 256) * 4;
        assert_eq!(
            &image[i..i + 4],
            &sky[i..i + 4],
            "transparent item hole was made opaque"
        );
        println!(
            "M7 separate item sprites actualmagenta{magenta}px,cutoutcenterSkyExact,noSpritesRejected; terrainatlasunchanged"
        );
    });
}

#[test]
fn block_drops_preserve_all_six_face_layers_in_one_64_byte_instance() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let colors = [
            [255, 0, 0, 255],
            [0, 255, 0, 255],
            [0, 0, 255, 255],
            [255, 255, 0, 255],
            [255, 0, 255, 255],
            [0, 255, 255, 255],
        ];
        let rgba = colors
            .into_iter()
            .flat_map(|color| std::iter::repeat_n(color, 256).flatten())
            .collect::<Vec<_>>();
        let atlas = render::entity_models::item_atlas(&rgba, 6).unwrap();
        let mut terrain = TerrainGpu::new(
            &gpu.device,
            &gpu.queue,
            wgpu::TextureFormat::Rgba8Unorm,
            256,
            192,
            OutputTransfer::Attachment,
            GpuConfig {
                max_pages: 8,
                max_quads: 8192,
                occlusion: false,
            },
            &atlas,
        )
        .unwrap();
        let mut entities = EntityGpu::new(
            &gpu.device,
            &gpu.queue,
            terrain.geometry(),
            EntityConfig {
                max_entities: 1,
                atlas_layers: 6,
            },
            None,
        )
        .unwrap();
        let mut cube = instance(6);
        cube.position = [0.; 3];
        cube.scale = 1.;
        cube.color = 0;
        cube.set_block_layers([0, 1, 2, 3, 4, 5]).unwrap();
        assert_eq!(std::mem::size_of_val(&cube), 64);
        assert_eq!(cube.block_layers(), Some([0, 1, 2, 3, 4, 5]));
        entities
            .upload(&gpu.queue, &[cube], &[], &mut UploadBudget::new(68))
            .unwrap();
        let normals = [
            [0., -1., 0.],
            [0., 1., 0.],
            [0., 0., -1.],
            [0., 0., 1.],
            [-1., 0., 0.],
            [1., 0., 0.],
        ];
        for (face, normal) in normals.into_iter().enumerate() {
            let eye = [normal[0] * 4., 0.6 + normal[1] * 4., normal[2] * 4.];
            let up = if normal[1] != 0. {
                [0., 0., 1.]
            } else {
                [0., 1., 0.]
            };
            let camera = Camera::perspective(eye, [0., 0.6, 0.], up, 1., 256. / 192., 0.1, 100.);
            let pixels = frame(&gpu, &mut terrain, Some(&mut entities), camera).0;
            let p = &pixels[(128 + 96 * 256) * 4..][..4];
            for channel in 0..3 {
                if colors[face][channel] == 0 {
                    assert_eq!(p[channel], 0, "wrong layer on face {face}: {p:?}");
                } else {
                    assert!(p[channel] > 80, "missing face {face}: {p:?}");
                }
            }
        }
        let before = entities.take_work();
        assert_eq!(before.instance_records, 1);
        assert_eq!(before.cpu_vertices, 0);
        assert_eq!(
            cube.set_block_layers([0, 1, 2, 3, 4, 256]),
            Err(EntityError::InvalidInstance)
        );
        let mut bad = cube;
        bad.pitch = f32::from_bits(0x3f800000 | 6);
        assert_eq!(
            entities.upload(&gpu.queue, &[bad], &[], &mut UploadBudget::new(68)),
            Err(EntityError::InvalidInstance)
        );
        println!("M7 blockdrop6independentfaceviewsPASS,one64Brecord,0CPUvertices,finitepacking");
    });
}

#[test]
fn optional_atmosphere_lighting_tracks_day_and_dark_night_without_recompilation() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    let mut terrain = terrain(&gpu);
    let mut budget = UploadBudget::new(1_000_000);
    let mut atmosphere = render::atmosphere::AtmosphereGpu::new(
        &gpu.device,
        &gpu.queue,
        render::atmosphere::AtmosphereConfig {
            transmittance_size: [32, 8],
            sky_size: [16, 8],
            transmittance_steps: 32,
            sky_steps: 16,
        },
        render::atmosphere::AtmosphereState {
            sun_direction: [0., -1., 0.],
            altitude_km: 0.072,
        },
        &mut budget,
    )
    .unwrap();
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    atmosphere
        .encode_update(&mut encoder, &mut 1_000_000)
        .unwrap();
    gpu.queue.submit([encoder.finish()]);
    let mut consumer = EntityGpu::new_with_environment(
        &gpu.device,
        &gpu.queue,
        terrain.geometry(),
        EntityConfig {
            max_entities: 8,
            atlas_layers: 186,
        },
        None,
        None,
        &atmosphere,
    )
    .unwrap();
    let mut pig = instance(0);
    pig.position = [0.; 3];
    consumer
        .upload(
            &gpu.queue,
            &[pig],
            &[PackedLight::new(15, 15, [0; 3]).unwrap()],
            &mut budget,
        )
        .unwrap();
    consumer.take_work();
    let dark = frame(&gpu, &mut terrain, Some(&mut consumer), camera()).0;
    let dark_pixels = dark
        .chunks_exact(4)
        .filter(|p| p[3] == 255 && p[..3] == [0, 0, 0])
        .count();
    assert!(
        dark_pixels > 50,
        "night receiver must be dark: {dark_pixels}"
    );
    atmosphere
        .set_state(
            &gpu.queue,
            render::atmosphere::AtmosphereState::default(),
            &mut budget,
        )
        .unwrap();
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    atmosphere
        .encode_update(&mut encoder, &mut 1_000_000)
        .unwrap();
    gpu.queue.submit([encoder.finish()]);
    let day = frame(&gpu, &mut terrain, Some(&mut consumer), camera()).0;
    let lit_pixels = day
        .chunks_exact(4)
        .zip(dark.chunks_exact(4))
        .filter(|(a, b)| b[3] == 255 && b[..3] == [0, 0, 0] && a[..3].iter().any(|v| *v > 3))
        .count();
    assert!(
        lit_pixels > 50,
        "day receivers must react to shared atmosphere: {lit_pixels}"
    );
    assert_eq!(
        consumer.take_work().pipeline_compilations,
        0,
        "day change must reuse pipelines"
    );
    println!(
        "M10_entity_gpu_ATMOSPHERE_GPU_OK consumer=entity_gpu night_black={dark_pixels} day_lit={lit_pixels} runtime_pipeline_compilations=0"
    );
}

#[test]
fn public_pipe_packets_keep_center_without_drop_bob_or_spin() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let mut terrain = terrain(&gpu);
        let items = render::entity_models::item_atlas(&vec![255; 16 * 16 * 4], 1).unwrap();
        let mut entities = EntityGpu::new_with_items(
            &gpu.device,
            &gpu.queue,
            terrain.geometry(),
            EntityConfig {
                max_entities: 2,
                atlas_layers: 186,
            },
            None,
            &items,
        )
        .unwrap();
        let vertices = read_model(&gpu, &entities);
        let mut rows = [instance(6), instance(7)];
        for (i, row) in rows.iter_mut().enumerate() {
            row.position = [if i == 0 { -0.45 } else { 0.45 }, 1., 0.];
            row.color = 0;
            row.flags = 32 | render::entity_gpu::PIPE;
            row.scale = if i == 0 { 0.2 } else { 0.4 };
        }
        let c = Camera::perspective(
            [0., 1., 3.],
            [0., 1., 0.],
            [0., 1., 0.],
            1.,
            256. / 192.,
            0.1,
            100.,
        );
        let sky = frame(&gpu, &mut terrain, None, c).0;
        entities
            .upload(&gpu.queue, &rows, &[], &mut UploadBudget::new(136))
            .unwrap();
        let (first, counts) = frame(&gpu, &mut terrain, Some(&mut entities), c);
        assert_eq!((counts[6], counts[7]), (1, 1));
        let mask = first
            .chunks_exact(4)
            .zip(sky.chunks_exact(4))
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .map(|(i, _)| i)
            .collect::<Vec<_>>();
        assert!(mask.len() > 100, "pipe records were hidden or absent");
        let mean_y = mask.iter().map(|i| (i / 256) as f64 + 0.5).sum::<f64>() / mask.len() as f64;
        assert!(
            (mean_y - 96.).abs() < 1.5,
            "pipe center moved from canonical point: y{mean_y}"
        );
        for row in &mut rows {
            row.limb_phase = 73.;
            row.reserved = 2.75;
        }
        entities
            .upload(&gpu.queue, &rows, &[], &mut UploadBudget::new(128))
            .unwrap();
        let fixed = frame(&gpu, &mut terrain, Some(&mut entities), c).0;
        assert_eq!(fixed, first, "pipe inherited age-dependent drop bob/spin");
        for row in &mut rows {
            row.flags &= !render::entity_gpu::PIPE;
            row.limb_phase = 0.;
            row.reserved = 0.;
        }
        entities
            .upload(&gpu.queue, &rows, &[], &mut UploadBudget::new(128))
            .unwrap();
        let drop_a = frame(&gpu, &mut terrain, Some(&mut entities), c).0;
        for row in &mut rows {
            row.limb_phase = 40.;
        }
        entities
            .upload(&gpu.queue, &rows, &[], &mut UploadBudget::new(128))
            .unwrap();
        let drop_b = frame(&gpu, &mut terrain, Some(&mut entities), c).0;
        assert_ne!(drop_a, drop_b, "ordinary drops lost their animation");
        let mut invalid = instance(0);
        invalid.flags = 32 | render::entity_gpu::PIPE;
        assert_eq!(
            entities.upload(&gpu.queue, &[invalid], &[], &mut UploadBudget::new(1000)),
            Err(EntityError::InvalidInstance)
        );
        assert_eq!(read_model(&gpu, &entities), vertices);
        assert_eq!(entities.take_work().cpu_vertices, 0);
        println!(
            "M9 pipe GPU cube/icon {}px centerY{mean_y:.4},age73 bitwise unchanged,drop animated,CPUvertices0",
            mask.len()
        );
    });
}
