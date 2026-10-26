#![cfg(not(target_arch = "wasm32"))]
#[path = "../src/shadow_footprint.rs"]
#[allow(dead_code)]
mod footprint;
use footprint::Footprints;
fn object(low: [f32; 3], high: [f32; 3], enabled: u32, opaque: u32) -> [u8; 128] {
    let mut b = [0u8; 128];
    for (i, v) in [
        1f32, 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.,
    ]
    .into_iter()
    .enumerate()
    {
        b[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    for (i, v) in low.into_iter().enumerate() {
        b[64 + i * 4..68 + i * 4].copy_from_slice(&v.to_le_bytes());
    }
    for (i, v) in high.into_iter().enumerate() {
        b[80 + i * 4..84 + i * 4].copy_from_slice(&v.to_le_bytes());
    }
    b[100..104].copy_from_slice(&opaque.to_le_bytes());
    b[120..124].copy_from_slice(&1u32.to_le_bytes());
    b[124..128].copy_from_slice(&enabled.to_le_bytes());
    b
}
fn read(gpu: &render::Gpu, b: &wgpu::Buffer) -> [u32; 28] {
    let r = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 112,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut e = gpu.device.create_command_encoder(&Default::default());
    e.copy_buffer_to_buffer(b, 0, &r, 0, 112);
    gpu.queue.submit([e.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    r.slice(..)
        .map_async(wgpu::MapMode::Read, move |v| tx.send(v).unwrap());
    gpu.device.poll(wgpu::PollType::Wait).unwrap();
    rx.recv().unwrap().unwrap();
    let v = r.slice(..).get_mapped_range();
    let out = std::array::from_fn(|i| u32::from_le_bytes(v[i * 4..i * 4 + 4].try_into().unwrap()));
    drop(v);
    r.unmap();
    out
}
#[test]
fn gpu_old_new_footprints_cover_static_models_removal_and_full_fallback() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let d = &gpu.device;
        let objects = d.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 256,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        gpu.queue
            .write_buffer(&objects, 0, &object([-4., -2., -3.], [4., 2., 3.], 1, 1));
        // Enabled water-only huge object must not expand the opaque caster footprint.
        gpu.queue
            .write_buffer(&objects, 128, &object([-100.; 3], [100.; 3], 1, 0));
        let mut uniform = [0u8; 96];
        let matrix = [
            1f32 / 32.,
            0.,
            0.,
            0.,
            0.,
            1. / 32.,
            0.,
            0.,
            0.,
            0.,
            1. / 2048.,
            0.,
            0.,
            0.,
            0.5,
            1.,
        ];
        for (i, v) in matrix.into_iter().enumerate() {
            uniform[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        uniform[64..68].copy_from_slice(&64f32.to_le_bytes());
        uniform[68..72].copy_from_slice(&64f32.to_le_bytes());
        uniform[80..84].copy_from_slice(&2u32.to_le_bytes());
        let cameras = std::array::from_fn(|_| {
            let b = d.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: 96,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            gpu.queue.write_buffer(&b, 0, &uniform);
            b
        });
        let f = Footprints::new(d, &gpu.queue, &objects, &cameras, 2, 64);
        let source = format!(
            "{}\n{}\n{}",
            footprint::COMMON,
            footprint::MODEL,
            "@compute @workgroup_size(1)fn add_model(){include_model_shadow_bounds(mat4x4<f32>(vec4(1./32.,0.,0.,0.),vec4(0.,1./32.,0.,0.),vec4(0.,0.,1./2048.,0.),vec4(0.,0.,0.5,1.)),64.,vec3(10.,-2.,0.),vec3(12.,2.,2.));}"
        );
        let module = d.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: None,
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let layout = d.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 5,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let bind = d.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 5,
                resource: f.binding(0),
            }],
        });
        let pl = d.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let add = d.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: None,
            layout: Some(&pl),
            module: &module,
            entry_point: Some("add_model"),
            compilation_options: Default::default(),
            cache: None,
        });
        let encode = |include_model: bool, full: bool| {
            let mut e = d.create_command_encoder(&Default::default());
            f.static_bounds(&mut e, 0);
            if include_model {
                let mut p = e.begin_compute_pass(&Default::default());
                p.set_pipeline(&add);
                p.set_bind_group(0, &bind, &[]);
                p.dispatch_workgroups(1, 1, 1);
            }
            f.combined_bounds(&mut e, 0, !full);
            gpu.queue.submit([e.finish()]);
            read(&gpu, &f.buffer)
        };
        let a = encode(true, false);
        assert_eq!(&a[8..12], &[23, 25, 41, 39]);
        assert_eq!(&a[20..24], &[23, 25, 46, 39]);
        assert_eq!(&a[24..28], &[252, 322, 252, 322]);
        gpu.queue
            .write_buffer(&objects, 0, &object([-12., -2., 0.], [-8., 2., 5.], 1, 1));
        let b = encode(true, false);
        assert_eq!(&b[8..12], &[15, 25, 41, 39]);
        assert_eq!(&b[20..24], &[15, 25, 46, 39]);
        assert_eq!(&b[24..28], &[364, 434, 196, 434]);
        gpu.queue
            .write_buffer(&objects, 0, &object([-12., -2., 0.], [-8., 2., 5.], 0, 1));
        let c = encode(false, false);
        assert_eq!(&c[8..12], &[15, 25, 29, 39]);
        assert_eq!(&c[20..24], &[15, 25, 46, 39]);
        let empty = encode(false, false);
        assert_eq!(&empty[24..28], &[0; 4]);
        let full = encode(false, true);
        assert_eq!(&full[20..24], &[0, 0, 64, 64]);
        assert_eq!(full[25], 4096);
        println!(
            "GPU_shadowFootprints static252/model322 moved364/434 removed196/434 empty0 fallback4096; noCPUreadbackneededforencoding"
        );
    });
}
