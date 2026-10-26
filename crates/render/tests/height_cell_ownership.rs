#![cfg(not(target_arch = "wasm32"))]
//! Integer-page reference for the production height-cell ownership proof.
use std::collections::BTreeSet;

use bytemuck::{Pod, Zeroable};
use render::{near_cover::NearCoverGpu, terrain_gpu::UploadBudget};

#[derive(Clone, Copy, Debug)]
struct Bounds {
    lo: [f32; 3],
    hi: [f32; 3],
}
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuBounds {
    lo: [f32; 4],
    hi: [f32; 4],
}
struct Context {
    name: &'static str,
    intended: Option<[[i32; 3]; 2]>,
    extras: BTreeSet<[i32; 3]>,
    page_offset: [i32; 3],
    origin: [i64; 3],
}
impl Context {
    fn page(&self, page: [i32; 3]) -> [i32; 3] {
        std::array::from_fn(|axis| page[axis] + self.page_offset[axis])
    }
    fn intended_world(&self) -> Option<[[i32; 3]; 2]> {
        self.intended
            .map(|bounds| bounds.map(|page| self.page(page)))
    }
    fn extras_world(&self) -> BTreeSet<[i32; 3]> {
        self.extras.iter().map(|page| self.page(*page)).collect()
    }
    fn world(&self, point: [f32; 3]) -> [f64; 3] {
        std::array::from_fn(|axis| f64::from(point[axis]) + f64::from(self.page_offset[axis]) * 16.)
    }
    fn gpu(&self, bounds: Bounds) -> GpuBounds {
        let relative = |point| {
            let world = self.world(point);
            [
                (world[0] - self.origin[0] as f64) as f32,
                (world[1] - self.origin[1] as f64) as f32,
                (world[2] - self.origin[2] as f64) as f32,
                0.,
            ]
        };
        GpuBounds {
            lo: relative(bounds.lo),
            hi: relative(bounds.hi),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Coverage {
    None,
    All,
    Mixed,
}
// This reference enumerates every raw page touched by the domain. It does not
// use GPU bounds/bit-grid shortcuts, center samples, an epsilon, or shader text.
fn reference(context: &Context, extras: &BTreeSet<[i32; 3]>, bounds: Bounds) -> (Coverage, u64) {
    let lo = context.world(bounds.lo);
    let hi = context.world(bounds.hi);
    assert!(lo[0] < hi[0] && lo[2] < hi[2] && lo[1] <= hi[1]);
    let min = lo.map(|value| (value / 16.).floor() as i32);
    // X/Z support is half open; the height interval is CLOSED, including its
    // upper page when it lands exactly on a page boundary.
    let max = [
        (hi[0] / 16.).ceil() as i32 - 1,
        (hi[1] / 16.).floor() as i32,
        (hi[2] / 16.).ceil() as i32 - 1,
    ];
    let intended = context.intended_world();
    let (mut covered, mut visited) = (0, 0);
    for y in min[1]..=max[1] {
        for z in min[2]..=max[2] {
            for x in min[0]..=max[0] {
                let page = [x, y, z];
                let inside = intended.is_some_and(|[a, b]| {
                    (0..3).all(|axis| a[axis] <= page[axis] && page[axis] < b[axis])
                }) || extras.contains(&page);
                covered += u64::from(inside);
                visited += 1;
            }
        }
    }
    assert!(visited > 0);
    (
        if covered == 0 {
            Coverage::None
        } else if covered == visited {
            Coverage::All
        } else {
            Coverage::Mixed
        },
        visited,
    )
}

fn contexts() -> Vec<Context> {
    let low = [[-1, 0, -1], [2, 1, 2]];
    let sparse = BTreeSet::from([[-1, 0, -1], [1, 0, 1], [0, 2, 0], [-2, -1, 1], [3, 1, -2]]);
    vec![
        Context {
            name: "none",
            intended: None,
            extras: BTreeSet::new(),
            page_offset: [0; 3],
            origin: [0; 3],
        },
        Context {
            name: "low",
            intended: Some(low),
            extras: BTreeSet::new(),
            page_offset: [0; 3],
            origin: [0; 3],
        },
        Context {
            name: "high",
            intended: Some([[-1, 2, -1], [2, 4, 2]]),
            extras: BTreeSet::new(),
            page_offset: [0; 3],
            origin: [0; 3],
        },
        Context {
            name: "sparse",
            intended: None,
            extras: sparse.clone(),
            page_offset: [0; 3],
            origin: [0; 3],
        },
        Context {
            name: "low_sparse",
            intended: Some(low),
            extras: sparse.clone(),
            page_offset: [0; 3],
            origin: [0; 3],
        },
        Context {
            name: "rebased_low_sparse",
            intended: Some(low),
            extras: sparse,
            page_offset: [1024, -8, -2048],
            origin: [16_384, -128, -32_768],
        },
    ]
}

const BORDER_OUTSIDE: usize = 0;
const BORDER_INSIDE: usize = 1;
const SPARSE_HOLE: usize = 2;
const SPARSE_MIXED: usize = 3;
fn cases() -> Vec<Bounds> {
    let mut cases = vec![
        Bounds {
            lo: [-17., 7.9999, 4.],
            hi: [-16., 7.9999, 5.],
        },
        Bounds {
            lo: [-16., 7.9999, 4.],
            hi: [-15., 7.9999, 5.],
        },
        Bounds {
            lo: [-16., 7.9999, 0.],
            hi: [-15., 7.9999, 1.],
        },
        Bounds {
            lo: [-16., 7.9999, -16.],
            hi: [16., 7.9999, 16.],
        },
        Bounds {
            lo: [-16., 15.9999, 4.],
            hi: [-15., 16., 5.],
        },
    ];
    for x in -20..=20 {
        for z in -20..=20 {
            for step in [1, 2, 8, 16, 32] {
                for [min_y, max_y] in [
                    [7.9999, 7.9999],
                    [0., 0.],
                    [15.9999, 15.9999],
                    [16., 16.],
                    [32., 32.],
                    [-1., 1.],
                    [8., 40.],
                ] {
                    cases.push(Bounds {
                        lo: [x as f32, min_y, z as f32],
                        hi: [(x + step) as f32, max_y, (z + step) as f32],
                    });
                }
            }
        }
    }
    assert_eq!(cases.len(), 58_840);
    cases
}

#[test]
fn exhaustive_page_reference_respects_closed_height_and_sparse_union() {
    let contexts = contexts();
    let cases = cases();
    let low = &contexts[1];
    let sparse = &contexts[3];
    let extras = low.extras_world();
    assert_eq!(
        reference(low, &extras, cases[BORDER_OUTSIDE]),
        (Coverage::None, 1)
    );
    assert_eq!(
        reference(low, &extras, cases[BORDER_INSIDE]),
        (Coverage::All, 1)
    );
    assert_eq!(reference(low, &extras, cases[4]), (Coverage::Mixed, 2));
    let extras = sparse.extras_world();
    assert_eq!(
        reference(sparse, &extras, cases[SPARSE_HOLE]),
        (Coverage::None, 1)
    );
    assert_eq!(
        reference(sparse, &extras, cases[SPARSE_MIXED]),
        (Coverage::Mixed, 4)
    );
    let original = &contexts[4];
    let rebased = &contexts[5];
    let a = original.extras_world();
    let b = rebased.extras_world();
    for bounds in cases {
        assert_eq!(
            reference(original, &a, bounds),
            reference(rebased, &b, bounds)
        );
        assert_eq!(original.gpu(bounds).lo, rebased.gpu(bounds).lo);
        assert_eq!(original.gpu(bounds).hi, rebased.gpu(bounds).hi);
    }
}

#[test]
fn production_height_cell_proofs_match_exhaustive_raw_page_coverage() {
    pollster::block_on(async {
        let gpu = render::Gpu::request(&render::instance(), None, true)
            .await
            .unwrap();
        let source = format!(
            "{}\n{}",
            render::height_gpu::near_shader_source(false, false, false, true),
            r#"
struct OwnershipCase {lo:vec4<f32>,hi:vec4<f32>};
@group(0) @binding(22) var<storage,read> ownership_cases:array<OwnershipCase>;
@group(0) @binding(23) var<storage,read_write> ownership_results:array<u32>;
@compute @workgroup_size(64) fn ownership_probe(@builtin(global_invocation_id) at:vec3<u32>) {
    if at.x>=arrayLength(&ownership_cases){return;}
    let row=ownership_cases[at.x];
    ownership_results[at.x]=height_cell_class(row.lo.xyz,row.hi.xyz);
}
"#
        );
        let module = gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("production height AABB ownership plus independent compute probe"),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            });
        let pipeline = gpu
            .device
            .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("actual production height-cell classification"),
                layout: None,
                module: &module,
                entry_point: Some("ownership_probe"),
                compilation_options: Default::default(),
                cache: None,
            });
        let bounds = cases();
        let inputs = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("bounded independent height ownership AABBs"),
            size: bounds.len() as u64 * std::mem::size_of::<GpuBounds>() as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let output_bytes = bounds.len() as u64 * 4;
        let output = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("actual height ownership proof classifications"),
            size: output_bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("height ownership result readback"),
            size: output_bytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut near = NearCoverGpu::new(&gpu.device);
        let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("only reachable near mask and ownership compute inputs"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 21,
                    resource: near.buffer().as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 22,
                    resource: inputs.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 23,
                    resource: output.as_entire_binding(),
                },
            ],
        });
        let mut before_rebase = None;
        for context in contexts() {
            let extras = context.extras_world();
            near.upload(
                &gpu.queue,
                context.intended_world(),
                &extras.iter().copied().collect::<Vec<_>>(),
                context.origin,
                &mut UploadBudget::new(render::near_cover::UPLOAD_BYTES),
            )
            .unwrap();
            let rows: Vec<_> = bounds.iter().map(|bounds| context.gpu(*bounds)).collect();
            gpu.queue
                .write_buffer(&inputs, 0, bytemuck::cast_slice(&rows));
            let mut encoder = gpu.device.create_command_encoder(&Default::default());
            {
                let mut pass = encoder.begin_compute_pass(&Default::default());
                pass.set_pipeline(&pipeline);
                pass.set_bind_group(0, &bind, &[]);
                pass.dispatch_workgroups((bounds.len() as u32).div_ceil(64), 1, 1);
            }
            encoder.copy_buffer_to_buffer(&output, 0, &readback, 0, output_bytes);
            gpu.queue.submit([encoder.finish()]);
            let (tx, rx) = std::sync::mpsc::channel();
            readback
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |result| tx.send(result).unwrap());
            gpu.device.poll(wgpu::PollType::Wait).unwrap();
            rx.recv().unwrap().unwrap();
            let bytes = readback.slice(..).get_mapped_range();
            let classes: Vec<_> = bytes
                .chunks_exact(4)
                .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
                .collect();
            drop(bytes);
            readback.unmap();
            let (mut counts, mut visited) = ([0usize; 3], 0);
            for (index, (bounds, class)) in bounds.iter().zip(&classes).enumerate() {
                let (expected, pages) = reference(&context, &extras, *bounds);
                visited += pages;
                assert!(
                    *class <= 2,
                    "{} case{index} {bounds:?}: invalid class{class}",
                    context.name
                );
                counts[*class as usize] += 1;
                match class {
                    0 => assert_eq!(
                        expected,
                        Coverage::None,
                        "{} case{index} {bounds:?}: false provenOutside",
                        context.name
                    ),
                    1 => assert_eq!(
                        expected,
                        Coverage::All,
                        "{} case{index} {bounds:?}: false provenInside",
                        context.name
                    ),
                    2 => {}
                    _ => unreachable!(),
                }
            }
            if context.name == "low" {
                assert_eq!(
                    classes[BORDER_OUTSIDE], 0,
                    "half-open border x[-17,-16] is outside near x[-16,32]"
                );
                assert_eq!(
                    classes[BORDER_INSIDE], 1,
                    "x[-16,-15] is inside near x[-16,32]"
                );
            }
            if context.name == "sparse" {
                assert_ne!(
                    classes[SPARSE_HOLE], 1,
                    "sparse extra bounding box is not filled"
                );
                assert_eq!(
                    classes[SPARSE_MIXED], 2,
                    "sparse present+absent pages require the mixed path"
                );
            }
            if context.name == "low_sparse" {
                before_rebase = Some(classes.clone());
            }
            if context.name == "rebased_low_sparse" {
                let before = before_rebase.as_ref().unwrap();
                assert!(
                    classes.iter().zip(before).all(|(a, b)| a == b),
                    "integer origin rebase changed height ownership"
                );
            }
            println!(
                "height_cell_ownership context={} cases={} exhaustive_page_visits={visited} outside={} inside={} mixed={}",
                context.name,
                bounds.len(),
                counts[0],
                counts[1],
                counts[2]
            );
        }
    });
}
