#![cfg(not(target_arch = "wasm32"))]
//! Independent full-scene depth/coverage oracle for the joined 3D near/far cut.
use client::view::horizon::{Config, ViewHorizon};
use core_types::{PageId, SpaceId, Version};
use net::height::{GeometryStamp, HeightKey, HeightTile, Replacement};
use render::terrain_gpu::{
    AtlasImage, Camera, CullingMode, GpuConfig, TerrainGpu, UploadBudget, translation,
};
use render::terrain_mesh::{CellMaterial, MeshScratch, PageHalo, PageMesh, mesh_page};
const SIZE: u32 = 128;
const LOW: [[i32; 3]; 2] = [[-1, 0, -1], [2, 3, 2]];
const HIGH: [[i32; 3]; 2] = [[-1, 5, -1], [2, 8, 2]];
const WIDE_LOW: [[i32; 3]; 2] = [[-2, 0, -2], [3, 3, 3]];
const WIDE_HIGH: [[i32; 3]; 2] = [[-2, 5, -2], [3, 8, 3]];
#[path = "../../render/tests/common/msaa.rs"]
mod sample_locations;
fn page(x: i32, y: i32, z: i32) -> PageId {
    PageId {
        space: SpaceId(0),
        x,
        y,
        z,
    }
}
fn raw(x: i32, y: i32, z: i32) -> u32 {
    if y < 8 {
        1
    } else if y == 71 && (0..16).contains(&x) && (0..16).contains(&z) {
        2
    } else {
        0
    }
}
fn mesh(page: PageId) -> PageMesh {
    let mut halo = PageHalo::filled(0);
    for y in -1..=16 {
        for z in -1..=16 {
            for x in -1..=16 {
                halo.set(
                    x,
                    y,
                    z,
                    raw(
                        page.x * 16 + i32::from(x),
                        page.y * 16 + i32::from(y),
                        page.z * 16 + i32::from(z),
                    ),
                );
            }
        }
    }
    let mut mesh = PageMesh::default();
    let mut scratch = MeshScratch::new();
    mesh_page(
        &halo,
        |raw| {
            if raw == 0 {
                CellMaterial::EMPTY
            } else {
                let mut m = CellMaterial::opaque(3);
                if raw == 2 {
                    m.tint = 0xff00ff00;
                }
                m
            }
        },
        &mut scratch,
        &mut mesh,
    );
    mesh
}
fn terrain(gpu: &render::Gpu) -> TerrainGpu {
    let mut terrain = TerrainGpu::new(
        &gpu.device,
        &gpu.queue,
        wgpu::TextureFormat::Rgba8Unorm,
        SIZE,
        SIZE,
        render::OutputTransfer::Attachment,
        GpuConfig {
            max_pages: 96,
            max_quads: 65536,
            occlusion: false,
        },
        &AtlasImage::donor().unwrap(),
    )
    .unwrap();
    terrain.prepare_scene_sampling(&gpu.device).unwrap();
    terrain
}
fn upload(gpu: &render::Gpu, terrain: &mut TerrainGpu, p: PageId) {
    terrain
        .upload_page(
            &gpu.queue,
            None,
            1,
            translation([p.x as f32 * 16., p.y as f32 * 16., p.z as f32 * 16.]),
            &mesh(p),
            &mut UploadBudget::new(1_000_000),
        )
        .unwrap();
}
fn near(
    gpu: &render::Gpu,
    terrain: &mut TerrainGpu,
    volume: [[i32; 3]; 2],
    missing: Option<PageId>,
) {
    for z in volume[0][2]..volume[1][2] {
        for y in volume[0][1]..volume[1][1] {
            for x in volume[0][0]..volume[1][0] {
                let p = page(x, y, z);
                if missing != Some(p) {
                    upload(gpu, terrain, p);
                }
            }
        }
    }
}
fn geometry(y: i32) -> net::PublicGeometry {
    let mut occupied = [0; 512];
    for i in 0..4096 {
        let x = i % 16;
        let z = i / 16 % 16;
        let local_y = i / 256;
        if raw(x as i32, y * 16 + local_y as i32, z as i32) != 0 {
            occupied[i / 8] |= 1 << (i % 8);
        }
    }
    net::PublicGeometry {
        page: page(0, y, 0),
        source_version: Version(42),
        view_version: 2,
        known: [255; 512],
        occupied,
        palette: if y == 0 || y == 4 {
            vec![net::PublicSurface {
                atlas_layer: 3,
                tint: if y == 4 { 0xff00ff00 } else { u32::MAX },
                kind: net::PublicSurfaceKind::Opaque,
            }]
        } else {
            Vec::new()
        },
        index_bits: 0,
        indices: Vec::new(),
    }
}
fn tile(key: HeightKey) -> Box<HeightTile> {
    let mut known = [255; 37];
    known[36] = 1;
    let exact = key.x == 0 && key.z == 0;
    Box::new(HeightTile {
        key,
        source: if exact { 2 } else { 1 },
        error: None,
        heights: [8; 289],
        known,
        atlas: [3; 256],
        tint: [u32::MAX; 256],
        protected: if exact { [255; 32] } else { [0; 32] },
        replacement: if exact {
            Replacement::Column {
                min_y: 0,
                max_y: 7,
                geometry: (0..8)
                    .map(|y| GeometryStamp {
                        page: page(0, y, 0),
                        source: Version(42),
                        view: 2,
                    })
                    .collect(),
            }
        } else {
            Replacement::None
        },
    })
}
fn horizon(
    gpu: &render::Gpu,
    terrain: &TerrainGpu,
    volume: Option<[[i32; 3]; 2]>,
    radius: u32,
    extras: &[PageId],
) -> (ViewHorizon, Vec<HeightKey>) {
    let mut h = ViewHorizon::prepare(
        &gpu.device,
        &gpu.queue,
        terrain,
        None,
        Config {
            max_tiles: 64,
            max_sources: 128,
            max_proxy_pages: 32,
            horizon_level: 0,
            near_volume: volume,
            ..Config::default()
        },
    )
    .unwrap();
    h.reserve_near_cutout(radius).unwrap();
    if volume.is_some() {
        h.guard_near_cover(&gpu.queue, None, extras, &mut UploadBudget::new(1_000_000))
            .unwrap()
            .unwrap();
    } else {
        h.guard_near(&gpu.queue, 0, [0, 0], &mut UploadBudget::new(1_000_000))
            .unwrap()
            .unwrap();
    }
    let plan = h.plan_task(0, [0, 0]).unwrap().unwrap().execute().unwrap();
    h.complete_plan(plan).unwrap();
    let keys = h.wanted_height().collect::<Vec<_>>();
    for &k in &keys {
        h.ingest_height(tile(k)).unwrap();
    }
    for y in 0..8 {
        h.ingest_geometry(&geometry(y)).unwrap();
    }
    while let Some(task) = h.take_proxy_task() {
        h.complete_proxy(task.execute().unwrap())
            .map_err(|_| ())
            .unwrap();
    }
    for _ in 0..4 {
        h.pump(&gpu.queue, &mut UploadBudget::new(8_000_000), 64, 32)
            .unwrap();
    }
    (h, keys)
}
struct Readback {
    samples: [[f64; 2]; 4],
    ground_bounds: [f64; 4],
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    depth: wgpu::Buffer,
    color: wgpu::Buffer,
}
impl Readback {
    fn new(gpu: &render::Gpu) -> Self {
        let device = &gpu.device;
        let samples = sample_locations::locations(gpu);
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("independent joined depth probe"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Depth,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: true,
                },
                count: None,
            }],
        });
        let shader=device.create_shader_module(wgpu::ShaderModuleDescriptor{label:Some("independent final MSAA depth read"),source:wgpu::ShaderSource::Wgsl(r#"
 @group(0) @binding(0) var source: texture_depth_multisampled_2d;
 @vertex fn vs(@builtin(vertex_index) i:u32)->@builtin(position) vec4<f32>{var p=array<vec2<f32>,3>(vec2(-1.,-1.),vec2(3.,-1.),vec2(-1.,3.));return vec4(p[i],0.,1.);}
 @fragment fn fs(@builtin(position) p:vec4<f32>)->@location(0) vec4<f32>{let q=vec2<i32>(p.xy);return vec4(textureLoad(source,q,0),textureLoad(source,q,1),textureLoad(source,q,2),textureLoad(source,q,3));}
 "#.into())});
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("final depth coverage oracle"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba32Float,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview: None,
            cache: None,
        });
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("independent final depth for each MSAA sample"),
            size: wgpu::Extent3d {
                width: SIZE,
                height: SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let buffer = |label, size| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let depth = buffer("four final depth samples", u64::from(SIZE * SIZE * 16));
        let color = buffer(
            "actual HDR color coverage bytes",
            u64::from(SIZE * SIZE * 8),
        );
        Self {
            samples,
            ground_bounds: render::height_tile::HeightPlan::new(0, [0, 0], 0, 64)
                .unwrap()
                .outer_bounds_pages
                .map(|page| page as f64 * 16.),
            layout,
            pipeline,
            texture,
            view,
            depth,
            color,
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
    fn frame(
        &self,
        gpu: &render::Gpu,
        terrain: &mut TerrainGpu,
        horizon: Option<&mut ViewHorizon>,
        camera: Camera,
        eye: [f32; 3],
    ) -> Image {
        self.frame_extra(gpu, terrain, horizon, None, camera, eye)
    }
    fn frame_extra(
        &self,
        gpu: &render::Gpu,
        terrain: &mut TerrainGpu,
        mut horizon: Option<&mut ViewHorizon>,
        mut heights: Option<&mut render::height_gpu::HeightGpu>,
        camera: Camera,
        eye: [f32; 3],
    ) -> Image {
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        if let Some(h) = horizon.as_mut() {
            h.prepare_camera_extent(&gpu.queue, &mut encoder, camera, eye, [SIZE, SIZE])
                .unwrap();
        }
        if let Some(h) = heights.as_mut() {
            h.prepare_frame(&gpu.queue, &mut encoder, camera).unwrap();
        }
        terrain.invalidate();
        terrain
            .encode_scene_with_extra(
                &gpu.queue,
                &mut encoder,
                &camera,
                CullingMode::Frustum,
                true,
                |pass| {
                    if let Some(h) = &heights {
                        h.draw(pass)
                    } else if let Some(h) = &horizon {
                        h.draw(pass)
                    } else {
                        0
                    }
                },
            )
            .unwrap();
        let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(terrain.scene_depth_msaa_view()),
            }],
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("read final depth after all near and far color"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..3, 0..1);
        }
        for (texture, buffer, bpp) in [
            (&self.texture, &self.depth, 16),
            (terrain.resolved_hdr_texture(), &self.color, 8),
        ] {
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(SIZE * bpp),
                        rows_per_image: Some(SIZE),
                    },
                },
                wgpu::Extent3d {
                    width: SIZE,
                    height: SIZE,
                    depth_or_array_layers: 1,
                },
            );
        }
        gpu.queue.submit([encoder.finish()]);
        let samples: Vec<[f32; 4]> = Self::read(gpu, &self.depth)
            .chunks_exact(16)
            .map(|c| {
                std::array::from_fn(|i| f32::from_le_bytes(c[i * 4..i * 4 + 4].try_into().unwrap()))
            })
            .collect();
        let depth = samples
            .iter()
            .map(|samples| samples.iter().sum::<f32>() / 4.)
            .collect();
        let color = Self::read(gpu, &self.color);
        let coverage = color
            .chunks_exact(8)
            .map(|c| u16::from_le_bytes(c[6..8].try_into().unwrap()))
            .collect();
        let mut image = Image {
            depth,
            coverage,
            samples,
            color,
            outside_support: 0,
        };
        // Terrain's raster guard may paint a sample just outside the finite
        // source footprint. Compare only common geometric support, determined
        // by independent f64 rays and measured hardware sample locations.
        restrict_to_fixture_support(&mut image, camera, eye, self.samples, self.ground_bounds);
        image
    }
}

#[test]
fn height_near_boundary_preserves_each_ground_sample_without_a_proxy() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let read = Readback::new(&gpu);
        let mut full = terrain(&gpu);
        let mut joined = terrain(&gpu);
        near(&gpu, &mut joined, LOW, None);
        let mut height = render::height_gpu::HeightGpu::new(
            &gpu.device,
            full.geometry(),
            render::height_gpu::HeightGpuConfig {
                max_tiles: 64,
                atlas_layers: 186,
            },
            None,
        )
        .unwrap();
        let plan = render::height_tile::HeightPlan::new(0, [0, 0], 0, 64).unwrap();
        for tile in plan.tiles {
            let mut known = [255; 37];
            known[36] = 1;
            height
                .upload(
                    &gpu.queue,
                    None,
                    &render::height_tile::HeightTile {
                        key: tile.key,
                        source: 1,
                        heights: [8; 289],
                        known,
                        atlas: [3; 256],
                        tint: [u32::MAX; 256],
                        protected: [0; 32],
                    },
                    0,
                    [0; 3],
                    true,
                    &mut UploadBudget::new(100_000),
                )
                .unwrap();
        }
        let eye = [0., 55., 100.];
        let camera = camera(eye, [0., 8., 0.], false);
        let expected = read.frame_extra(&gpu, &mut full, None, Some(&mut height), camera, eye);
        height
            .update_near_cover(
                &gpu.queue,
                Some(LOW),
                &[],
                [0; 3],
                &mut UploadBudget::new(100_000),
            )
            .unwrap();
        let actual = read.frame_extra(&gpu, &mut joined, None, Some(&mut height), camera, eye);
        let diff = mismatch(&expected, &actual);
        if diff.0 != 0 || diff.1 != 0 {
            save_difference("ground-only", &expected, &actual);
        }
        assert_eq!(diff.0, 0, "ground-only HDR near boundary: {diff:?}");
        assert_eq!(diff.1, 0, "ground-only final MSAA depth: {diff:?}");
    });
}
struct Image {
    depth: Vec<f32>,
    coverage: Vec<u16>,
    samples: Vec<[f32; 4]>,
    color: Vec<u8>,
    outside_support: u32,
}
fn fixture_support(camera: Camera, eye: [f32; 3], sample: [f64; 2], bounds: [f64; 4]) -> bool {
    let ndc = [
        2. * sample[0] / f64::from(SIZE) - 1.,
        1. - 2. * sample[1] / f64::from(SIZE),
    ];
    let a: [f64; 3] = std::array::from_fn(|axis| {
        f64::from(camera.view_projection[axis][1])
            - ndc[1] * f64::from(camera.view_projection[axis][3])
    });
    let b: [f64; 3] = std::array::from_fn(|axis| {
        f64::from(camera.view_projection[axis][0])
            - ndc[0] * f64::from(camera.view_projection[axis][3])
    });
    let mut ray = [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ];
    if (0..3)
        .map(|axis| ray[axis] * f64::from(camera.view_projection[axis][3]))
        .sum::<f64>()
        < 0.
    {
        ray = ray.map(|value| -value);
    }
    let eye = eye.map(f64::from);
    if ray[1] != 0. {
        let t = (8. - eye[1]) / ray[1];
        let x = eye[0] + t * ray[0];
        let z = eye[2] + t * ray[2];
        if t >= 0. && x >= bounds[0] && x <= bounds[2] && z >= bounds[1] && z <= bounds[3] {
            return true;
        }
    }
    // Independent closed-box geometry for raw()'s one-block floating slab.
    let mut enter = 0f64;
    let mut exit = f64::INFINITY;
    for (axis, (lo, hi)) in [(0., 16.), (71., 72.), (0., 16.)].into_iter().enumerate() {
        if ray[axis] == 0. {
            if eye[axis] < lo || eye[axis] > hi {
                return false;
            }
        } else {
            let a = (lo - eye[axis]) / ray[axis];
            let b = (hi - eye[axis]) / ray[axis];
            enter = enter.max(a.min(b));
            exit = exit.min(a.max(b));
        }
    }
    exit >= enter
}
fn restrict_to_fixture_support(
    image: &mut Image,
    camera: Camera,
    eye: [f32; 3],
    offsets: [[f64; 2]; 4],
    bounds: [f64; 4],
) {
    // The fixture contains opaque surfaces only; these exact f16 encodings are
    // the five possible MSAA resolve coverage values, not sample positions.
    const COVERAGE: [u16; 5] = [0, 0x3400, 0x3800, 0x3a00, 0x3c00];
    for (index, samples) in image.samples.iter_mut().enumerate() {
        let count = samples.iter().filter(|depth| **depth > 0.).count();
        assert_eq!(
            image.coverage[index], COVERAGE[count],
            "HDR alpha and final depth coverage disagree at pixel{index}"
        );
        for (sample, depth) in samples.iter_mut().enumerate() {
            let position = [
                (index % SIZE as usize) as f64 + offsets[sample][0],
                (index / SIZE as usize) as f64 + offsets[sample][1],
            ];
            if !fixture_support(camera, eye, position, bounds) {
                image.outside_support += u32::from(*depth != 0.);
                *depth = 0.;
            }
        }
        image.depth[index] = samples.iter().sum::<f32>() / 4.;
        image.coverage[index] = COVERAGE[samples.iter().filter(|depth| **depth > 0.).count()];
    }
}
fn mismatch(a: &Image, b: &Image) -> (usize, usize, f32) {
    let holes = a
        .coverage
        .iter()
        .zip(&b.coverage)
        .filter(|(a, b)| a != b)
        .count();
    let mut bad = 0;
    let mut max = 0f32;
    for (a, b) in a.samples.iter().zip(&b.samples) {
        let d = a
            .iter()
            .zip(b)
            .map(|(a, b)| (a - b).abs())
            .fold(0f32, f32::max);
        max = max.max(d);
        if d > 1e-6 {
            bad += 1
        }
    }
    (holes, bad, max)
}
fn save_difference(label: &str, expected: &Image, actual: &Image) {
    let directory =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.local/artifacts");
    std::fs::create_dir_all(&directory).unwrap();
    for (kind, image) in [("expected", expected), ("actual", actual)] {
        let bytes = image
            .depth
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect::<Vec<_>>();
        std::fs::write(
            directory.join(format!("near-far-{label}-{kind}.depth")),
            bytes,
        )
        .unwrap();
        std::fs::write(
            directory.join(format!("near-far-{label}-{kind}.hdr")),
            &image.color,
        )
        .unwrap();
    }
    for (i, (a, b)) in expected
        .depth
        .iter()
        .zip(&actual.depth)
        .enumerate()
        .filter(|(index, _)| {
            expected.samples[*index]
                .iter()
                .zip(actual.samples[*index])
                .any(|(a, b)| (a - b).abs() > 1e-6)
        })
        .take(20)
    {
        eprintln!(
            "joinDepth {label} x{} y{} expected{a:.10} actual{b:.10} expectedSamples{:?} actualSamples{:?}",
            i % SIZE as usize,
            i / SIZE as usize,
            expected.samples[i],
            actual.samples[i],
        );
    }
}
fn camera(eye: [f32; 3], target: [f32; 3], top: bool) -> Camera {
    if top {
        // ViewHorizon exposes the production perspective camera contract.
        // A downward perspective keeps a64-block footprint on the ground;
        // an orthographic ray requires a distinct ProxyCamera API.
        Camera::perspective(
            eye,
            target,
            [0., 0., -1.],
            2. * (32. / (eye[1] - target[1])).atan(),
            1.,
            0.1,
            300.,
        )
    } else {
        Camera::perspective(eye, target, [0., 1., 0.], 1., 1., 0.1, 300.)
    }
}
#[test]
fn actual_three_dimensional_near_height_and_proxy_join_matches_independent_full_scene() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let mut read = Readback::new(&gpu);
        println!("joined oracle measured sample positions {:?}", read.samples);
        let mut full = terrain(&gpu);
        let full_plan = render::height_tile::HeightPlan::new(0, [0, 0], 0, 64).unwrap();
        for t in &full_plan.tiles {
            assert_eq!(t.key.level, 0);
            upload(&gpu, &mut full, page(t.key.x, 0, t.key.z));
        }
        upload(&gpu, &mut full, page(0, 4, 0));
        let cameras = [
            ([0., 150., 0.], [0., 8., 0.], true),
            ([0., 55., 100.], [0., 8., 0.], false),
            ([0., 175., 140.], [0., 24., 0.], false),
        ];
        for (label, volume, old_resident) in [
            ("low", LOW, false),
            ("high", HIGH, false),
            ("high_with_old_low", HIGH, true),
            ("near32_low", WIDE_LOW, false),
            ("near32_high", WIDE_HIGH, false),
        ] {
            let mut reference_pages = full_plan
                .tiles
                .iter()
                .map(|tile| page(tile.key.x, 0, tile.key.z))
                .collect::<std::collections::BTreeSet<_>>();
            if volume[0][1] <= 0 && volume[1][1] > 0 {
                for z in volume[0][2]..volume[1][2] {
                    for x in volume[0][0]..volume[1][0] {
                        reference_pages.insert(page(x, 0, z));
                    }
                }
            }
            read.ground_bounds = [
                f64::from(reference_pages.iter().map(|page| page.x).min().unwrap()) * 16.,
                f64::from(reference_pages.iter().map(|page| page.z).min().unwrap()) * 16.,
                (f64::from(reference_pages.iter().map(|page| page.x).max().unwrap()) + 1.) * 16.,
                (f64::from(reference_pages.iter().map(|page| page.z).max().unwrap()) + 1.) * 16.,
            ];
            let mut extended_reference = if reference_pages.len() > full_plan.tiles.len() {
                let mut reference = terrain(&gpu);
                for &page in &reference_pages {
                    upload(&gpu, &mut reference, page);
                }
                upload(&gpu, &mut reference, page(0, 4, 0));
                Some(reference)
            } else {
                None
            };
            let mut joined = terrain(&gpu);
            if old_resident {
                near(&gpu, &mut joined, LOW, None)
            } else {
                near(&gpu, &mut joined, volume, None)
            }
            let extras = if old_resident {
                (-1..=1)
                    .flat_map(|z| (-1..=1).map(move |x| page(x, 0, z)))
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            let (mut h, keys) = horizon(&gpu, &joined, Some(volume), 2, &extras);
            assert_eq!(keys.len(), full_plan.tiles.len());
            assert_eq!(h.stats().proxy_front, 8);
            for (eye, target, top) in cameras {
                let c = camera(eye, target, top);
                let expected = read.frame(
                    &gpu,
                    extended_reference.as_mut().unwrap_or(&mut full),
                    None,
                    c,
                    eye,
                );
                let actual = read.frame(&gpu, &mut joined, Some(&mut h), c, eye);
                if expected.outside_support != 0 || actual.outside_support != 0 {
                    println!(
                        "{label} eye{eye:?} raster-only samples outside finite fixture support: reference{} joined{}",
                        expected.outside_support, actual.outside_support
                    );
                }
                let diff = mismatch(&expected, &actual);
                if diff.0 != 0 || diff.1 != 0 {
                    save_difference(label, &expected, &actual);
                }
                assert_eq!(
                    diff.0, 0,
                    "{label} HDR alpha coverage changed eye{eye:?}: {diff:?}"
                );
                assert_eq!(
                    diff.1, 0,
                    "{label} final MSAA depth changed eye{eye:?}: {diff:?}"
                );
            }
        }
        // Deliberate old implementation: CPU halo radius2 removes5×5, while only
        // 3×3 raw pages draw. The independent complete scene must detect that gap.
        let mut old = terrain(&gpu);
        near(&gpu, &mut old, LOW, None);
        let (mut h, _) = horizon(&gpu, &old, None, 2, &[]);
        let (eye, target, top) = cameras[0];
        let c = camera(eye, target, top);
        let expected = read.frame(&gpu, &mut full, None, c, eye);
        let old_image = read.frame(&gpu, &mut old, Some(&mut h), c, eye);
        let negative = mismatch(&expected, &old_image);
        assert!(
            negative.0 > 0 && negative.1 > 0,
            "old5×5cut/3×3draw negative control failed: {negative:?}"
        );
        // An intended raw page that is not available must remain Unknown. Its
        // missing surface is the expected image; far data cannot fill the hole.
        let missing = page(1, 0, 0);
        let mut partial_reference = terrain(&gpu);
        for t in &full_plan.tiles {
            let p = page(t.key.x, 0, t.key.z);
            if p != missing {
                upload(&gpu, &mut partial_reference, p);
            }
        }
        upload(&gpu, &mut partial_reference, page(0, 4, 0));
        let mut partial_near = terrain(&gpu);
        near(&gpu, &mut partial_near, LOW, Some(missing));
        let (mut partial_horizon, _) = horizon(&gpu, &partial_near, Some(LOW), 2, &[]);
        for (eye, target, top) in cameras {
            let c = camera(eye, target, top);
            let unknown_expected = read.frame(&gpu, &mut partial_reference, None, c, eye);
            let actual = read.frame(&gpu, &mut partial_near, Some(&mut partial_horizon), c, eye);
            let diff = mismatch(&unknown_expected, &actual);
            assert_eq!(
                diff.0, 0,
                "Unknown page received fake far coverage: {diff:?}"
            );
            assert_eq!(diff.1, 0, "Unknown page received fake far depth: {diff:?}");
        }
        // With the X/Z radii matched, the old whole-column retirement still
        // loses the floating Y4 platform outside near Y0..2, without an XY hole.
        let mut old_vertical = terrain(&gpu);
        near(&gpu, &mut old_vertical, LOW, None);
        let (mut old_vertical_horizon, _) = horizon(&gpu, &old_vertical, None, 1, &[]);
        let vertical = read.frame(
            &gpu,
            &mut old_vertical,
            Some(&mut old_vertical_horizon),
            c,
            eye,
        );
        let vertical_negative = mismatch(&expected, &vertical);
        assert_eq!(
            vertical_negative.0, 0,
            "matched X/Z control must not create a ground hole"
        );
        assert!(
            vertical_negative.1 > 0,
            "old whole-column mask retained the outside-Y platform unexpectedly"
        );
        println!(
            "Near/far independent HDR+finalMSAAdepth PASS 6volumes×3cameras; old5×5negative {negative:?}; oldvertical {vertical_negative:?}"
        );
    });
}
