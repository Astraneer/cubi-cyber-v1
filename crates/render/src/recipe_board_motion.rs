//! Optional namespace19 motion for the exact prepared card quads. Previous
//! records come from the last PRESENTED token, not the last queue upload.
use crate::{
    recipe_boards::{BoardGpu, RecipeBoardGpu},
    taa::FrameToken,
    terrain_gpu::{Camera, UploadBudget},
};
use bytemuck::{Pod, Zeroable};
const PARAMETERS_BYTES: u64 = 96;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidSource,
    InvalidFrame,
    MemoryBudget,
    UploadBudget,
    Busy,
    WrongToken,
    NotEncoded,
    GenerationExhausted,
    Shader,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
struct Parameters {
    previous_vp: [[f32; 4]; 4],
    inverse_extent_jitter: [f32; 4],
    flags: [u32; 4],
}
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
struct SurfacePair {
    current: [u32; 4],
    previous: [u32; 4],
}
const _: () = assert!(std::mem::size_of::<Parameters>() == PARAMETERS_BYTES as usize);
const _: () = assert!(std::mem::size_of::<SurfacePair>() == 32);
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Work {
    pub upload_bytes: u64,
    pub previous_records: u32,
    pub identity_records: u32,
    pub draws: u32,
    pub pipeline_compilations: u32,
}
#[derive(Clone, Copy)]
struct Candidate {
    token: FrameToken,
    vp: Camera,
    encoded: bool,
}
pub struct RecipeBoardMotionGpu {
    primary: wgpu::BindGroup,
    pipeline: wgpu::RenderPipeline,
    bind: wgpu::BindGroup,
    previous: wgpu::Buffer,
    identities: wgpu::Buffer,
    parameters: wgpu::Buffer,
    front_records: Vec<BoardGpu>,
    candidate_records: Vec<BoardGpu>,
    last_records: Vec<BoardGpu>,
    uploaded_previous: Vec<BoardGpu>,
    pairs: Vec<SurfacePair>,
    staging_pairs: Vec<SurfacePair>,
    generations: Vec<u32>,
    staging_generations: Vec<u32>,
    front_pairs: Vec<[u32; 4]>,
    front_camera: Option<Camera>,
    last_session: Option<u64>,
    last_parameters: Option<Parameters>,
    candidate: Option<Candidate>,
    last_serial: u64,
    front_frame: u64,
    active: u32,
}
impl RecipeBoardMotionGpu {
    pub fn required_buffer_bytes(capacity: u32) -> Result<u64, Error> {
        if capacity == 0 || capacity > 32 {
            return Err(Error::InvalidSource);
        }
        Ok(u64::from(capacity) * 96 + PARAMETERS_BYTES)
    }
    pub fn new(
        device: &wgpu::Device,
        source: &RecipeBoardGpu,
        budget_bytes: u64,
    ) -> Result<Self, Error> {
        if source.sample_count() != 1 {
            return Err(Error::InvalidSource);
        }
        let count = source.capacity() as usize;
        if Self::required_buffer_bytes(source.capacity())? > budget_bytes {
            return Err(Error::MemoryBudget);
        }
        let buffer = |label, size, uniform| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage: wgpu::BufferUsages::COPY_DST
                    | if uniform {
                        wgpu::BufferUsages::UNIFORM
                    } else {
                        wgpu::BufferUsages::STORAGE
                    },
                mapped_at_creation: false,
            })
        };
        let previous = buffer("presented recipe card64B poses", count as u64 * 64, false);
        let identities = buffer(
            "exact card session/lifetime identities",
            count as u64 * 32,
            false,
        );
        let parameters = buffer("presented recipe camera", PARAMETERS_BYTES, true);
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("recipe card previous PRESENTED source"),
            entries: &[
                entry(0, true, wgpu::ShaderStages::VERTEX_FRAGMENT),
                entry(1, false, wgpu::ShaderStages::VERTEX),
                entry(2, false, wgpu::ShaderStages::VERTEX),
            ],
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &layout,
            entries: &[
                binding(0, &parameters),
                binding(1, &previous),
                binding(2, &identities),
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("exact world recipe card motion19"),
            source: wgpu::ShaderSource::Wgsl(shader_source()?.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[source.primary_layout(), &layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("recipe card motion19 Equal"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("motion_vertex"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("motion_fragment"),
                compilation_options: Default::default(),
                targets: &[
                    Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba16Float,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    }),
                    Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba32Uint,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    }),
                ],
            }),
            primitive: wgpu::PrimitiveState {
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::Equal,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            multiview: None,
            cache: None,
        });
        Ok(Self {
            primary: source.primary_bind().clone(),
            pipeline,
            bind,
            previous,
            identities,
            parameters,
            front_records: vec![BoardGpu::zeroed(); count],
            candidate_records: vec![BoardGpu::zeroed(); count],
            last_records: vec![BoardGpu::zeroed(); count],
            uploaded_previous: vec![BoardGpu::zeroed(); count],
            pairs: vec![SurfacePair::zeroed(); count],
            staging_pairs: vec![SurfacePair::zeroed(); count],
            generations: vec![0; count],
            staging_generations: vec![0; count],
            front_pairs: vec![[0; 4]; count],
            front_camera: None,
            last_session: None,
            last_parameters: None,
            candidate: None,
            last_serial: 0,
            front_frame: 0,
            active: 0,
        })
    }
    pub fn buffer_bytes(&self) -> u64 {
        self.previous.size() + self.identities.size() + self.parameters.size()
    }
    pub fn startup_pipeline_compilations(&self) -> u32 {
        1
    }
    pub fn required_upload_bytes(&self) -> u64 {
        self.buffer_bytes()
    }
    pub fn pending(&self) -> Option<FrameToken> {
        self.candidate.map(|c| c.token)
    }
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_frame(
        &mut self,
        queue: &wgpu::Queue,
        source: &RecipeBoardGpu,
        session: Option<u64>,
        token: FrameToken,
        base: Camera,
        extent: [u32; 2],
        jitter: [f32; 2],
        budget: &mut UploadBudget,
    ) -> Result<Work, Error> {
        if self.candidate.is_some() {
            return Err(Error::Busy);
        }
        if self.primary != *source.primary_bind()
            || source.capacity() as usize != self.front_records.len()
        {
            return Err(Error::InvalidSource);
        }
        if token.serial() <= self.last_serial
            || token.frame() <= self.front_frame
            || extent.contains(&0)
            || base
                .view_projection
                .iter()
                .flatten()
                .chain(jitter.iter())
                .any(|v| !v.is_finite())
            || session == Some(0)
            || jitter.iter().any(|v| v.abs() > 0.5)
        {
            return Err(Error::InvalidFrame);
        }
        self.staging_generations.copy_from_slice(&self.generations);
        for (index, record) in source.uploaded_records().iter().enumerate() {
            let visible = record.codes[2] & 1 != 0 && session.is_some();
            let old = &self.last_records[index];
            if visible
                && (old.codes[2] & 1 == 0
                    || self.last_session != session
                    || !same_geometry(record, old))
            {
                self.staging_generations[index] = self.generations[index]
                    .checked_add(1)
                    .ok_or(Error::GenerationExhausted)?;
            }
            let uid = session.unwrap_or(0);
            let identity = if visible {
                [
                    0,
                    uid as u32,
                    (uid >> 32) as u32,
                    self.staging_generations[index],
                ]
            } else {
                [0; 4]
            };
            self.staging_pairs[index] = SurfacePair {
                current: identity,
                previous: self.front_pairs[index],
            };
        }
        let previous_changed = self.uploaded_previous != self.front_records;
        let identities_changed = self.pairs != self.staging_pairs;
        let parameters = Parameters {
            previous_vp: self.front_camera.unwrap_or(base).view_projection,
            inverse_extent_jitter: [
                1. / extent[0] as f32,
                1. / extent[1] as f32,
                jitter[0],
                jitter[1],
            ],
            flags: [u32::from(self.front_camera.is_some()), 0, 0, 0],
        };
        let camera_changed = self.last_parameters != Some(parameters);
        let bytes = u64::from(previous_changed) * self.previous.size()
            + u64::from(identities_changed) * self.identities.size()
            + u64::from(camera_changed) * PARAMETERS_BYTES;
        if budget.remaining_bytes < bytes {
            return Err(Error::UploadBudget);
        }
        if previous_changed {
            queue.write_buffer(&self.previous, 0, bytemuck::cast_slice(&self.front_records));
            self.uploaded_previous.copy_from_slice(&self.front_records);
        }
        if identities_changed {
            queue.write_buffer(
                &self.identities,
                0,
                bytemuck::cast_slice(&self.staging_pairs),
            );
            self.pairs.copy_from_slice(&self.staging_pairs);
        }
        if camera_changed {
            queue.write_buffer(&self.parameters, 0, bytemuck::bytes_of(&parameters));
            self.last_parameters = Some(parameters);
        }
        budget.remaining_bytes -= bytes;
        self.generations.copy_from_slice(&self.staging_generations);
        self.last_records.copy_from_slice(source.uploaded_records());
        self.candidate_records
            .copy_from_slice(source.uploaded_records());
        self.last_session = session;
        self.active = source.active_boards();
        self.last_serial = token.serial();
        self.candidate = Some(Candidate {
            token,
            vp: base,
            encoded: false,
        });
        Ok(Work {
            upload_bytes: bytes,
            previous_records: if previous_changed {
                source.capacity()
            } else {
                0
            },
            identity_records: if identities_changed {
                source.capacity()
            } else {
                0
            },
            ..Default::default()
        })
    }
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, token: FrameToken) -> Result<Work, Error> {
        if self.candidate.is_none_or(|c| c.token != token) {
            return Err(Error::WrongToken);
        }
        if self.active == 0 {
            return Ok(Work::default());
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.primary, &[]);
        pass.set_bind_group(1, &self.bind, &[]);
        pass.draw(0..6, 0..self.front_records.len() as u32);
        Ok(Work {
            draws: 1,
            ..Default::default()
        })
    }
    pub fn mark_encoded(&mut self, token: FrameToken) -> Result<(), Error> {
        let c = self
            .candidate
            .as_mut()
            .filter(|c| c.token == token)
            .ok_or(Error::WrongToken)?;
        if c.encoded {
            return Err(Error::Busy);
        }
        c.encoded = true;
        Ok(())
    }
    pub fn validate_commit(&self, token: FrameToken) -> Result<(), Error> {
        let c = self
            .candidate
            .filter(|c| c.token == token)
            .ok_or(Error::WrongToken)?;
        if !c.encoded {
            return Err(Error::NotEncoded);
        }
        Ok(())
    }
    pub fn commit_presented(&mut self, token: FrameToken) -> Result<(), Error> {
        self.validate_commit(token)?;
        let c = self.candidate.take().expect("validated token");
        self.front_camera = Some(c.vp);
        self.front_frame = token.frame();
        self.front_records.copy_from_slice(&self.candidate_records);
        for (i, pair) in self.pairs.iter().enumerate() {
            self.front_pairs[i] = pair.current;
        }
        Ok(())
    }
    pub fn discard(&mut self, token: FrameToken) -> Result<(), Error> {
        if self.candidate.is_none_or(|c| c.token != token) {
            return Err(Error::WrongToken);
        }
        self.candidate = None;
        Ok(())
    }
}
fn same_geometry(a: &BoardGpu, b: &BoardGpu) -> bool {
    a.center == b.center
        && a.right_width == b.right_width
        && a.up_height == b.up_height
        && a.codes[0] == b.codes[0]
}
fn entry(
    binding: u32,
    uniform: bool,
    visibility: wgpu::ShaderStages,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: if uniform {
                wgpu::BufferBindingType::Uniform
            } else {
                wgpu::BufferBindingType::Storage { read_only: true }
            },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}
fn binding(binding: u32, buffer: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: buffer.as_entire_binding(),
    }
}
pub fn shader_source() -> Result<String, Error> {
    let source = crate::recipe_boards::shader_source(false, false, false, false);
    let old = "@vertex fn vertex(@builtin(vertex_index)vertex:u32,@builtin(instance_index)instance:u32)->VertexOut{";
    if !source.contains(old) || !source.contains("let item=boards[instance];") {
        return Err(Error::Shader);
    }
    let source = source
        .replacen(old, "fn board_pose(vertex:u32,item:Board)->VertexOut{", 1)
        .replacen("let item=boards[instance];", "", 1);
    Ok(format!(
        "{source}\n{}",
        include_str!("recipe_board_motion.wgsl")
    ))
}
