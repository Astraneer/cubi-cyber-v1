#![cfg(not(target_arch = "wasm32"))]
//! Independent read-only audit of the actual buffers used by distant shadows.
use render::{
    OutputTransfer,
    lod::{MaskedRayProxy, PublicPage, PublicVoxel, SurfaceKind},
    lod_gpu::{LodProxyGpu, ProxyConfig, ProxyPayload},
    terrain_gpu::{AtlasImage, GpuConfig, TerrainGpu, UploadBudget, translation},
};

#[test]
fn actual_gpu_cutout_material_histogram_and_alpha_facts_match_loading_atlas() {
    let instance = render::instance();
    let gpu = pollster::block_on(render::Gpu::request(&instance, None, true))
        .expect("actual GPU required for shadow metadata");
    let mut atlas = AtlasImage::donor().unwrap();
    for mip in &mut atlas.mips {
        let bytes = mip.size as usize * mip.size as usize * 4;
        for (layer, alpha) in [(1, 0), (2, 255), (3, 127), (4, 128)] {
            for pixel in mip.rgba[layer * bytes..(layer + 1) * bytes].chunks_exact_mut(4) {
                pixel[3] = alpha;
            }
        }
    }
    let terrain = TerrainGpu::new(
        &gpu.device,
        &gpu.queue,
        wgpu::TextureFormat::Rgba8Unorm,
        64,
        64,
        OutputTransfer::Attachment,
        GpuConfig {
            max_pages: 4,
            max_quads: 4096,
            occlusion: false,
        },
        &atlas,
    )
    .unwrap();
    let mut proxies = LodProxyGpu::new(
        &gpu.device,
        &gpu.queue,
        terrain.geometry(),
        ProxyConfig {
            max_objects: 8,
            max_payload_words: 65536,
            atlas_layers: 186,
            instrument: false,
        },
        None,
    )
    .unwrap();
    let mut public = PublicPage::filled(PublicVoxel::Air);
    for y in 0..16 {
        for z in 4..12 {
            for x in 4..12 {
                public.set(
                    [x, y, z],
                    PublicVoxel::Solid {
                        material: 4,
                        tint: u32::MAX,
                        kind: SurfaceKind::Cutout,
                    },
                );
            }
        }
    }
    let payload =
        ProxyPayload::from_public(&MaskedRayProxy::from_protected(&public, 4), 186).unwrap();
    assert_eq!(payload.cutout_quads(), 64);
    let handle = proxies
        .upload(
            &gpu.queue,
            None,
            1,
            translation([32., 16., 16.]),
            [0.; 4],
            &payload,
            &mut UploadBudget::new(1_000_000),
        )
        .unwrap();
    assert_eq!(handle.slot, 0);
    let source = proxies.shadow_source();
    const WORDS: usize = 16 + 186;
    let output = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readonly shadow alpha GPU audit"),
        size: (WORDS * 4) as u64,
        usage: wgpu::BufferUsages::STORAGE
            | wgpu::BufferUsages::COPY_SRC
            | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    gpu.queue.write_buffer(&output, 0, &[0; WORDS * 4]);
    let shader = gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("independent public quad material metadata audit"),
        source: wgpu::ShaderSource::Wgsl(
            r#"
struct Object{model:mat4x4<f32>,inverse_model:mat4x4<f32>,bounds_min:vec4<f32>,bounds_max:vec4<f32>,data:vec4<u32>,light:vec4<f32>,reserved:array<vec4<u32>,4>};
@group(0)@binding(0)var<storage,read>materials:array<vec4<u32>>;
@group(0)@binding(1)var<storage,read>objects:array<Object>;
@group(0)@binding(2)var<storage,read>payload:array<u32>;
@group(0)@binding(3)var<storage,read_write>out:array<atomic<u32>>;
@compute @workgroup_size(64)fn main(@builtin(global_invocation_id)id:vec3<u32>){
 let object=objects[0];let count=object.reserved[0].z;
 if id.x==0u{
  atomicStore(&out[0],materials[4].y);atomicStore(&out[1],materials[4].x);
  atomicStore(&out[2],object.data.w);atomicStore(&out[3],count);
  atomicStore(&out[8],arrayLength(&materials));atomicStore(&out[9],object.reserved[0].x);
  atomicStore(&out[10],object.reserved[0].y);
 }
 if id.x>=count{return;}
 let word=object.reserved[0].y+id.x*4u;
 if word+3u>=arrayLength(&payload){atomicAdd(&out[11],1u);return;}
 let material=payload[word+1u]&65535u;
 if material>=arrayLength(&materials)||material>=186u{atomicAdd(&out[7],1u);return;}
 atomicAdd(&out[16u+material],1u);
 let value=materials[material];
 if value.x>=186u{atomicAdd(&out[12],1u);return;}
 if (value.y&2u)!=0u{atomicAdd(&out[4],1u);}
 else if(value.y&4u)!=0u{atomicAdd(&out[5],1u);}
 else{atomicAdd(&out[6],1u);}
}
"#
            .into(),
        ),
    });
    let pipeline = gpu
        .device
        .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("actual public shadow metadata, no draw or timing"),
            layout: None,
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
    let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: source.materials.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: source.objects.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: source.payload.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: output.as_entire_binding(),
            },
        ],
    });
    let readback = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("bounded shadow metadata report"),
        size: (WORDS * 4) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.dispatch_workgroups(1, 1, 1);
    }
    encoder.copy_buffer_to_buffer(&output, 0, &readback, 0, (WORDS * 4) as u64);
    gpu.queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    readback.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        tx.send(r).unwrap();
    });
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    rx.recv().unwrap().unwrap();
    let values: Vec<u32> = readback
        .slice(..)
        .get_mapped_range()
        .chunks_exact(4)
        .map(|v| u32::from_le_bytes(v.try_into().unwrap()))
        .collect();
    readback.unmap();
    let histogram: Vec<_> = values[16..]
        .iter()
        .enumerate()
        .filter(|(_, count)| **count != 0)
        .map(|(material, count)| format!("{material}:{count}"))
        .collect();
    eprintln!(
        "\n::notice title=SHADOW_ALPHA_GPU_AUDIT::material4Y={} layer={} active={} quads={} pass={} fail={} mixed={} invalid={}/{}/{} classes={} histogram={}",
        values[0],
        values[1],
        values[2],
        values[3],
        values[4],
        values[5],
        values[6],
        values[7],
        values[11],
        values[12],
        values[9],
        histogram.join(","),
    );
    assert_ne!(values[0] & 2, 0, "material4 GPU alpha128 must be pass-all");
    assert_eq!(values[0] & 4, 0);
    assert_eq!(&values[1..4], &[4, 1, 64]);
    assert_eq!(&values[4..8], &[64, 0, 0, 0]);
    assert_eq!(values[8], 186);
    assert_eq!(values[9] & 2, 2);
    assert_eq!(&values[11..13], &[0, 0]);
    assert_eq!(histogram, ["4:64"]);
}
