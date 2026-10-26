//! GPU-resident conservative old/new caster footprints. No readback or frame
//! allocation is needed to encode a clear/restore rectangle.
use std::num::NonZeroU64;
pub(crate) const STRIDE: u64 = 256;
pub(crate) const COMMON: &str = include_str!("shadow_footprint_common.wgsl");
pub(crate) const MODEL: &str = r#"
@group(0)@binding(5)var<storage,read_write>shadow_footprint:ShadowFootprintAtomic;
fn include_model_shadow_bounds(matrix:mat4x4<f32>,resolution:f32,minimum:vec3<f32>,maximum:vec3<f32>){
 let r=project_shadow_rect(matrix,resolution,minimum,maximum);if !shadow_rect_valid(r){return;}
 atomicMin(&shadow_footprint.new_combined[0],r.x);atomicMin(&shadow_footprint.new_combined[1],r.y);
 atomicMax(&shadow_footprint.new_combined[2],r.z);atomicMax(&shadow_footprint.new_combined[3],r.w);
}
"#;
pub(crate) struct Footprints {
    pub buffer: wgpu::Buffer,
    full: wgpu::Buffer,
    binds: [wgpu::BindGroup; 3],
    reset: wgpu::ComputePipeline,
    terrain: wgpu::ComputePipeline,
    finish: wgpu::ComputePipeline,
    combined: wgpu::ComputePipeline,
    combined_full: wgpu::ComputePipeline,
    objects: u32,
}
impl Footprints {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        objects: &wgpu::Buffer,
        cameras: &[wgpu::Buffer; 3],
        capacity: u32,
        resolution: u32,
    ) -> Self {
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("three GPU old/new shadow footprint records"),
            size: STRIDE * 3,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let full = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("constant full shadow rectangle"),
            size: 16,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let mut data = [0u8; 16];
        data[8..12].copy_from_slice(&resolution.to_le_bytes());
        data[12..16].copy_from_slice(&resolution.to_le_bytes());
        queue.write_buffer(&full, 0, &data);
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("shadow footprint reduction layout"),
            entries: &[
                entry(0, wgpu::BufferBindingType::Uniform),
                entry(1, wgpu::BufferBindingType::Storage { read_only: true }),
                entry(2, wgpu::BufferBindingType::Storage { read_only: false }),
            ],
        });
        let binds = std::array::from_fn(|i| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("shadow footprint reduction inputs"),
                layout: &layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: cameras[i].as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: objects.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &buffer,
                            offset: i as u64 * STRIDE,
                            size: NonZeroU64::new(STRIDE),
                        }),
                    },
                ],
            })
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("shadow footprint pipelines"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let source = format!("{}\n{}", COMMON, include_str!("shadow_footprint.wgsl"));
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("conservative shadow caster reduction"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let pipeline = |name| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(name),
                layout: Some(&pl),
                module: &shader,
                entry_point: Some(name),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        Self {
            buffer,
            full,
            binds,
            reset: pipeline("reset_bounds"),
            terrain: pipeline("reduce_terrain"),
            finish: pipeline("finish_terrain"),
            combined: pipeline("finish_combined"),
            combined_full: pipeline("finish_combined_full"),
            objects: capacity,
        }
    }
    pub fn binding(&self, cascade: usize) -> wgpu::BindingResource<'_> {
        wgpu::BindingResource::Buffer(wgpu::BufferBinding {
            buffer: &self.buffer,
            offset: cascade as u64 * STRIDE,
            size: NonZeroU64::new(STRIDE),
        })
    }
    pub fn static_bounds(&self, encoder: &mut wgpu::CommandEncoder, cascade: usize) {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("GPU old/new terrain shadow footprint"),
            timestamp_writes: None,
        });
        pass.set_bind_group(0, &self.binds[cascade], &[]);
        for (pipeline, groups) in [
            (&self.reset, 1),
            (&self.terrain, self.objects.div_ceil(64)),
            (&self.finish, 1),
        ] {
            pass.set_pipeline(pipeline);
            pass.dispatch_workgroups(groups, 1, 1);
        }
    }
    pub fn seed_combined(&self, encoder: &mut wgpu::CommandEncoder, cascade: usize) {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("seed model shadow bounds from static geometry"),
            timestamp_writes: None,
        });
        pass.set_bind_group(0, &self.binds[cascade], &[]);
        pass.set_pipeline(&self.finish);
        pass.dispatch_workgroups(1, 1, 1);
    }
    pub fn combined_bounds(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        cascade: usize,
        complete: bool,
    ) {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("GPU old/new combined shadow footprint"),
            timestamp_writes: None,
        });
        pass.set_bind_group(0, &self.binds[cascade], &[]);
        pass.set_pipeline(if complete {
            &self.combined
        } else {
            &self.combined_full
        });
        pass.dispatch_workgroups(1, 1, 1);
    }
    pub fn mark_static_full(&self, encoder: &mut wgpu::CommandEncoder, cascade: usize) {
        for field in [16, 32] {
            encoder.copy_buffer_to_buffer(
                &self.full,
                0,
                &self.buffer,
                cascade as u64 * STRIDE + field,
                16,
            );
        }
    }
}
