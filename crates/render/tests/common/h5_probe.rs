//! Actual per-layer/combined GPU signals. Modes1/2/3 are deliberately wrong
//! image signals used to prove that alternating noise and one hard jump fail.
use super::*;
pub(super) struct LayerProbe {
    pipeline: wgpu::RenderPipeline,
    bind: wgpu::BindGroup,
    mode: wgpu::Buffer,
    texture: wgpu::Texture,
    read: wgpu::Buffer,
}
impl LayerProbe {
    pub(super) fn new(gpu: &render::Gpu, shadow: &ShadowGpu) -> Self {
        let source = format!(
            "{}\n{}",
            include_str!("../../src/shadow_sampling.wgsl"),
            r#"
@group(0)@binding(8)var<uniform>mode:vec4<u32>;
@vertex fn vertex(@builtin(vertex_index)i:u32)->@builtin(position)vec4<f32>{let p=array<vec2<f32>,3>(vec2(-1.,-1.),vec2(3.,-1.),vec2(-1.,3.));return vec4(p[i],0.,1.);}
@fragment fn fragment(@builtin(position)p:vec4<f32>)->@location(0)vec4<f32>{
 let world=vec3(-4.+p.x*.1875,1.01,-4.+p.y*.1875);
 let direction=normalize(shadow_sampling.sun.xyz);
 let bias=shadow_sampling.sun.w*(1.+2.*(1.-max(direction.y,0.)));
 let a=shadow_layer(0u,world,bias,vec3(0.,1.,0.));let b=shadow_layer(1u,world,bias,vec3(0.,1.,0.));let c=shadow_layer(2u,world,bias,vec3(0.,1.,0.));
 var combined=cascade_shadow(world,vec3(0.,1.,0.));
 if mode.x==1u{combined=1.-combined;}else if mode.x==2u{combined=a.x;}else if mode.x==3u{combined=b.x;}else if mode.x==4u{combined=select(combined+.0001,combined-.0001,combined>=.5);}
 return vec4(select(-1.,a.x,a.y>0.),select(-1.,b.x,b.y>0.),select(-1.,c.x,c.y>0.),combined);
}
"#
        );
        let shader = gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("H5 independent layer signals"),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            });
        let mode = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("H5 negative signal mode"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let layout = gpu
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("H5 layer signals"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 8,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 9,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 10,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Depth,
                            view_dimension: wgpu::TextureViewDimension::D2Array,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 11,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                        count: None,
                    },
                ],
            });
        let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("H5 layer sources"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 8,
                    resource: mode.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 9,
                    resource: shadow.sampling_buffer().as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 10,
                    resource: wgpu::BindingResource::TextureView(shadow.depth_view()),
                },
                wgpu::BindGroupEntry {
                    binding: 11,
                    resource: wgpu::BindingResource::Sampler(shadow.sampler()),
                },
            ],
        });
        let layout = gpu
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("H5 signal pipeline layout"),
                bind_group_layouts: &[&layout],
                push_constant_ranges: &[],
            });
        let pipeline = gpu
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("actual float shadow signals"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vertex"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fragment"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba32Float,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview: None,
                cache: None,
            });
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("H5 float visibility channels"),
            size: wgpu::Extent3d {
                width: 128,
                height: 128,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let read = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("H5 all completed-frame signals"),
            size: 128 * 128 * 16,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        Self {
            pipeline,
            bind,
            mode,
            texture,
            read,
        }
    }
    pub(super) fn capture(&self, gpu: &render::Gpu, mode: u32) -> Vec<[f32; 4]> {
        gpu.queue
            .write_buffer(&self.mode, 0, bytemuck::bytes_of(&[mode, 0, 0, 0]));
        let view = self.texture.create_view(&Default::default());
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("all shadow visibility signals"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind, &[]);
            pass.draw(0..3, 0..1);
        }
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &self.read,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(128 * 16),
                    rows_per_image: Some(128),
                },
            },
            wgpu::Extent3d {
                width: 128,
                height: 128,
                depth_or_array_layers: 1,
            },
        );
        gpu.queue.submit([encoder.finish()]);
        read_buffer(gpu, &self.read)
            .chunks_exact(16)
            .map(|p| {
                std::array::from_fn(|i| f32::from_le_bytes(p[i * 4..i * 4 + 4].try_into().unwrap()))
            })
            .collect()
    }
}
pub(super) fn footprint_edge(
    sampling: &render::shadows::ShadowSampling,
    layer: usize,
    pixel: usize,
) -> f64 {
    let world = [
        -4. + (pixel % 128) as f64 * 0.1875 + 0.09375,
        1.01,
        -4. + (pixel / 128) as f64 * 0.1875 + 0.09375,
    ];
    let m = &sampling.desired_matrices[layer];
    let p = std::array::from_fn::<_, 3, _>(|r| {
        f64::from(m[0][r]) * world[0]
            + f64::from(m[1][r]) * world[1]
            + f64::from(m[2][r]) * world[2]
            + f64::from(m[3][r])
    });
    if (0.0..=1.0).contains(&p[2]) {
        p[0].abs().max(p[1].abs())
    } else {
        f64::INFINITY
    }
}
fn committed_valid(sampling: &render::shadows::ShadowSampling, layer: usize, pixel: usize) -> bool {
    if sampling.params[0] & (1 << layer) == 0 {
        return false;
    }
    let world = [
        -4. + (pixel % 128) as f64 * 0.1875 + 0.09375,
        1.01,
        -4. + (pixel / 128) as f64 * 0.1875 + 0.09375,
    ];
    let m = &sampling.matrices[layer];
    let p = std::array::from_fn::<_, 3, _>(|r| {
        f64::from(m[0][r]) * world[0]
            + f64::from(m[1][r]) * world[1]
            + f64::from(m[2][r]) * world[2]
            + f64::from(m[3][r])
    });
    let guard = 1. - 2. / f64::from(sampling.params[1]);
    p[0].abs() <= guard && p[1].abs() <= guard && (0.0..=1.0).contains(&p[2])
}
/// CPU definition of the requested smooth LOD signal; it uses public footprint
/// matrices and initially observed per-layer visibility, not the combined shader.
pub(super) fn expected(sampling: &render::shadows::ShadowSampling, base: &[[f32; 4]]) -> Vec<f64> {
    base.iter()
        .enumerate()
        .map(|(i, layers)| {
            let world = [
                -4. + (i % 128) as f64 * 0.1875 + 0.09375,
                1.01,
                -4. + (i / 128) as f64 * 0.1875 + 0.09375,
            ];
            for layer in 0..3 {
                if sampling.params[0] & (1 << layer) == 0 {
                    continue;
                }
                let m = &sampling.desired_matrices[layer];
                let p = std::array::from_fn::<_, 3, _>(|r| {
                    f64::from(m[0][r]) * world[0]
                        + f64::from(m[1][r]) * world[1]
                        + f64::from(m[2][r]) * world[2]
                        + f64::from(m[3][r])
                });
                let edge = p[0].abs().max(p[1].abs());
                if edge >= 0.96 || !(0.0..=1.0).contains(&p[2]) {
                    continue;
                }
                assert!(
                    committed_valid(sampling, layer, i),
                    "positive primary blend escaped committed guard layer{layer} pixel{i}"
                );
                if layer == 2 || edge <= 0.60 {
                    return f64::from(layers[layer]);
                }
                // Every farther layer actually sampled by this blend must also
                // lie inside its own production footprint; the per-layer raw
                // metric therefore covers all rendered layer contributions.
                assert!(footprint_edge(sampling, layer + 1, i) < 0.96);
                assert!(
                    committed_valid(sampling, layer + 1, i),
                    "positive farther blend escaped committed guard layer{} pixel{i}",
                    layer + 1
                );
                let t = ((edge - 0.60) / (0.96 - 0.60)).clamp(0., 1.);
                let weight = t * t * (3. - 2. * t);
                return f64::from(layers[layer])
                    + f64::from(layers[layer + 1] - layers[layer]) * weight;
            }
            1.
        })
        .collect()
}
pub(super) fn signal_error(a: &[[f32; 4]], b: &[[f32; 4]]) -> f64 {
    a.iter()
        .zip(b)
        .map(|(a, b)| f64::from((a[3] - b[3]).abs()))
        .sum::<f64>()
        / a.len() as f64
        * 1000.
}
pub(super) fn temporal_residual(
    a: &[[f32; 4]],
    previous: &[[f32; 4]],
    expected: &[f64],
    old_expected: &[f64],
) -> f64 {
    a.iter()
        .zip(previous)
        .zip(expected.iter().zip(old_expected))
        .map(|((a, b), (e, old))| ((f64::from(a[3]) - f64::from(b[3])) - (e - old)).abs())
        .sum::<f64>()
        / a.len() as f64
        * 1000.
}
