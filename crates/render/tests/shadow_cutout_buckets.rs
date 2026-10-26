#![cfg(not(target_arch = "wasm32"))]
//! Exact production compute source; independent bounded arenas and CPU pair oracle.
use bytemuck::{Pod, Zeroable};
use render::terrain_mesh::{Face, QuadDescriptor};
use std::collections::BTreeSet;

const OBJECTS: usize = 128;
const QUADS_PER_OBJECT: usize = 8;
const CAPACITY: usize = OBJECTS * QUADS_PER_OBJECT;
const OPAQUE_START: usize = 64;
const CUTOUT_START: usize = OPAQUE_START + OBJECTS;
const VISIBLE_WORDS: usize = CUTOUT_START + CAPACITY * 2;
const SENTINEL: u32 = 0xa5a5_a5a5;
const LAYERS: u32 = 4;
type Pair = (u32, u32);

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CameraGpu {
    matrix: [[f32; 4]; 4],
    eye: [f32; 4],
    direction: [f32; 4],
    params: [u32; 4],
    opaque: [u32; 8],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ObjectGpu {
    model: [[f32; 4]; 4],
    inverse: [[f32; 4]; 4],
    minimum: [f32; 4],
    maximum: [f32; 4],
    data: [u32; 4],
    light: [f32; 4],
    reserved: [[u32; 4]; 4],
}

const _: () = assert!(std::mem::size_of::<CameraGpu>() == 144);
const _: () = assert!(std::mem::size_of::<ObjectGpu>() == 256);

#[derive(Clone, Copy, Debug)]
enum Contents {
    Full,
    Classified,
    Rejected,
}

fn fixture(contents: Contents) -> (Vec<ObjectGpu>, Vec<u32>, BTreeSet<Pair>, BTreeSet<Pair>) {
    let mut objects = vec![ObjectGpu::zeroed(); OBJECTS];
    // The physical source binding deliberately exceeds the configured quad arena.
    // Taking its arrayLength()/4 as visible capacity must fail this control.
    let mut payload = vec![SENTINEL; CAPACITY * 4 + 1024];
    let mut pass = BTreeSet::new();
    let mut mixed = BTreeSet::new();
    for (slot, object) in objects.iter_mut().enumerate() {
        object.model = [
            [1. / 32., 0., 0., 0.],
            [0., 1. / 32., 0., 0.],
            [0., 0., 1. / 32., 0.],
            [-0.25, -0.25, 0.25, 1.],
        ];
        object.inverse = [
            [32., 0., 0., 0.],
            [0., 32., 0., 0.],
            [0., 0., 32., 0.],
            [8., 8., -8., 1.],
        ];
        object.minimum = [-0.25, -0.25, 0.25, 0.];
        object.maximum = [0.25, 0.25, 0.75, 0.];
        let start = slot * QUADS_PER_OBJECT * 4;
        object.data = [start as u32, 0, 0, 1];
        object.reserved[0] = [2, start as u32, QUADS_PER_OBJECT as u32, 0];
        for q in 0..QUADS_PER_OBJECT {
            let word = start + q * 4;
            let selector = (slot * QUADS_PER_OBJECT + q) % 10;
            let material = match contents {
                Contents::Full => (q % 2) as u16,
                Contents::Classified => [0, 1, 2, 3, 4, 5, 6, 7, 8, u16::MAX][selector],
                Contents::Rejected => [2, 3, 4, 5, 8, u16::MAX][selector % 6],
            };
            let quad = QuadDescriptor::new(
                [q as u8, 0, 1],
                Face::South,
                [1, 1],
                material,
                [3; 4],
                u32::MAX,
            )
            .unwrap();
            payload[word..word + 4].copy_from_slice(&quad.words);
            let pair = (slot as u32, word as u32);
            // Deliberately enumerate expected cases instead of reproducing the
            // shader's bit tests or reading its emitted counters as an oracle.
            match (contents, material) {
                (Contents::Full | Contents::Classified, 0 | 6) => {
                    pass.insert(pair);
                }
                (Contents::Full | Contents::Classified, 1 | 7) => {
                    mixed.insert(pair);
                }
                _ => {}
            }
        }
    }
    assert!(pass.len() + mixed.len() <= CAPACITY);
    (objects, payload, pass, mixed)
}

struct Snapshot {
    visible: Vec<u32>,
    draws: Vec<u32>,
    footprint: Vec<u32>,
}

struct Control<'a> {
    gpu: &'a render::Gpu,
    camera: wgpu::Buffer,
    objects: wgpu::Buffer,
    payload: wgpu::Buffer,
    visible: wgpu::Buffer,
    draws: wgpu::Buffer,
    footprint: wgpu::Buffer,
    readback: wgpu::Buffer,
    bind: wgpu::BindGroup,
    reset: wgpu::ComputePipeline,
    cull: wgpu::ComputePipeline,
}

impl<'a> Control<'a> {
    fn new(gpu: &'a render::Gpu) -> Self {
        let buffer = |label, words, usage| {
            gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: (words * 4) as u64,
                usage,
                mapped_at_creation: false,
            })
        };
        let writable = wgpu::BufferUsages::STORAGE
            | wgpu::BufferUsages::COPY_DST
            | wgpu::BufferUsages::COPY_SRC;
        let camera = buffer(
            "bucket control camera",
            36,
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
        let tiles = buffer("bucket control unused tile header", 16, writable);
        let objects = buffer("bucket control object headers", OBJECTS * 64, writable);
        let payload = buffer(
            "bucket control oversized payload",
            CAPACITY * 4 + 1024,
            writable,
        );
        let visible = buffer(
            "bucket control exact visible capacity",
            VISIBLE_WORDS,
            writable,
        );
        let draws = buffer("bucket control four indirect records", 16, writable);
        let footprint = buffer("bucket control footprint", 64, writable);
        let materials = buffer("bucket control material facts", 8 * 4, writable);
        gpu.queue.write_buffer(
            &materials,
            0,
            bytemuck::cast_slice(&[
                [0u32, 2, 0, 0],     // PASS.
                [1, 0, 0, 0],        // MIXED.
                [2, 4, 0, 0],        // FAIL.
                [3, 6, 0, 0],        // Conflicting flags: FAIL takes precedence.
                [LAYERS, 2, 0, 0],   // Exact first invalid layer, despite PASS.
                [u32::MAX, 2, 0, 0], // Invalid layer cannot wrap into a valid one.
                [0, 3, 0, 0],        // High material ID, valid layer, PASS plus tint.
                [3, 1, 0, 0],        // Last valid layer, MIXED plus tint.
            ]),
        );
        let layout = gpu
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("bucket control exact seven-storage layout"),
                entries: &(0..8)
                    .map(|binding| wgpu::BindGroupLayoutEntry {
                        binding,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: if binding == 0 {
                                wgpu::BufferBindingType::Uniform
                            } else {
                                wgpu::BufferBindingType::Storage {
                                    read_only: matches!(binding, 1 | 2 | 6 | 7),
                                }
                            },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    })
                    .collect::<Vec<_>>(),
            });
        let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bucket control unchanged production binding numbers"),
            layout: &layout,
            entries: &[
                &camera, &tiles, &objects, &visible, &draws, &footprint, &payload, &materials,
            ]
            .into_iter()
            .enumerate()
            .map(|(binding, buffer)| wgpu::BindGroupEntry {
                binding: binding as u32,
                resource: buffer.as_entire_binding(),
            })
            .collect::<Vec<_>>(),
        });
        let source =
            render::distant_shadows::shader_sources(OPAQUE_START as u32, CUTOUT_START as u32)
                .into_iter()
                .next()
                .unwrap();
        let module = gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("exact production distant cutout bucket compute source"),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            });
        let pipeline_layout = gpu
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("bucket control shared reset/cull layout"),
                bind_group_layouts: &[&layout],
                push_constant_ranges: &[],
            });
        let pipeline = |entry| {
            gpu.device
                .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some(entry),
                    layout: Some(&pipeline_layout),
                    module: &module,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    cache: None,
                })
        };
        Self {
            gpu,
            camera,
            objects,
            payload,
            visible,
            draws,
            footprint,
            readback: buffer(
                "bucket control bounded readback",
                VISIBLE_WORDS + 16 + 64,
                wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            ),
            bind,
            reset: pipeline("reset"),
            cull: pipeline("cull"),
        }
    }

    fn dispatch(&self, collect: bool, count: u32) -> Snapshot {
        let mut camera = CameraGpu::zeroed();
        for axis in 0..4 {
            camera.matrix[axis][axis] = 1.;
        }
        camera.params = [0, count, 64, (LAYERS << 1) | u32::from(collect)];
        self.gpu
            .queue
            .write_buffer(&self.camera, 0, bytemuck::bytes_of(&camera));
        // Every field, including first_instance, must be rewritten by reset.
        self.gpu
            .queue
            .write_buffer(&self.draws, 0, bytemuck::cast_slice(&[u32::MAX; 16]));
        let mut encoder = self.gpu.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_bind_group(0, &self.bind, &[]);
            pass.set_pipeline(&self.reset);
            pass.dispatch_workgroups(1, 1, 1);
            pass.set_pipeline(&self.cull);
            pass.dispatch_workgroups((OBJECTS as u32).div_ceil(64), 1, 1);
        }
        let mut offset = 0;
        for source in [&self.visible, &self.draws, &self.footprint] {
            encoder.copy_buffer_to_buffer(source, 0, &self.readback, offset, source.size());
            offset += source.size();
        }
        self.gpu.queue.submit([encoder.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        self.readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
        self.gpu.device.poll(wgpu::PollType::Wait).unwrap();
        rx.recv().unwrap().unwrap();
        let values: Vec<u32> = self
            .readback
            .slice(..)
            .get_mapped_range()
            .chunks_exact(4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
            .collect();
        self.readback.unmap();
        Snapshot {
            visible: values[..VISIBLE_WORDS].to_vec(),
            draws: values[VISIBLE_WORDS..VISIBLE_WORDS + 16].to_vec(),
            footprint: values[VISIBLE_WORDS + 16..].to_vec(),
        }
    }
}

fn assert_buckets(snapshot: &Snapshot, pass: &BTreeSet<Pair>, mixed: &BTreeSet<Pair>) {
    assert_eq!(
        snapshot.draws,
        [
            1536,
            0,
            0,
            0,
            36,
            0,
            0,
            0,
            4,
            mixed.len() as u32,
            0,
            0,
            4,
            pass.len() as u32,
            0,
            0
        ],
        "wrong indirect counts, static fields or bucket ordering"
    );
    assert!(
        snapshot.visible[..CUTOUT_START]
            .iter()
            .all(|&v| v == SENTINEL)
    );
    let pairs: Vec<Pair> = snapshot.visible[CUTOUT_START..]
        .chunks_exact(2)
        .map(|p| (p[0], p[1]))
        .collect();
    let actual_pass: BTreeSet<_> = pairs[..pass.len()].iter().copied().collect();
    let actual_mixed: BTreeSet<_> = pairs[CAPACITY - mixed.len()..]
        .iter()
        .rev()
        .copied()
        .collect();
    assert_eq!(actual_pass.len(), pass.len(), "duplicate PASS reservation");
    assert_eq!(
        actual_mixed.len(),
        mixed.len(),
        "duplicate MIXED reservation"
    );
    assert_eq!(&actual_pass, pass, "wrong or missing PASS object/quad pair");
    assert_eq!(
        &actual_mixed, mixed,
        "wrong or missing MIXED object/quad pair"
    );
    assert!(
        pairs[pass.len()..CAPACITY - mixed.len()]
            .iter()
            .all(|&p| p == (SENTINEL, SENTINEL)),
        "unexpected write into the unused middle"
    );
}

#[test]
fn production_cutout_buckets_are_disjoint_bounded_and_reset_without_ghosts() {
    let gpu = pollster::block_on(render::Gpu::request(&render::instance(), None, true))
        .expect("actual GPU required for independent cutout bucket control");
    let control = Control::new(&gpu);
    let untouched = vec![SENTINEL; 64];
    for contents in [Contents::Full, Contents::Classified] {
        let (objects, payload, pass, mixed) = fixture(contents);
        if matches!(contents, Contents::Full) {
            assert_eq!(pass.len() + mixed.len(), CAPACITY);
        }
        gpu.queue
            .write_buffer(&control.objects, 0, bytemuck::cast_slice(&objects));
        gpu.queue
            .write_buffer(&control.payload, 0, bytemuck::cast_slice(&payload));
        gpu.queue.write_buffer(
            &control.visible,
            0,
            bytemuck::cast_slice(&vec![SENTINEL; VISIBLE_WORDS]),
        );
        gpu.queue
            .write_buffer(&control.footprint, 0, bytemuck::cast_slice(&untouched));
        let snapshot = control.dispatch(false, OBJECTS as u32);
        assert_buckets(&snapshot, &pass, &mixed);
        assert_eq!(
            snapshot.footprint, untouched,
            "packed layer bits enabled footprint writes"
        );
    }

    let (mut objects, payload, pass, mixed) = fixture(Contents::Classified);
    gpu.queue
        .write_buffer(&control.objects, 0, bytemuck::cast_slice(&objects));
    gpu.queue
        .write_buffer(&control.payload, 0, bytemuck::cast_slice(&payload));
    gpu.queue.write_buffer(
        &control.visible,
        0,
        bytemuck::cast_slice(&vec![SENTINEL; VISIBLE_WORDS]),
    );
    let mut footprint = untouched.clone();
    footprint[12..16].copy_from_slice(&[u32::MAX, u32::MAX, 0, 0]);
    gpu.queue
        .write_buffer(&control.footprint, 0, bytemuck::cast_slice(&footprint));
    let collected = control.dispatch(true, OBJECTS as u32);
    assert_buckets(&collected, &pass, &mixed);
    footprint[12..16].copy_from_slice(&[22, 22, 42, 42]);
    assert_eq!(
        collected.footprint, footprint,
        "collect bit did not reduce the exact object bounds"
    );

    // Keep the old populated visible arena: zero counts must make it unreachable.
    for object in &mut objects {
        object.data[3] = 0;
    }
    gpu.queue
        .write_buffer(&control.objects, 0, bytemuck::cast_slice(&objects));
    let disabled = control.dispatch(true, OBJECTS as u32);
    assert_eq!(
        disabled.draws,
        [1536, 0, 0, 0, 36, 0, 0, 0, 4, 0, 0, 0, 4, 0, 0, 0]
    );
    assert_eq!(
        disabled.visible, collected.visible,
        "disabled objects wrote visible entries"
    );
    assert_eq!(disabled.footprint, collected.footprint);
    let (objects, payload, _, _) = fixture(Contents::Rejected);
    gpu.queue
        .write_buffer(&control.objects, 0, bytemuck::cast_slice(&objects));
    gpu.queue
        .write_buffer(&control.payload, 0, bytemuck::cast_slice(&payload));
    for count in [OBJECTS as u32, 0, OBJECTS as u32] {
        let empty = control.dispatch(false, count);
        assert_eq!(
            empty.draws, disabled.draws,
            "rejected/empty dispatch retained indirect instances"
        );
        assert_eq!(
            empty.visible, disabled.visible,
            "rejected/empty dispatch polluted old visible data"
        );
        assert_eq!(empty.footprint, disabled.footprint);
    }
    println!(
        "CUTOUT_BUCKETS_NATIVE objects128 arena1024 full=EXACT mixed=EXACT invalid=SKIP oversizedPayload=BOUNDED packedCollect=EXACT emptyReset=EXACT"
    );
}
