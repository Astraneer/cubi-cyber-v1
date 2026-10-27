//! Optional single-sample temporal reconstruction. Resolver validation is
//! separate from the actual presented-object-motion and product-mode gates.
use crate::terrain_gpu::Camera;
use bytemuck::{Pod, Zeroable};

pub const JITTER_PHASES: usize = 8;
/// Motion is previous UNJITTERED geometry UV minus current UNJITTERED geometry UV, followed by
/// predicted previous reverse depth and a validity bit. Half-float depth error
/// is explicitly included in reprojection rejection, rather than ignored.
pub const MOTION_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// [family/part namespace, checked stable token low, token high, geometry key].
/// Tokens preserve full identity through the source cache; they are not hashes.
pub const SURFACE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba32Uint;
pub const HDR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R32Float;
/// Motion8+identity16, two{color8+depth4+identity16} histories, stable output8.
/// A bounded8B/pixel GPU copy keeps the output binding stable for Post.
pub const BYTES_PER_PIXEL: u64 = 88;
pub const OWNED_TEXTURE_BYTES_PER_PIXEL: u64 = 64;
pub const MOTION_TEXTURE_BYTES_PER_PIXEL: u64 = 24;

#[derive(Clone, Copy, Debug)]
pub struct TaaConfig {
    pub history_samples: u32,
    pub max_pixels: u64,
    pub memory_budget_bytes: u64,
    pub depth_absolute_epsilon: f32,
    pub depth_relative_epsilon: f32,
    pub max_motion_pixels: f32,
    pub statistics: bool,
}
impl Default for TaaConfig {
    fn default() -> Self {
        Self {
            history_samples: 8,
            max_pixels: 3840 * 2160,
            memory_budget_bytes: 256 * 1024 * 1024,
            depth_absolute_epsilon: 0.000001,
            depth_relative_epsilon: 1. / 1024.,
            max_motion_pixels: 128.,
            statistics: false,
        }
    }
}

#[derive(Clone, Copy)]
pub struct TaaInputs<'a> {
    pub hdr: &'a wgpu::TextureView,
    pub depth: &'a wgpu::TextureView,
    pub motion: &'a wgpu::TextureView,
    pub surface: &'a wgpu::TextureView,
    pub extent: [u32; 2],
    pub samples: u32,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TaaWork {
    pub passes: u32,
    pub pixels: u64,
    pub texture_loads_upper_bound: u64,
    pub history_copy_bytes: u64,
    pub upload_bytes: u64,
    pub counter_clear_bytes: u64,
    pub history_available: bool,
}
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Parameters {
    size: [u32; 4],
    jitter_blend: [f32; 4],
    depth_limits: [f32; 4],
    flags: [u32; 4],
}
const PARAMETER_BYTES: u64 = std::mem::size_of::<Parameters>() as u64;
struct HistoryImages {
    color: wgpu::Texture,
    color_view: wgpu::TextureView,
    _depth: wgpu::Texture,
    depth_view: wgpu::TextureView,
    _surface: wgpu::Texture,
    surface_view: wgpu::TextureView,
}
struct Targets {
    output: wgpu::Texture,
    output_view: wgpu::TextureView,
    history: [HistoryImages; 2],
}
pub struct TaaGpu {
    config: TaaConfig,
    extent: [u32; 2],
    history: PresentedHistory,
    targets: Targets,
    parameters: wgpu::Buffer,
    statistics: Option<wgpu::Buffer>,
    layout: wgpu::BindGroupLayout,
    binds: [wgpu::BindGroup; 2],
    pipeline: wgpu::RenderPipeline,
    timer: crate::gpu_time::GpuPassTimer<2>,
    total: crate::gpu_time::GpuGraphTimer,
    constant_bytes: u64,
    output_frame: Option<u64>,
    needs_output_restore: bool,
    output_restore_pending: Option<OutputRestoreToken>,
    output_restore_serial: u64,
}
impl TaaGpu {
    /// None returns before any device call: defaultMSAA4 has no TAA resources,
    /// shader/pipeline preparation or timestamp queries.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        config: Option<TaaConfig>,
        inputs: TaaInputs<'_>,
    ) -> Result<Option<Self>, Error> {
        let Some(config) = config else {
            return Ok(None);
        };
        let constant_bytes = config
            .required_constant_bytes(device.features().contains(wgpu::Features::TIMESTAMP_QUERY));
        validate_allocation(device, config, inputs, constant_bytes)?;
        let parameters = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("TAA temporal parameters"),
            size: PARAMETER_BYTES,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let statistics = config.statistics.then(|| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("actual TAA fragments loads rejection counters"),
                size: 16,
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_DST
                    | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            })
        });
        let f = wgpu::ShaderStages::FRAGMENT;
        let mut entries = vec![wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: f,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }];
        for binding in 1..=7 {
            entries.push(wgpu::BindGroupLayoutEntry {
                binding,
                visibility: f,
                ty: wgpu::BindingType::Texture {
                    sample_type: if binding == 2 {
                        wgpu::TextureSampleType::Depth
                    } else if binding == 4 || binding == 7 {
                        wgpu::TextureSampleType::Uint
                    } else {
                        wgpu::TextureSampleType::Float { filterable: false }
                    },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            });
        }
        if config.statistics {
            entries.push(wgpu::BindGroupLayoutEntry {
                binding: 8,
                visibility: f,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            });
        }
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("single-sample TAA immutable inputs"),
            entries: &entries,
        });
        let targets = make_targets(device, inputs.extent);
        let binds = make_binds(
            device,
            &layout,
            &parameters,
            statistics.as_ref(),
            &targets,
            inputs,
        );
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("bounded HDR TAA resolve"),
            source: wgpu::ShaderSource::Wgsl(shader_source(config.statistics).into()),
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("TAA resolve layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let target = |format| {
            Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })
        };
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("HDR resolve and exact identity depth history"),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fragment"),
                compilation_options: Default::default(),
                targets: &[
                    target(HDR_FORMAT),
                    target(DEPTH_FORMAT),
                    target(SURFACE_FORMAT),
                ],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview: None,
            cache: None,
        });
        Ok(Some(Self {
            config,
            extent: inputs.extent,
            history: PresentedHistory::default(),
            targets,
            parameters,
            statistics,
            layout,
            binds,
            pipeline,
            timer: crate::gpu_time::GpuPassTimer::new(device, queue, "TAA resolve pass"),
            total: crate::gpu_time::GpuGraphTimer::new(
                device,
                queue,
                "whole TAA including history copies",
            ),
            constant_bytes,
            output_frame: None,
            needs_output_restore: false,
            output_restore_pending: None,
            output_restore_serial: 0,
        }))
    }
    pub fn begin_frame(&mut self, state: TaaFrame) -> Result<FrameToken, Error> {
        if self.output_restore_pending.is_some() {
            return Err(Error::Busy);
        }
        self.history.begin(state)
    }
    pub fn next_jitter(&self) -> [f32; 2] {
        self.history.next_jitter()
    }
    pub fn needs_refinement(&self, scene_revision: u64) -> bool {
        self.history
            .needs_refinement(scene_revision, self.config.history_samples)
    }
    pub fn presented(&self) -> Option<PresentedFrame> {
        self.history.front()
    }
    pub fn pending(&self) -> Option<FrameToken> {
        self.history.pending()
    }
    /// Stable loading binding. Current-frame Post may consume it only after
    /// encode; cached presentation must additionally check presented_output_ready.
    pub fn output_view(&self) -> &wgpu::TextureView {
        &self.targets.output_view
    }
    pub fn presented_output_ready(&self) -> bool {
        !self.needs_output_restore
            && self
                .history
                .front()
                .is_some_and(|front| self.output_frame == Some(front.state.frame))
    }
    pub fn statistics_buffer(&self) -> Option<&wgpu::Buffer> {
        self.statistics.as_ref()
    }
    /// Actual owned textures/buffers; the supplied motion/identity images are
    /// charged by their source owner, so global accounting must not add twice.
    pub fn allocation_bytes(&self) -> u64 {
        u64::from(self.extent[0]) * u64::from(self.extent[1]) * OWNED_TEXTURE_BYTES_PER_PIXEL
            + self.constant_bytes
    }
    pub fn feature_bytes(&self) -> u64 {
        self.allocation_bytes()
            + u64::from(self.extent[0]) * u64::from(self.extent[1]) * MOTION_TEXTURE_BYTES_PER_PIXEL
    }
    pub fn pipeline_compilations(&self) -> u32 {
        1
    }
    pub fn graph(&self) -> crate::gpu_time::PassGraph<2> {
        self.timer.graph()
    }
    pub fn timing_source(&self) -> Option<crate::gpu_time::TimingSource<'_>> {
        self.total
            .timestamp_buffer()
            .map(|(buffer, period)| crate::gpu_time::TimingSource {
                buffer,
                period,
                kinds: [1; 32],
                count: 1,
            })
    }
    pub fn clear_timing(&self) {
        self.timer.begin();
        self.total.begin_frame();
    }
    /// CPU-only global graph preflight. This does not reserve credit, write a
    /// uniform, advance history, or record commands.
    pub fn pending_work(&self, token: FrameToken) -> Result<TaaWork, Error> {
        self.history.frame(token)?;
        if self
            .history
            .pending
            .is_some_and(|candidate| candidate.encoded)
        {
            return Err(Error::Busy);
        }
        let pixels = u64::from(self.extent[0]) * u64::from(self.extent[1]);
        Ok(TaaWork {
            passes: 1,
            pixels,
            texture_loads_upper_bound: pixels * 28,
            history_copy_bytes: pixels * 8,
            upload_bytes: PARAMETER_BYTES,
            counter_clear_bytes: u64::from(self.statistics.is_some()) * 16,
            history_available: self.history.history_available(token)?,
        })
    }
    /// Validate all CPU state/credit before any queue write or encoder command.
    pub fn encode(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        token: FrameToken,
        budget: &mut crate::terrain_gpu::UploadBudget,
    ) -> Result<TaaWork, Error> {
        let work = self.pending_work(token)?;
        let state = self.history.frame(token)?;
        if budget.remaining_bytes < PARAMETER_BYTES {
            return Err(Error::UploadBudget);
        }
        let available = work.history_available;
        let age = self
            .history
            .front()
            .map_or(0, |p| p.samples)
            .min(self.config.history_samples - 1);
        let blend = if available {
            age as f32 / (age + 1) as f32
        } else {
            0.
        };
        let params = Parameters {
            size: [self.extent[0], self.extent[1], age, u32::from(available)],
            jitter_blend: [
                state.jitter_pixels[0],
                state.jitter_pixels[1],
                blend,
                self.config.max_motion_pixels,
            ],
            depth_limits: [
                self.config.depth_absolute_epsilon,
                self.config.depth_relative_epsilon,
                0.,
                0.,
            ],
            flags: [0; 4],
        };
        queue.write_buffer(&self.parameters, 0, bytemuck::bytes_of(&params));
        budget.remaining_bytes -= PARAMETER_BYTES;
        self.timer.begin();
        self.total.begin_frame();
        self.total.start(encoder);
        if let Some(stats) = &self.statistics {
            encoder.clear_buffer(stats, 0, None);
        }
        let back = self.history.back_index();
        let attachment = |view| {
            Some(wgpu::RenderPassColorAttachment {
                view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })
        };
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("TAA HDR reprojection and exact history metadata"),
                color_attachments: &[
                    attachment(&self.targets.output_view),
                    attachment(&self.targets.history[back].depth_view),
                    attachment(&self.targets.history[back].surface_view),
                ],
                depth_stencil_attachment: None,
                timestamp_writes: self.timer.render(1),
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.binds[1 - back], &[]);
            pass.draw(0..3, 0..1);
        }
        copy_color(
            encoder,
            &self.targets.output,
            &self.targets.history[back].color,
            self.extent,
        );
        self.timer.finish(encoder);
        self.total.finish(encoder);
        self.history.mark_encoded(token)?;
        self.output_frame = Some(state.frame);
        self.needs_output_restore = true;
        Ok(work)
    }
    pub fn validate_commit(&self, token: FrameToken) -> Result<(), Error> {
        if self.output_restore_pending.is_some() {
            return Err(Error::Busy);
        }
        self.history
            .validate_commit(token, self.config.history_samples)
    }
    pub fn commit_presented(&mut self, token: FrameToken) -> Result<PresentedFrame, Error> {
        self.validate_commit(token)?;
        let frame = self
            .history
            .commit_presented(token, self.config.history_samples)?;
        self.needs_output_restore = false;
        Ok(frame)
    }
    pub fn discard(&mut self, token: FrameToken) -> Result<(), Error> {
        self.needs_output_restore |= self.history.discard(token)?;
        Ok(())
    }
    /// A cancelled candidate cannot leak through UI-only cached presentation.
    /// Restore the old presented HDR without advancing its camera/history/pose.
    pub fn restore_presented_output(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<u64, Error> {
        if self.history.pending().is_some() || self.output_restore_pending.is_some() {
            return Err(Error::Busy);
        }
        let front = self.history.front().ok_or(Error::NoPresentedFrame)?;
        if !self.needs_output_restore {
            return Ok(0);
        }
        let serial = self
            .output_restore_serial
            .checked_add(1)
            .ok_or(Error::InvalidFrame)?;
        let token = OutputRestoreToken {
            frame: front.state.frame,
            serial,
        };
        self.timer.begin();
        self.total.begin_frame();
        self.total.start(encoder);
        copy_color(
            encoder,
            &self.targets.history[front.history_index].color,
            &self.targets.output,
            self.extent,
        );
        self.total.finish(encoder);
        self.output_restore_serial = serial;
        self.output_restore_pending = Some(token);
        Ok(u64::from(self.extent[0]) * u64::from(self.extent[1]) * 8)
    }
    /// Encoding a copy does not prove queue submission. The platform carries
    /// this exact token through its submit/abandon boundary, including UI-only
    /// encoders with no new scene token.
    pub fn output_restore_token(&self) -> Option<OutputRestoreToken> {
        self.output_restore_pending
    }
    pub fn mark_output_restore_submitted(
        &mut self,
        token: OutputRestoreToken,
    ) -> Result<(), Error> {
        if self.output_restore_pending != Some(token)
            || self
                .history
                .front()
                .is_none_or(|front| front.state.frame != token.frame)
        {
            return Err(Error::WrongToken);
        }
        self.output_frame = Some(token.frame);
        self.needs_output_restore = false;
        self.output_restore_pending = None;
        Ok(())
    }
    pub fn discard_output_restore(&mut self, token: OutputRestoreToken) -> Result<(), Error> {
        if self.output_restore_pending != Some(token) {
            return Err(Error::WrongToken);
        }
        self.output_restore_pending = None;
        self.needs_output_restore = true;
        self.clear_timing();
        Ok(())
    }
    /// Rebind/resize only resources. The single pipeline remains prepared.
    pub fn resize_inputs(
        &mut self,
        device: &wgpu::Device,
        inputs: TaaInputs<'_>,
    ) -> Result<(), Error> {
        if self.history.pending().is_some() || self.output_restore_pending.is_some() {
            return Err(Error::Busy);
        }
        validate_allocation(device, self.config, inputs, self.constant_bytes)?;
        let targets = make_targets(device, inputs.extent);
        let binds = make_binds(
            device,
            &self.layout,
            &self.parameters,
            self.statistics.as_ref(),
            &targets,
            inputs,
        );
        self.targets = targets;
        self.binds = binds;
        self.extent = inputs.extent;
        self.history.invalidate()?;
        self.output_frame = None;
        self.needs_output_restore = false;
        Ok(())
    }
}
fn validate_allocation(
    device: &wgpu::Device,
    config: TaaConfig,
    inputs: TaaInputs<'_>,
    constant: u64,
) -> Result<(), Error> {
    if inputs.samples != 1
        || inputs
            .extent
            .iter()
            .any(|v| *v > device.limits().max_texture_dimension_2d)
    {
        return Err(Error::InvalidConfig);
    }
    let texture_bytes = config.allocation_bytes(inputs.extent)?;
    if texture_bytes
        .checked_add(constant)
        .is_none_or(|bytes| bytes > config.memory_budget_bytes)
    {
        return Err(Error::MemoryBudget);
    }
    Ok(())
}
fn make_targets(device: &wgpu::Device, extent: [u32; 2]) -> Targets {
    let texture = |label, format, copy| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: extent[0],
                height: extent[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | if copy {
                    wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST
                } else {
                    wgpu::TextureUsages::empty()
                },
            view_formats: &[],
        })
    };
    let output = texture("stable temporal HDR output", HDR_FORMAT, true);
    let output_view = output.create_view(&Default::default());
    let history = std::array::from_fn(|_| {
        let color = texture("presented HDR history", HDR_FORMAT, true);
        let color_view = color.create_view(&Default::default());
        let depth = texture("presented reverse depth history", DEPTH_FORMAT, false);
        let depth_view = depth.create_view(&Default::default());
        let surface = texture("presented exact surface identity", SURFACE_FORMAT, false);
        let surface_view = surface.create_view(&Default::default());
        HistoryImages {
            color,
            color_view,
            _depth: depth,
            depth_view,
            _surface: surface,
            surface_view,
        }
    });
    Targets {
        output,
        output_view,
        history,
    }
}
fn make_binds(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    parameters: &wgpu::Buffer,
    statistics: Option<&wgpu::Buffer>,
    targets: &Targets,
    inputs: TaaInputs<'_>,
) -> [wgpu::BindGroup; 2] {
    std::array::from_fn(|i| {
        let mut entries = vec![wgpu::BindGroupEntry {
            binding: 0,
            resource: parameters.as_entire_binding(),
        }];
        for (binding, view) in [
            (1, inputs.hdr),
            (2, inputs.depth),
            (3, inputs.motion),
            (4, inputs.surface),
            (5, &targets.history[i].color_view),
            (6, &targets.history[i].depth_view),
            (7, &targets.history[i].surface_view),
        ] {
            entries.push(wgpu::BindGroupEntry {
                binding,
                resource: wgpu::BindingResource::TextureView(view),
            });
        }
        if let Some(stats) = statistics {
            entries.push(wgpu::BindGroupEntry {
                binding: 8,
                resource: stats.as_entire_binding(),
            });
        }
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("immutable TAA history front inputs"),
            layout,
            entries: &entries,
        })
    })
}
fn copy_color(
    encoder: &mut wgpu::CommandEncoder,
    source: &wgpu::Texture,
    target: &wgpu::Texture,
    extent: [u32; 2],
) {
    let texture = |texture| wgpu::TexelCopyTextureInfo {
        texture,
        mip_level: 0,
        origin: wgpu::Origin3d::ZERO,
        aspect: wgpu::TextureAspect::All,
    };
    encoder.copy_texture_to_texture(
        texture(source),
        texture(target),
        wgpu::Extent3d {
            width: extent[0],
            height: extent[1],
            depth_or_array_layers: 1,
        },
    );
}
pub fn shader_source(statistics: bool) -> String {
    let stats = if statistics {
        r#"struct TaaStats{fragments:atomic<u32>,loads:atomic<u32>,accepted:atomic<u32>,rejected:atomic<u32>};@group(0)@binding(8)var<storage,read_write>stats:TaaStats;fn record_fragment(){atomicAdd(&stats.fragments,1u);}fn record_loads(n:u32){atomicAdd(&stats.loads,n);}fn record_history_accept(){atomicAdd(&stats.accepted,1u);}fn record_identity_reject(){atomicAdd(&stats.rejected,1u);}fn record_depth_reject(){atomicAdd(&stats.rejected,1u);}"#
    } else {
        "fn record_fragment(){}fn record_loads(n:u32){}fn record_history_accept(){}fn record_identity_reject(){}fn record_depth_reject(){}"
    };
    include_str!("taa.wgsl").replace("// STATISTICS", stats)
}
impl TaaConfig {
    pub fn required_constant_bytes(self, timestamp_queries: bool) -> u64 {
        PARAMETER_BYTES + u64::from(self.statistics) * 16 + u64::from(timestamp_queries) * 512
    }
    pub fn allocation_bytes(self, extent: [u32; 2]) -> Result<u64, Error> {
        if !(2..=32).contains(&self.history_samples)
            || extent.contains(&0)
            || self.max_pixels == 0
            || !self.depth_absolute_epsilon.is_finite()
            || self.depth_absolute_epsilon < 0.
            || !self.depth_relative_epsilon.is_finite()
            || self.depth_relative_epsilon < 1. / 2048.
            || !self.max_motion_pixels.is_finite()
            || self.max_motion_pixels <= 0.
        {
            return Err(Error::InvalidConfig);
        }
        let pixels = u64::from(extent[0]) * u64::from(extent[1]);
        let bytes = pixels
            .checked_mul(BYTES_PER_PIXEL)
            .ok_or(Error::MemoryBudget)?;
        if pixels > self.max_pixels || bytes > self.memory_budget_bytes {
            return Err(Error::MemoryBudget);
        }
        Ok(bytes)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidConfig,
    InvalidFrame,
    MemoryBudget,
    Busy,
    WrongToken,
    NotEncoded,
    NoPresentedFrame,
    UploadBudget,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResetReasons(pub u32);
impl ResetReasons {
    pub const NONE: Self = Self(0);
    pub const GEOMETRY_EDIT: Self = Self(1);
    pub const SOURCE_REVOKED: Self = Self(2);
    pub const CAMERA_CUT: Self = Self(4);
    pub const SPACE_CHANGE: Self = Self(8);
    pub const RESIZE: Self = Self(16);
    pub const DISCARDED_FRAME: Self = Self(32);
    pub const EXPLICIT: Self = Self(64);
    const ALL: u32 = 127;
}
impl std::ops::BitOr for ResetReasons {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TaaFrame {
    pub frame: u64,
    pub space: u32,
    /// Semantic scene changes, excluding the TAA jitter itself. Drives bounded
    /// settling; a moving object does not revoke valid reprojected history.
    pub scene_revision: u64,
    pub geometry_epoch: u64,
    pub source_epoch: u64,
    /// Positive right/down in RENDER pixels, before any output-scale conversion.
    pub jitter_pixels: [f32; 2],
    pub reset: ResetReasons,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameToken {
    frame: u64,
    serial: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputRestoreToken {
    frame: u64,
    serial: u64,
}
impl OutputRestoreToken {
    pub fn frame(self) -> u64 {
        self.frame
    }
    pub fn serial(self) -> u64 {
        self.serial
    }
}
impl FrameToken {
    pub fn frame(self) -> u64 {
        self.frame
    }
    pub fn serial(self) -> u64 {
        self.serial
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PresentedFrame {
    pub state: TaaFrame,
    pub samples: u32,
    pub phase: u32,
    pub history_index: usize,
}
#[derive(Clone, Copy, Debug)]
struct PendingFrame {
    token: FrameToken,
    state: TaaFrame,
    encoded: bool,
    phase: u32,
}
/// One candidate transaction. The source-pose owner uses the SAME token and
/// commits only after GPU completion plus actual presentation. Encoding alone
/// never changes the presented camera, object poses or history front.
#[derive(Default)]
pub struct PresentedHistory {
    front: Option<PresentedFrame>,
    pending: Option<PendingFrame>,
    serial: u64,
}
impl PresentedHistory {
    pub fn front(&self) -> Option<PresentedFrame> {
        self.front
    }
    pub fn pending(&self) -> Option<FrameToken> {
        self.pending.map(|p| p.token)
    }
    pub fn begin(&mut self, state: TaaFrame) -> Result<FrameToken, Error> {
        if self.pending.is_some() {
            return Err(Error::Busy);
        }
        if state.reset.0 & !ResetReasons::ALL != 0
            || state.frame == 0
            || state
                .jitter_pixels
                .iter()
                .any(|v| !v.is_finite() || v.abs() > 0.5)
            || self.front.is_some_and(|p| state.frame <= p.state.frame)
        {
            return Err(Error::InvalidFrame);
        }
        let serial = self.serial.checked_add(1).ok_or(Error::InvalidFrame)?;
        let token = FrameToken {
            frame: state.frame,
            serial,
        };
        let phase = self
            .front
            .map_or(0, |p| (p.phase + 1) % JITTER_PHASES as u32);
        self.serial = serial;
        self.pending = Some(PendingFrame {
            token,
            state,
            encoded: false,
            phase,
        });
        Ok(token)
    }
    pub fn mark_encoded(&mut self, token: FrameToken) -> Result<(), Error> {
        let p = self
            .pending
            .as_mut()
            .filter(|p| p.token == token)
            .ok_or(Error::WrongToken)?;
        if p.encoded {
            return Err(Error::Busy);
        }
        p.encoded = true;
        Ok(())
    }
    pub fn history_available(&self, token: FrameToken) -> Result<bool, Error> {
        let p = self
            .pending
            .filter(|p| p.token == token)
            .ok_or(Error::WrongToken)?;
        Ok(self.front.is_some_and(|old| {
            p.state.reset == ResetReasons::NONE
                && old.state.space == p.state.space
                && old.state.geometry_epoch == p.state.geometry_epoch
                && old.state.source_epoch == p.state.source_epoch
        }))
    }
    pub fn frame(&self, token: FrameToken) -> Result<TaaFrame, Error> {
        self.pending
            .filter(|p| p.token == token)
            .map(|p| p.state)
            .ok_or(Error::WrongToken)
    }
    pub fn back_index(&self) -> usize {
        self.front.map_or(0, |p| 1 - p.history_index)
    }
    pub fn needs_refinement(&self, scene_revision: u64, max_samples: u32) -> bool {
        self.front
            .is_none_or(|p| p.state.scene_revision != scene_revision || p.samples < max_samples)
    }
    pub fn commit_presented(
        &mut self,
        token: FrameToken,
        max_samples: u32,
    ) -> Result<PresentedFrame, Error> {
        self.validate_commit(token, max_samples)?;
        let p = self.pending.ok_or(Error::WrongToken)?;
        let available = self.history_available(token)?;
        let samples = if available {
            self.front
                .filter(|old| old.state.scene_revision == p.state.scene_revision)
                .map_or(1, |old| old.samples.saturating_add(1).min(max_samples))
        } else {
            1
        };
        let front = PresentedFrame {
            state: p.state,
            samples,
            phase: p.phase,
            history_index: self.back_index(),
        };
        self.front = Some(front);
        self.pending = None;
        Ok(front)
    }
    pub fn validate_commit(&self, token: FrameToken, max_samples: u32) -> Result<(), Error> {
        if !(2..=32).contains(&max_samples) {
            return Err(Error::InvalidConfig);
        }
        let p = self
            .pending
            .filter(|p| p.token == token)
            .ok_or(Error::WrongToken)?;
        if !p.encoded {
            return Err(Error::NotEncoded);
        }
        Ok(())
    }
    /// Return whether encoded output may need restoration from the unchanged
    /// presented front before a cached presentation is allowed.
    pub fn discard(&mut self, token: FrameToken) -> Result<bool, Error> {
        let p = self
            .pending
            .filter(|p| p.token == token)
            .ok_or(Error::WrongToken)?;
        self.pending = None;
        Ok(p.encoded)
    }
    pub fn invalidate(&mut self) -> Result<(), Error> {
        if self.pending.is_some() {
            return Err(Error::Busy);
        }
        self.front = None;
        Ok(())
    }
    pub fn next_jitter(&self) -> [f32; 2] {
        jitter(self.front.map_or(0, |p| p.phase as usize + 1))
    }
}

/// Original centered8-phase Halton(2,3) sequence; no SDK table or RNG state.
pub fn jitter(phase: usize) -> [f32; 2] {
    const OFFSETS: [[f32; 2]; 8] = [
        [7. / 128., -1. / 6.],
        [-25. / 128., 1. / 6.],
        [39. / 128., -7. / 18.],
        [-41. / 128., -1. / 18.],
        [23. / 128., 5. / 18.],
        [-9. / 128., -5. / 18.],
        [55. / 128., 1. / 18.],
        [-49. / 128., 7. / 18.],
    ];
    OFFSETS[phase % JITTER_PHASES]
}
pub fn jitter_camera(camera: Camera, extent: [u32; 2], offset: [f32; 2]) -> Result<Camera, Error> {
    if extent.contains(&0) || offset.iter().any(|v| !v.is_finite() || v.abs() > 0.5) {
        return Err(Error::InvalidFrame);
    }
    let mut matrix = camera.view_projection;
    if matrix.iter().flatten().any(|v| !v.is_finite()) {
        return Err(Error::InvalidFrame);
    }
    for column in &mut matrix {
        column[0] += 2. * offset[0] / extent[0] as f32 * column[3];
        column[1] -= 2. * offset[1] / extent[1] as f32 * column[3];
    }
    Ok(Camera {
        view_projection: matrix,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_resolve_shaders_validate_with_and_without_counters() {
        for statistics in [false, true] {
            let source = shader_source(statistics);
            let module = wgpu::naga::front::wgsl::parse_str(&source)
                .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&source)));
            wgpu::naga::valid::Validator::new(
                wgpu::naga::valid::ValidationFlags::all(),
                wgpu::naga::valid::Capabilities::all(),
            )
            .validate(&module)
            .unwrap();
        }
    }
    fn frame(n: u64) -> TaaFrame {
        TaaFrame {
            frame: n,
            space: 0,
            scene_revision: 1,
            geometry_epoch: 1,
            source_epoch: 1,
            jitter_pixels: jitter(n as usize - 1),
            reset: ResetReasons::NONE,
        }
    }
    #[test]
    fn candidate_encoding_never_publishes_camera_or_history() {
        let mut h = PresentedHistory::default();
        let t = h.begin(frame(1)).unwrap();
        assert_eq!(h.front().map(|p| p.state.frame), None);
        assert_eq!(h.commit_presented(t, 8), Err(Error::NotEncoded));
        h.mark_encoded(t).unwrap();
        assert!(h.front().is_none());
        let other = FrameToken {
            frame: 1,
            serial: t.serial + 1,
        };
        assert_eq!(h.discard(other), Err(Error::WrongToken));
        assert_eq!(h.commit_presented(other, 8), Err(Error::WrongToken));
        h.commit_presented(t, 8).unwrap();
        let next = h.begin(frame(2)).unwrap();
        assert!(h.history_available(next).unwrap());
        h.mark_encoded(next).unwrap();
        assert!(h.discard(next).unwrap());
        assert_eq!(h.front().unwrap().state.frame, 1);
        assert_eq!(h.next_jitter(), jitter(1));
    }
    #[test]
    fn edit_space_provenance_and_camera_cut_deny_history_exactly() {
        for mode in 0..4 {
            let mut h = PresentedHistory::default();
            let t = h.begin(frame(1)).unwrap();
            h.mark_encoded(t).unwrap();
            h.commit_presented(t, 8).unwrap();
            let mut current = frame(2);
            match mode {
                0 => current.geometry_epoch += 1,
                1 => current.source_epoch += 1,
                2 => current.space = 7,
                _ => current.reset = ResetReasons::CAMERA_CUT,
            };
            let t = h.begin(current).unwrap();
            assert!(!h.history_available(t).unwrap());
            h.mark_encoded(t).unwrap();
            assert_eq!(h.commit_presented(t, 8).unwrap().samples, 1);
        }
    }
    #[test]
    fn single_pending_and_checked_serial_refuse_without_front_mutation() {
        let mut h = PresentedHistory::default();
        let t = h.begin(frame(1)).unwrap();
        assert_eq!(h.begin(frame(2)), Err(Error::Busy));
        h.discard(t).unwrap();
        h.serial = u64::MAX;
        assert_eq!(h.begin(frame(2)), Err(Error::InvalidFrame));
        assert!(h.pending().is_none());
        assert!(h.front().is_none());
    }
    #[test]
    fn static_history_settles_in_bounded_phases_and_jitter_is_zero_mean() {
        let mut h = PresentedHistory::default();
        for n in 1..=8 {
            assert!(h.needs_refinement(1, 8));
            let t = h.begin(frame(n)).unwrap();
            h.mark_encoded(t).unwrap();
            h.commit_presented(t, 8).unwrap();
        }
        assert!(!h.needs_refinement(1, 8));
        assert!(h.needs_refinement(2, 8));
        let sum = (0..8)
            .map(jitter)
            .fold([0.; 2], |a, b| [a[0] + b[0], a[1] + b[1]]);
        assert!(sum.iter().all(|v| v.abs() < 1e-6));
    }
    #[test]
    fn memory_is_explicit_and_camera_jitter_keeps_depth_unchanged() {
        let c = TaaConfig::default();
        assert_eq!(c.allocation_bytes([1280, 720]).unwrap(), 81_100_800);
        assert_eq!(c.allocation_bytes([3840, 2160]), Err(Error::MemoryBudget));
        let camera = Camera {
            view_projection: [
                [1., 0., 0., 0.],
                [0., 1., 0., 0.],
                [0., 0., 1., 0.],
                [0., 0., 0., 1.],
            ],
        };
        let result = jitter_camera(camera, [100, 50], [0.25, -0.25]).unwrap();
        assert_eq!(result.view_projection[3], [0.005, 0.01, 0., 1.]);
        assert_eq!(
            jitter_camera(camera, [0, 1], [0.; 2]),
            Err(Error::InvalidFrame)
        );
    }
}
