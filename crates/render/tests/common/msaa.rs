//! Measure the device's actual sample locations instead of assuming API orientation.
fn pixel(gpu: &render::Gpu, view: &wgpu::TextureView, pixel: [u32; 2]) -> [[f32; 4]; 4] {
    let source = format!(
        "@group(0)@binding(0)var samples:texture_multisampled_2d<f32>;@group(0)@binding(1)var<storage,read_write>positions:array<vec4<f32>,4>;@compute @workgroup_size(4)fn read(@builtin(local_invocation_index)i:u32){{positions[i]=textureLoad(samples,vec2({}i,{}i),i32(i));}}",
        pixel[0], pixel[1]
    );
    let shader = gpu
        .device
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("read actual MSAA sample values"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
    let pipeline = gpu
        .device
        .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("actual MSAA sample readback"),
            layout: None,
            module: &shader,
            entry_point: Some("read"),
            compilation_options: Default::default(),
            cache: None,
        });
    let values = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("actual MSAA sample storage"),
        size: 64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let output = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("actual MSAA sample readback"),
        size: 64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("actual MSAA sample bindings"),
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: values.as_entire_binding(),
            },
        ],
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.dispatch_workgroups(1, 1, 1);
    }
    encoder.copy_buffer_to_buffer(&values, 0, &output, 0, 64);
    gpu.queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    output
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    rx.recv().unwrap().unwrap();
    let bytes = output.slice(..).get_mapped_range();
    let result = std::array::from_fn(|i| {
        std::array::from_fn(|c| {
            f32::from_le_bytes(
                bytes[i * 16 + c * 4..i * 16 + c * 4 + 4]
                    .try_into()
                    .unwrap(),
            )
        })
    });
    drop(bytes);
    output.unmap();
    result
}
pub fn locations(gpu: &render::Gpu) -> [[f64; 2]; 4] {
    let source = r#"
struct Vertex {@builtin(position)p:vec4<f32>,@location(0)@interpolate(linear,sample)uv:vec2<f32>};
@vertex fn vertex(@builtin(vertex_index)i:u32)->Vertex {
let p=array<vec2<f32>,3>(vec2(-1.,-1.),vec2(3.,-1.),vec2(-1.,3.));var o:Vertex;o.p=vec4(p[i],0.,1.);o.uv=vec2(p[i].x*.5+.5,.5-p[i].y*.5);return o;
}
@fragment fn fragment(input:Vertex,@builtin(sample_index)sample:u32)->@location(0)vec4<f32>{return vec4(input.uv,f32(sample),1.);}
"#;
    let shader = gpu
        .device
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("independent actual device MSAA sample locations"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
    let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("one pixel actual sample calibration"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 4,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let view = target.create_view(&Default::default());
    let render = gpu
        .device
        .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("sample-frequency calibration raster"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState {
                count: 4,
                ..Default::default()
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fragment"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba16Float,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
            cache: None,
        });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("raster one calibration pixel"),
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
        pass.set_pipeline(&render);
        pass.draw(0..3, 0..1);
    }
    gpu.queue.submit([encoder.finish()]);
    let values = pixel(gpu, &view, [0, 0]);
    let positions = std::array::from_fn(|i| {
        assert_eq!(values[i][2], i as f32);
        [f64::from(values[i][0]), f64::from(values[i][1])]
    });
    assert!(positions.iter().flatten().all(|v| *v > 0. && *v < 1.));
    positions
}
