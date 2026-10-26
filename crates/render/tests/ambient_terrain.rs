//! Optional MRT preserves original terrain/PBR output and excludes material emission.
#![cfg(not(target_arch = "wasm32"))]
use render::{
    OutputTransfer,
    material_pack::{LayerSource, Limits, MaterialPackGpu, PreparedMaterials},
    terrain_gpu::{AtlasImage, AtlasMip, Camera, CullingMode, GpuConfig, TerrainGpu, UploadBudget},
    terrain_mesh::{CellMaterial, MeshScratch, PageHalo, PageMesh},
};
fn png(pixel: [u8; 4]) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut e = png::Encoder::new(&mut bytes, 1, 1);
        e.set_color(png::ColorType::Rgba);
        e.set_depth(png::BitDepth::Eight);
        e.write_header().unwrap().write_image_data(&pixel).unwrap();
    }
    bytes
}
fn renderer(gpu: &render::Gpu, atlas: &AtlasImage, mrt: bool) -> TerrainGpu {
    let mut renderer = TerrainGpu::new(
        &gpu.device,
        &gpu.queue,
        wgpu::TextureFormat::Rgba8Unorm,
        64,
        64,
        OutputTransfer::Attachment,
        GpuConfig {
            max_pages: 1,
            max_quads: 32,
            occlusion: false,
        },
        atlas,
    )
    .unwrap();
    if mrt {
        renderer.prepare_ambient_mrt(&gpu.device).unwrap();
    }
    let mut mesh = PageMesh::default();
    render::terrain_mesh::mesh_page(
        &PageHalo::from_page(&[1; 4096], 0),
        |cell| {
            if cell == 0 {
                CellMaterial::EMPTY
            } else {
                CellMaterial::opaque(0)
            }
        },
        &mut MeshScratch::new(),
        &mut mesh,
    );
    renderer
        .upload_page(
            &gpu.queue,
            None,
            1,
            render::terrain_gpu::translation([0.; 3]),
            &mesh,
            &mut UploadBudget::new(10000),
        )
        .unwrap();
    renderer
}
fn capture(gpu: &render::Gpu, renderer: &mut TerrainGpu) -> (Vec<u8>, Vec<f32>) {
    let output = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("terrain ambient MRT first-output oracle"),
        size: wgpu::Extent3d {
            width: 64,
            height: 64,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let read = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 64 * 64 * 4,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    renderer
        .encode_frame(
            &gpu.queue,
            &mut encoder,
            &output.create_view(&Default::default()),
            &Camera::perspective([27., 21., 30.], [8.; 3], [0., 1., 0.], 1., 1., 0.1, 200.),
            CullingMode::None,
        )
        .unwrap();
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &output,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &read,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(256),
                rows_per_image: Some(64),
            },
        },
        wgpu::Extent3d {
            width: 64,
            height: 64,
            depth_or_array_layers: 1,
        },
    );
    let delta=renderer.ambient_msaa_view().map(|view|{
  let shader=gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor{label:Some("independent per-sample signed MRT readback"),source:wgpu::ShaderSource::Wgsl("@group(0)@binding(0)var ambient:texture_multisampled_2d<f32>;@group(0)@binding(1)var<storage,read_write>result:array<vec4<f32>>;@compute @workgroup_size(8,8)fn main(@builtin(global_invocation_id)id:vec3<u32>){var value=vec4(0.0);for(var s=0;s<4;s++){value+=textureLoad(ambient,vec2<i32>(id.xy),s)*0.25;}result[id.x+64u*id.y]=value;}".into())});
  let pipeline=gpu.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor{label:None,layout:None,module:&shader,entry_point:Some("main"),compilation_options:Default::default(),cache:None});
  let storage=gpu.device.create_buffer(&wgpu::BufferDescriptor{label:None,size:64*64*16,usage:wgpu::BufferUsages::STORAGE|wgpu::BufferUsages::COPY_SRC,mapped_at_creation:false});
  let back=gpu.device.create_buffer(&wgpu::BufferDescriptor{label:None,size:64*64*16,usage:wgpu::BufferUsages::MAP_READ|wgpu::BufferUsages::COPY_DST,mapped_at_creation:false});
  let bind=gpu.device.create_bind_group(&wgpu::BindGroupDescriptor{label:None,layout:&pipeline.get_bind_group_layout(0),entries:&[wgpu::BindGroupEntry{binding:0,resource:wgpu::BindingResource::TextureView(view)},wgpu::BindGroupEntry{binding:1,resource:storage.as_entire_binding()}]});
  {let mut pass=encoder.begin_compute_pass(&Default::default());pass.set_pipeline(&pipeline);pass.set_bind_group(0,&bind,&[]);pass.dispatch_workgroups(8,8,1);}
  encoder.copy_buffer_to_buffer(&storage,0,&back,0,64*64*16);back
 });
    gpu.queue.submit([encoder.finish()]);
    let bytes = map(gpu, &read);
    let values = delta
        .map(|buffer| {
            map(gpu, &buffer)
                .chunks_exact(4)
                .map(|v| f32::from_le_bytes(v.try_into().unwrap()))
                .collect()
        })
        .unwrap_or_default();
    (bytes, values)
}
fn map(gpu: &render::Gpu, buffer: &wgpu::Buffer) -> Vec<u8> {
    let (tx, rx) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |v| tx.send(v).unwrap());
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    rx.recv().unwrap().unwrap();
    let data = buffer.slice(..).get_mapped_range().to_vec();
    buffer.unmap();
    data
}
#[test]
fn terrain_and_local_material_mrt_preserve_original_color_and_keep_conductor_emission_outside_delta()
 {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true)).unwrap();
    let atlas = AtlasImage {
        tile_size: 1,
        layers: 1,
        mips: vec![AtlasMip {
            size: 1,
            rgba: vec![255, 0, 0, 255],
            tint_mask: vec![0],
        }],
    };
    for mode in 0..4 {
        let mut plain = renderer(&gpu, &atlas, false);
        let mut mrt = renderer(&gpu, &atlas, true);
        if mode > 0 {
            let normal = png([128, 128, 255, 255]);
            let spec = png([64, if mode == 3 { 230 } else { 10 }, 0, 254]);
            let prepared = PreparedMaterials::prepare(
                &atlas,
                &[LayerSource {
                    layer: 0,
                    albedo_png: None,
                    normal_png: Some(&normal),
                    specular_png: Some(&spec),
                }],
                1,
                Limits::default(),
            )
            .unwrap();
            for renderer in [&mut plain, &mut mrt] {
                renderer
                    .prepare_material_pack(
                        &gpu.device,
                        &gpu.queue,
                        MaterialPackGpu::upload(&gpu.device, &gpu.queue, &prepared).unwrap(),
                        mode > 1,
                    )
                    .unwrap();
                renderer
                    .set_material_view(&gpu.queue, [27., 21., 30.], 0)
                    .unwrap();
            }
        }
        assert_eq!(plain.ambient_target_bytes(), 0);
        assert_eq!(mrt.ambient_target_bytes(), 64 * 64 * 4 * 8);
        let baseline = capture(&gpu, &mut plain).0;
        let (color, delta) = capture(&gpu, &mut mrt);
        assert_eq!(
            color, baseline,
            "MRT first output must remain bitwise for material mode{mode}"
        );
        let positive = color
            .chunks_exact(4)
            .filter(|p| p[0] > p[1].saturating_add(32))
            .count();
        assert!(positive > 100);
        let lit = delta.chunks_exact(4).filter(|p| p[0] > 0.01).count();
        assert!(
            delta
                .chunks_exact(4)
                .all(|p| p.iter().all(|v| v.is_finite()) && p[1] == 0. && p[2] == 0.),
            "sky and emission cannot enter the signed ambient delta"
        );
        if mode == 3 {
            assert_eq!(
                lit, 0,
                "conductor has no diffuse ambient, despite positive emission"
            );
        } else {
            assert!(lit > 100);
        }
        eprintln!(
            "AMBIENT_TERRAIN_MRT_OK mode={mode} first_output_BITWISE=true colored_pixels={positive} ambient_pixels={lit} conductor_emission_excluded={}",
            mode == 3
        );
    }
}
