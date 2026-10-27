#![cfg(not(target_arch = "wasm32"))]
//! Actual source-authority fix: protect the high owner and publish its real public geometry.
//! No UI mining claim: the final state is committed through World/HeightEditIndex.
use core_types::{BlockPos, Cell, LocalIdx, PageId, SpaceId, Tick};
use gen_authority::{PrivateSeed, ServerGenerator};
use render::{
    height_gpu::{HeightGpu, HeightGpuConfig},
    height_tile::{HeightTile, HeightTileKey},
    lod::{MaskedRayProxy, PublicPage, PublicVoxel, SurfaceKind},
    lod_gpu::{LodProxyGpu, ProxyCamera, ProxyConfig, ProxyPayload},
    terrain_gpu::{
        AtlasImage, Camera, CullingMode, GpuConfig, TerrainGpu, UploadBudget, translation,
    },
    terrain_mesh::{Face, MeshScratch, PageHalo, PageMesh, mesh_page},
};
use server::{
    cold_projection::{ColumnKey, ColumnSnapshot, Limits},
    height_index::{CellEdit, HeightEditIndex, IndexLimits, TileKey},
    height_tile::{
        HeightBuildLimits, PublicHeightTile, ReplacementRequirement, build_canonical_tile,
    },
};
use std::collections::BTreeMap;
use terrain_gen::{GeneratedColumn, TerrainSeed};
use world::{Transaction, World};

const SIZE: u32 = 512;
const FOV: f32 = 1.;
const EYE: [f32; 3] = [-0.5, 70.62, -39.5];
// Settled Near32, centred on page(-1,4,-3), with no old resident extras.
const NEAR: [[i32; 3]; 2] = [[-3, 3, -5], [2, 6, 0]];
const HIGH_WIRE: &[u8] = include_bytes!("fixtures/height-wall-high.hti2");
const LOW_WIRE: &[u8] = include_bytes!("fixtures/height-wall-low.hti2");

fn page(x: i32, y: i32, z: i32) -> PageId {
    PageId {
        space: SpaceId(0),
        x,
        y,
        z,
    }
}
fn local(x: u8, y: u8, z: u8) -> LocalIdx {
    LocalIdx::new(u16::from(x) + 16 * (u16::from(z) + 16 * u16::from(y))).unwrap()
}
fn key(x: i32) -> TileKey {
    TileKey {
        space: SpaceId(0),
        level: 0,
        x,
        z: -3,
    }
}
fn pos(x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos {
        space: SpaceId(0),
        x,
        y,
        z,
    }
}
fn canonical(g: &ServerGenerator, index: &HeightEditIndex, k: TileKey) -> PublicHeightTile {
    build_canonical_tile(
        g.terrain(),
        &index.capture(k, 4096, 65536).unwrap(),
        HeightBuildLimits::default(),
    )
    .unwrap()
    .0
}
fn assert_captured(actual: &PublicHeightTile, bytes: &[u8]) {
    let wire = net::height::decode(bytes).unwrap();
    assert_eq!(actual.source, wire.source);
    assert_eq!(actual.error, wire.error);
    assert_eq!(actual.heights, wire.heights);
    assert_eq!(actual.known, wire.known);
    assert_eq!(actual.atlas, wire.atlas);
    assert_eq!(actual.tint, wire.tint);
    assert_eq!(actual.protected, wire.protected);
    assert_eq!(wire.replacement, net::height::Replacement::None);
    assert_eq!(actual.replacement, ReplacementRequirement::None);
}

struct Fixture {
    generator: ServerGenerator,
    index: HeightEditIndex,
    high: PublicHeightTile,
    public_column: Box<server::cold_projection::BuiltColumn>,
    public_high: Box<server::cold_projection::BuiltColumn>,
    columns: BTreeMap<(i32, i32), GeneratedColumn>,
    edited_cells: usize,
}
impl Fixture {
    fn new() -> Self {
        let generator = ServerGenerator::new(TerrainSeed(1), PrivateSeed::new([37; 32]));
        let mut index = HeightEditIndex::new(IndexLimits::default()).unwrap();
        let before_high = canonical(&generator, &index, key(3));
        let before_low = canonical(&generator, &index, key(2));
        assert_captured(&before_high, HIGH_WIRE);
        assert_captured(&before_low, LOW_WIRE);
        assert_eq!(before_high.heights, [71; 289]);
        assert_eq!(before_high.error.unwrap().max_error_q1024, 0);
        // A naturally authored cliff here would be a false premise.
        for z in 0..17 {
            assert_eq!(before_low.heights[15 + z * 17], 71);
            assert_eq!(before_low.heights[16 + z * 17], before_high.heights[z * 17]);
        }
        let mut world = World::new(rules::properties, 128);
        // Create actual canonical authority pages; no fabricated pre-edit cells.
        for x in -1..=2 {
            let column = generator.generate_column(x, -3).unwrap();
            let mut birth = Transaction::new(Tick(0), SpaceId(0));
            for y in 4..8 {
                let p = page(x, y, -3);
                let cells: Box<[Cell]> = column.page(p, None).unwrap().cells;
                birth.create_page(p, cells);
            }
            world.commit(birth).unwrap();
        }
        let mut edit = Transaction::new(Tick(1), SpaceId(0));
        for x in -1..48 {
            for z in -44..-36 {
                assert!((f64::from(x) - 0.5).abs() > 32. || (f64::from(z) - 0.5).abs() > 32.);
                edit.set_cell(pos(x, 68, z), Cell::from_parts(rules::ids::STONE, 0));
                for y in 69..128 {
                    edit.set_cell(pos(x, y, z), Cell::AIR);
                }
            }
        }
        let committed = world.commit(edit).unwrap();
        let edits = committed
            .cells
            .iter()
            .map(|change| {
                let (x, y, z) = change.index.xyz();
                CellEdit {
                    position: pos(
                        change.page.x * 16 + i32::from(x),
                        change.page.y * 16 + i32::from(y),
                        change.page.z * 16 + i32::from(z),
                    ),
                    cell: change.new,
                }
            })
            .collect::<Vec<_>>();
        assert!(!edits.is_empty());
        assert!(
            edits
                .iter()
                .all(|e| e.position.x <= 47 && (-44..-36).contains(&e.position.z))
        );
        // Actual committed values, not an aspirational write list or fake stamps.
        index.apply_committed(committed.tx, &edits).unwrap();
        let encoded = index.encode_snapshot().unwrap();
        let index = HeightEditIndex::decode_snapshot(&encoded, IndexLimits::default()).unwrap();
        assert_eq!(encoded, index.encode_snapshot().unwrap());
        let high = canonical(&generator, &index, key(3));
        assert!(
            high.source > before_high.source,
            "foreign rim edit did not invalidate the high owner"
        );
        assert_eq!(high.heights, before_high.heights);
        assert_eq!(high.error, before_high.error);
        assert_eq!(high.known, before_high.known);
        assert_eq!(high.atlas, before_high.atlas);
        assert_eq!(high.tint, before_high.tint);
        assert_eq!(high.protected, [255; 32]);
        assert_eq!(
            high.replacement,
            ReplacementRequirement::Column {
                source: high.source,
                min_page_y: 0,
                max_page_y: 7
            }
        );
        let halo = index.capture(key(3), 4096, 65536).unwrap();
        assert!(!halo.columns.is_empty());
        assert!(
            halo.columns
                .iter()
                .all(|c| c.x == 47 && (-44..-36).contains(&c.z))
        );
        let low = canonical(&generator, &index, key(2));
        assert_eq!(low.protected, [255; 32]);
        assert_eq!(
            low.replacement,
            ReplacementRequirement::Column {
                source: low.source,
                min_page_y: 0,
                max_page_y: 7
            }
        );
        for z in 0..17 {
            assert_eq!(
                low.heights[16 + z * 17],
                high.heights[z * 17],
                "actual baseline seam does not fail after excavation"
            );
        }
        // Exercise the real public exterior/secret policy. No synthetic all-Air DTO.
        let empty = World::new(rules::properties, 16);
        let public_column =
            ColumnSnapshot::capture(&empty, &index, ColumnKey { x: 2, z: -3 }, Limits::default())
                .unwrap()
                .execute(&generator, || false)
                .unwrap();
        assert!(public_column.certificate.matches(&empty, &index));
        let public_high =
            ColumnSnapshot::capture(&empty, &index, ColumnKey { x: 3, z: -3 }, Limits::default())
                .unwrap()
                .execute(&generator, || false)
                .unwrap();
        assert!(public_high.certificate.matches(&empty, &index));
        assert_eq!(public_high.pages.len(), 8);
        assert!(
            rules::properties(
                public_high.pages[4].sealed_cells.as_ref().unwrap()
                    [usize::from(local(0, 6, 8).get())]
            )
            .opaque
        );

        assert_eq!(public_column.pages.len(), 8);
        for y in 69..128 {
            let p = &public_column.pages[(y / 16) as usize];
            let i = local(15, (y % 16) as u8, 8);
            assert_eq!(
                p.sealed_cells.as_ref().unwrap()[usize::from(i.get())],
                Cell::AIR,
                "exterior corridor was sealed in actual public projection"
            );
        }
        assert!(
            rules::properties(
                generator
                    .generate_page(page(3, 4, -3), None)
                    .unwrap()
                    .page
                    .cells[local(0, 6, 8).get() as usize]
            )
            .opaque
        );
        Self {
            generator,
            index,
            high,
            public_column,
            public_high,
            columns: BTreeMap::new(),
            edited_cells: edits.len(),
        }
    }
    fn cell(&mut self, x: i32, y: i32, z: i32) -> Cell {
        if let Some(edit) = self.index.get(pos(x, y, z)) {
            return edit.cell;
        }
        if !(0..128).contains(&y) {
            return Cell::AIR;
        }
        let k = (x.div_euclid(16), z.div_euclid(16));
        let generator = &self.generator;
        let column = self
            .columns
            .entry(k)
            .or_insert_with(|| generator.generate_column(k.0, k.1).unwrap());
        Cell::from_parts(
            column.block_padded(x.rem_euclid(16) + 1, y, z.rem_euclid(16) + 1),
            0,
        )
    }
    fn mesh(&mut self, p: PageId) -> PageMesh {
        let mut halo = PageHalo::filled(0);
        for y in -1..=16 {
            for z in -1..=16 {
                for x in -1..=16 {
                    halo.set(
                        x,
                        y,
                        z,
                        self.cell(
                            p.x * 16 + i32::from(x),
                            p.y * 16 + i32::from(y),
                            p.z * 16 + i32::from(z),
                        )
                        .raw(),
                    );
                }
            }
        }
        let mut output = PageMesh::default();
        mesh_page(
            &halo,
            |raw| client::material(raw, u16::MAX),
            &mut MeshScratch::new(),
            &mut output,
        );
        output
    }
    fn payloads(&self, high: bool) -> Vec<(PageId, ProxyPayload)> {
        let column = if high {
            &self.public_high
        } else {
            &self.public_column
        };
        column
            .pages
            .iter()
            .map(|p| {
                let geometry = p.geometry.as_ref().unwrap();
                assert_eq!(geometry.known, [255; 512]);
                let public = PublicPage::from_exposed(|[x, y, z]| {
                    let i = local(x, y, z);
                    let at = usize::from(i.get());
                    if geometry.occupied[at / 8] & (1 << (at % 8)) == 0 {
                        return PublicVoxel::Air;
                    }
                    let m = geometry.palette[usize::from(geometry.palette_index(i))];
                    PublicVoxel::Solid {
                        material: m.atlas_layer,
                        tint: m.tint,
                        kind: match m.kind {
                            server::lod::PublicSurfaceKind::Opaque => SurfaceKind::Opaque,
                            server::lod::PublicSurfaceKind::Cutout => SurfaceKind::Cutout,
                            server::lod::PublicSurfaceKind::Water => SurfaceKind::Water,
                        },
                    }
                });
                let packed = ProxyPayload::from_public(
                    &MaskedRayProxy::from_public_page(
                        &public,
                        render::lod::protection_groups(&public, 4),
                    ),
                    186,
                )
                .unwrap();
                if p.page.x == 2 && p.page.y == 4 {
                    assert_eq!(packed.voxel([15, 6, 8]), PublicVoxel::Air);
                }
                (p.page, packed)
            })
            .collect()
    }
}

#[test]
fn actual_authority_post_edit_protects_high_owner_and_publishes_both_exact_columns() {
    let fixture = Fixture::new();
    assert!(fixture.high.source > 1);
    assert!(fixture.edited_cells <= 23520);
    assert_eq!(fixture.payloads(false).len(), 8);
    assert_eq!(fixture.payloads(true).len(), 8);
}

fn terrain(gpu: &render::Gpu, samples: u32) -> TerrainGpu {
    let mut t = TerrainGpu::new_with_samples(
        &gpu.device,
        &gpu.queue,
        wgpu::TextureFormat::Rgba8Unorm,
        SIZE,
        SIZE,
        render::OutputTransfer::Attachment,
        GpuConfig {
            max_pages: 96,
            max_quads: 131072,
            occlusion: false,
        },
        &AtlasImage::donor().unwrap(),
        samples,
    )
    .unwrap();
    t.prepare_scene_sampling(&gpu.device).unwrap();
    t
}
fn upload(gpu: &render::Gpu, t: &mut TerrainGpu, p: PageId, mesh: &PageMesh) {
    t.upload_page(
        &gpu.queue,
        None,
        1,
        translation([p.x as f32 * 16., p.y as f32 * 16., p.z as f32 * 16.]),
        mesh,
        &mut UploadBudget::new(4_000_000),
    )
    .unwrap();
}
struct Image {
    depth: Vec<[f32; 4]>,
    color: Vec<u8>,
}
fn read(gpu: &render::Gpu, b: &wgpu::Buffer) -> Vec<u8> {
    let (tx, rx) = std::sync::mpsc::channel();
    b.slice(..)
        .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    rx.recv().unwrap().unwrap();
    let bytes = b.slice(..).get_mapped_range().to_vec();
    b.unmap();
    bytes
}
fn frame(
    gpu: &render::Gpu,
    terrain: &mut TerrainGpu,
    height: Option<&mut HeightGpu>,
    proxy: Option<&mut LodProxyGpu>,
    camera: Camera,
    samples: u32,
) -> Image {
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    let mut height = height;
    let mut proxy = proxy;
    if let Some(h) = height.as_mut() {
        h.prepare_frame(&gpu.queue, &mut encoder, camera).unwrap();
    }
    if let Some(p) = proxy.as_mut() {
        p.prepare_frame(
            &gpu.queue,
            &mut encoder,
            ProxyCamera {
                camera,
                eye: EYE,
                viewport_height: SIZE,
                orthographic_direction: None,
            },
        )
        .unwrap();
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
                height.as_ref().map_or(0, |h| h.draw(pass))
                    + proxy.as_ref().map_or(0, |p| p.draw(pass))
            },
        )
        .unwrap();
    let depth_texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("independent actual final-depth capture"),
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
    let depth_view = depth_texture.create_view(&Default::default());
    let sample_type = if samples == 1 {
        "texture_depth_2d"
    } else {
        "texture_depth_multisampled_2d"
    };
    let loads = if samples == 1 {
        "let d=textureLoad(source,q,0);return vec4(d);"
    } else {
        "return vec4(textureLoad(source,q,0),textureLoad(source,q,1),textureLoad(source,q,2),textureLoad(source,q,3));"
    };
    let shader=gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor{label:Some("depth-only readback without altering scene"),source:wgpu::ShaderSource::Wgsl(format!("@group(0)@binding(0)var source:{sample_type}; @vertex fn vs(@builtin(vertex_index)i:u32)->@builtin(position)vec4f{{let p=array<vec2f,3>(vec2f(-1.,-1.),vec2f(3.,-1.),vec2f(-1.,3.));return vec4f(p[i],0.,1.);}} @fragment fn fs(@builtin(position)p:vec4f)->@location(0)vec4f{{let q=vec2i(p.xy);{loads}}}").into())});
    let pipeline = gpu
        .device
        .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("read retained depth"),
            layout: None,
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
    let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::TextureView(terrain.scene_depth_msaa_view()),
        }],
    });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("observe four original depth samples"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &depth_view,
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
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
    }
    let buffer = |size| {
        gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("diagnostic image readback"),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        })
    };
    let depths = buffer(u64::from(SIZE * SIZE * 16));
    let colors = buffer(u64::from(SIZE * SIZE * 8));
    for (texture, buffer, bpp) in [
        (&depth_texture, &depths, 16),
        (terrain.resolved_hdr_texture(), &colors, 8),
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
    let depth = read(gpu, &depths)
        .chunks_exact(16)
        .map(|c| {
            std::array::from_fn(|i| f32::from_le_bytes(c[4 * i..4 * i + 4].try_into().unwrap()))
        })
        .collect();
    Image {
        depth,
        color: read(gpu, &colors),
    }
}
// Independent analytic pinhole, +X forward, +Z right. Mask strictly inside one
// one-block-high coplanar strip of genuine voxel faces. Samples stay inside
// the strip outer boundary; internal coplanar voxel edges are retained.
fn witness(x: u32, y: u32) -> Option<[f64; 3]> {
    let tangent = (f64::from(FOV) * 0.5).tan();
    let ndc = [
        2. * (f64::from(x) + 0.5) / f64::from(SIZE) - 1.,
        1. - 2. * (f64::from(y) + 0.5) / f64::from(SIZE),
    ];
    let distance = 48. - f64::from(EYE[0]);
    let p = [
        48.,
        f64::from(EYE[1]) + distance * ndc[1] * tangent,
        f64::from(EYE[2]) + distance * ndc[0] * tangent,
    ];
    ((70.2..70.8).contains(&p[1]) && (-42.5..-37.5).contains(&p[2])).then_some(p)
}
fn projected_depth(camera: Camera, p: [f64; 3]) -> f64 {
    let clip: [f64; 4] = std::array::from_fn(|row| {
        (0..3)
            .map(|i| f64::from(camera.view_projection[i][row]) * p[i])
            .sum::<f64>()
            + f64::from(camera.view_projection[3][row])
    });
    clip[2] / clip[3]
}
fn save(name: &str, image: &Image, mask: &[u8], samples: u32) {
    if let Some(path) = std::env::var_os("HEIGHT_WALL_ARTIFACTS") {
        let path = std::path::PathBuf::from(path);
        std::fs::create_dir_all(&path).unwrap();
        let depth = image
            .depth
            .iter()
            .flat_map(|v| v.iter().flat_map(|x| x.to_le_bytes()))
            .collect::<Vec<_>>();
        std::fs::write(path.join(format!("{name}-aa{samples}.depth4.f32")), depth).unwrap();
        std::fs::write(
            path.join(format!("{name}-aa{samples}.rgba16f")),
            &image.color,
        )
        .unwrap();
        std::fs::write(path.join(format!("witness-aa{samples}.mask8")), mask).unwrap();
    }
}

fn height_owner(gpu: &render::Gpu, t: &TerrainGpu, tile: &HeightTile) -> HeightGpu {
    let mut owner = HeightGpu::new(
        &gpu.device,
        t.geometry(),
        HeightGpuConfig {
            max_tiles: 1,
            atlas_layers: 186,
        },
        None,
    )
    .unwrap();
    owner
        .upload(
            &gpu.queue,
            None,
            tile,
            0,
            [0; 3],
            true,
            &mut UploadBudget::new(1_000_000),
        )
        .unwrap();
    owner
}
fn proxy_owner(
    gpu: &render::Gpu,
    t: &TerrainGpu,
    payloads: &[(PageId, ProxyPayload)],
) -> LodProxyGpu {
    let mut owner = LodProxyGpu::new(
        &gpu.device,
        &gpu.queue,
        t.geometry(),
        ProxyConfig {
            max_objects: payloads.len() as u32,
            max_payload_words: 262144,
            atlas_layers: 186,
            instrument: true,
        },
        None,
    )
    .unwrap();
    for (p, payload) in payloads {
        owner
            .upload(
                &gpu.queue,
                None,
                1,
                translation([p.x as f32 * 16., p.y as f32 * 16., p.z as f32 * 16.]),
                [1.; 4],
                payload,
                &mut UploadBudget::new(4_000_000),
            )
            .unwrap();
    }
    owner
}
#[test]
fn authority_boundary_halo_closes_the_actual_wall_with_old_source_negative() {
    let mut fixture = Fixture::new();
    let low_payloads = fixture.payloads(false);
    let mut all_payloads = low_payloads.clone();
    all_payloads.extend(fixture.payloads(true));
    let high_mesh = fixture.mesh(page(3, 4, -3));
    assert!(high_mesh.opaque.iter().any(|q| {
        let o = q.origin();
        let s = q.size();
        q.face() == Face::West
            && o[0] == 0
            && o[1] <= 6
            && o[1] + s[1] >= 7
            && o[2] <= 8
            && o[2] + s[0] >= 9
    }));
    assert_eq!(NEAR[1][0] * 16, 32);
    let mut meshes = BTreeMap::new();
    for z in NEAR[0][2]..NEAR[1][2] {
        for y in NEAR[0][1]..NEAR[1][1] {
            for x in NEAR[0][0]..NEAR[1][0] {
                let p = page(x, y, z);
                meshes.insert(p, fixture.mesh(p));
            }
        }
    }
    for x in 2..=3 {
        for y in 3..6 {
            let p = page(x, y, -3);
            meshes.insert(p, fixture.mesh(p));
        }
    }
    assert_eq!(meshes.len(), 81);
    let old = net::height::decode(HIGH_WIRE).unwrap();
    let old_tile = HeightTile {
        key: HeightTileKey {
            space: 0,
            level: 0,
            x: 3,
            z: -3,
        },
        source: old.source,
        heights: old.heights,
        known: old.known,
        atlas: old.atlas,
        tint: old.tint,
        protected: old.protected,
    };
    let tile = HeightTile {
        key: old_tile.key,
        source: fixture.high.source,
        heights: fixture.high.heights,
        known: fixture.high.known,
        atlas: fixture.high.atlas,
        tint: fixture.high.tint,
        protected: fixture.high.protected,
    };
    assert_eq!(
        tile.drawable_cells(0),
        0,
        "protected Height must not also draw an invented or duplicate wall"
    );
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        for samples in [1, 4] {
            let mut full = terrain(&gpu, samples);
            let mut joined = terrain(&gpu, samples);
            let mut old_source = terrain(&gpu, samples);
            let mut repaired = terrain(&gpu, samples);
            let mut empty = terrain(&gpu, samples);
            for (p, mesh) in &meshes {
                upload(&gpu, &mut full, *p, mesh);
                if p.x < 2 {
                    upload(&gpu, &mut joined, *p, mesh);
                    upload(&gpu, &mut old_source, *p, mesh);
                    upload(&gpu, &mut repaired, *p, mesh);
                } else if p.x == 3 {
                    upload(&gpu, &mut repaired, *p, mesh);
                }
            }
            let mut height = height_owner(&gpu, &joined, &tile);
            let mut legacy_height = height_owner(&gpu, &old_source, &old_tile);
            let mut proxy = proxy_owner(&gpu, &joined, &all_payloads);
            let mut legacy_proxy = proxy_owner(&gpu, &old_source, &low_payloads);
            let camera =
                Camera::perspective(EYE, [48., EYE[1], EYE[2]], [0., 1., 0.], FOV, 1., 0.1, 96.);
            let expected = frame(&gpu, &mut full, None, None, camera, samples);
            let actual = frame(
                &gpu,
                &mut joined,
                Some(&mut height),
                Some(&mut proxy),
                camera,
                samples,
            );
            let old_negative = frame(
                &gpu,
                &mut old_source,
                Some(&mut legacy_height),
                Some(&mut legacy_proxy),
                camera,
                samples,
            );
            let restored = frame(
                &gpu,
                &mut repaired,
                Some(&mut legacy_height),
                Some(&mut legacy_proxy),
                camera,
                samples,
            );
            let empty_negative = frame(&gpu, &mut empty, None, None, camera, samples);
            let mask = (0..SIZE * SIZE)
                .map(|i| u8::from(witness(i % SIZE, i / SIZE).is_some()))
                .collect::<Vec<_>>();
            for (name, image) in [
                ("full-voxel-reference", &expected),
                ("authority-fixed", &actual),
                ("old-source-negative", &old_negative),
                ("restored-owner-control", &restored),
                ("empty-geometry-negative", &empty_negative),
            ] {
                save(name, image, &mask, samples);
            }
            let mut count = 0;
            let mut max_depth_error = 0_f64;
            for (i, &selected) in mask.iter().enumerate() {
                if selected == 0 {
                    continue;
                }
                count += 1;
                let wanted =
                    projected_depth(camera, witness(i as u32 % SIZE, i as u32 / SIZE).unwrap());
                for sample in 0..samples as usize {
                    assert!(
                        (f64::from(expected.depth[i][sample]) - wanted).abs() <= 1e-6,
                        "full voxel reference is not analytic wall"
                    );
                    let error =
                        f64::from((actual.depth[i][sample] - expected.depth[i][sample]).abs());
                    max_depth_error = max_depth_error.max(error);
                    assert!(
                        error <= 1e-6,
                        "actual public high owner failed wall pixel{i} sample{sample}: {error}"
                    );
                    assert!((restored.depth[i][sample] - expected.depth[i][sample]).abs() <= 1e-6);
                    assert_eq!(
                        old_negative.depth[i][sample], 0.,
                        "old source control stopped detecting the original owner-face gap"
                    );
                    assert_eq!(
                        empty_negative.depth[i][sample], 0.,
                        "empty geometry negative failed"
                    );
                }
                assert_eq!(
                    &restored.color[8 * i..8 * i + 8],
                    &expected.color[8 * i..8 * i + 8],
                    "restored raw owner HDR control must remain BITWISE"
                );
                assert_eq!(
                    &actual.color[8 * i + 6..8 * i + 8],
                    &expected.color[8 * i + 6..8 * i + 8],
                    "actual public wall must retain opaque HDR coverage"
                );
                assert_ne!(
                    &actual.color[8 * i..8 * i + 8],
                    &empty_negative.color[8 * i..8 * i + 8],
                    "actual wall must not be a sky/empty positive"
                );
                assert_ne!(
                    &old_negative.color[8 * i..8 * i + 8],
                    &expected.color[8 * i..8 * i + 8],
                    "old source color negative did not expose the missing wall"
                );
            }
            assert!(count >= 128);
            println!(
                "\nHEIGHT_BOUNDARY_AUTHORITY::notice::AA{samples} actual committed rows={}+snapshot reload; high E0 nowColumn0..7 with unchanged baseline; actual low+high16public pages; Near75/full81; {count} independent wall pixels/allSamples depthMax={max_depth_error}, opaqueHDRcurrent; old-source+empty negatives, restored raw-owner BITWISE; no invented walls/no caps or UI/WAL/performance claim",
                fixture.edited_cells
            );
        }
    });
}
