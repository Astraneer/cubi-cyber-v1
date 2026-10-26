//! Page-aligned near coverage. Intended raw cells remain Unknown until their
//! meshes arrive; old resident pages stay excluded from far rendering until
//! their GPU retirement. The mask contains no world cells or material IDs.
use crate::terrain_gpu::UploadBudget;
const EDGE: usize = 32;
const HEADER: usize = 16;
const WORDS: usize = HEADER + EDGE * EDGE * EDGE / 32;
pub const UPLOAD_BYTES: u64 = (WORDS * 4) as u64;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Bounds,
    Extent,
    UploadBudget,
}
pub struct NearCoverGpu {
    buffer: wgpu::Buffer,
    current: Box<[u32; WORDS]>,
    staged: Box<[u32; WORDS]>,
}
impl NearCoverGpu {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            buffer: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("intended and resident 3D near coverage"),
                size: UPLOAD_BYTES,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            current: Box::new([0; WORDS]),
            staged: Box::new([0; WORDS]),
        }
    }
    pub fn buffer(&self) -> &wgpu::Buffer {
        &self.buffer
    }
    /// Validate the complete replacement before any queue write. Pages outside
    /// the bounded grid refuse publication rather than disappearing from it.
    pub fn upload(
        &mut self,
        queue: &wgpu::Queue,
        intended: Option<[[i32; 3]; 2]>,
        extras: &[[i32; 3]],
        origin: [i64; 3],
        budget: &mut UploadBudget,
    ) -> Result<u64, Error> {
        self.staged.fill(0);
        if let Some([min, max]) = intended {
            if (0..3).any(|a| min[a] >= max[a]) {
                return Err(Error::Bounds);
            }
            for a in 0..3 {
                self.staged[a] = ((f64::from(min[a]) * 16. - origin[a] as f64) as f32).to_bits();
                self.staged[4 + a] =
                    ((f64::from(max[a]) * 16. - origin[a] as f64) as f32).to_bits();
            }
            self.staged[3] = 1;
        }
        if !extras.is_empty() {
            let mut min = extras[0];
            let mut max = min;
            for p in extras {
                for a in 0..3 {
                    min[a] = min[a].min(p[a]);
                    max[a] = max[a].max(p[a]);
                }
            }
            if (0..3).any(|a| i64::from(max[a]) - i64::from(min[a]) >= EDGE as i64) {
                return Err(Error::Extent);
            }
            for a in 0..3 {
                self.staged[8 + a] =
                    ((f64::from(min[a]) * 16. - origin[a] as f64) as f32).to_bits();
            }
            self.staged[11] = 1;
            for a in 0..3 {
                self.staged[12 + a] =
                    (((f64::from(max[a]) + 1.) * 16. - origin[a] as f64) as f32).to_bits();
            }
            for p in extras {
                let x = (p[0] - min[0]) as usize;
                let y = (p[1] - min[1]) as usize;
                let z = (p[2] - min[2]) as usize;
                let i = x + EDGE * (z + EDGE * y);
                self.staged[HEADER + i / 32] |= 1 << (i % 32);
            }
        }
        if self.current == self.staged {
            return Ok(0);
        }
        if budget.remaining_bytes < UPLOAD_BYTES {
            return Err(Error::UploadBudget);
        }
        queue.write_buffer(&self.buffer, 0, bytemuck::cast_slice(&self.staged[..]));
        self.current.copy_from_slice(&self.staged[..]);
        budget.remaining_bytes -= UPLOAD_BYTES;
        Ok(UPLOAD_BYTES)
    }
}
pub const SHADER: &str = r#"
@group(0) @binding(21) var<storage,read> near_cover:array<vec4<u32>>;
fn covered_by_near(world:vec3<f32>)->bool {
    let lo=bitcast<vec3<f32>>(near_cover[0].xyz);
    let hi=bitcast<vec3<f32>>(near_cover[1].xyz);
    if near_cover[0].w!=0u && all(world>=lo) && all(world<hi) {return true;}
    if near_cover[2].w==0u {return false;}
    let at=floor((world-bitcast<vec3<f32>>(near_cover[2].xyz))/16.);
    if any(at<vec3(0.)) || any(at>=vec3(32.)) {return false;}
    let p=vec3<u32>(at);let index=p.x+32u*(p.z+32u*p.y);
    let word=4u+index/128u;let lane=(index/32u)%4u;
    return (near_cover[word][lane] & (1u<<(index%32u)))!=0u;
}
"#;
