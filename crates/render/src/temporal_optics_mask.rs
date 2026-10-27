//! Optional exact optical coverage invalidation. It reuses the real water
//! geometry and cloud transport composite, without repeating optical shading.
use crate::{
    cloud_gpu::{CloudGpu, CloudTemporalSource},
    gpu_time::{GpuPassTimer, PassGraph, TimingSource},
    water_gpu::{WaterGpu, WaterTemporalSource},
};
const COMMON: &str = include_str!("temporal_optics_mask.wgsl");
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidTargets,
    InvalidSource,
    SourcesChanged,
    NotPrepared,
    WorkBudget,
    Shader,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Work {
    pub passes: u32,
    pub draws: u32,
    /// Nominal viewport-pixel quota, not a measured fragment invocation count.
    pub pixels: u64,
    pub texture_loads_bound: u64,
    pub vertex_invocations_bound: u64,
}
pub struct TemporalOpticsMask {
    water: Option<wgpu::RenderPipeline>,
    cloud: Option<wgpu::RenderPipeline>,
    motion: wgpu::TextureView,
    surface: wgpu::TextureView,
    extent: [u32; 2],
    timer: GpuPassTimer<2>,
}
fn targets(motion: &wgpu::TextureView, surface: &wgpu::TextureView) -> Result<[u32; 2], Error> {
    let m = motion.texture();
    let s = surface.texture();
    if m.format() != wgpu::TextureFormat::Rgba16Float
        || s.format() != wgpu::TextureFormat::Rgba32Uint
        || m.sample_count() != 1
        || s.sample_count() != 1
        || m.width() == 0
        || m.height() == 0
        || m.size() != s.size()
        || m.dimension() != wgpu::TextureDimension::D2
        || s.dimension() != wgpu::TextureDimension::D2
        || m.depth_or_array_layers() != 1
        || m.mip_level_count() != 1
        || s.mip_level_count() != 1
        || !m.usage().contains(wgpu::TextureUsages::RENDER_ATTACHMENT)
        || !s.usage().contains(wgpu::TextureUsages::RENDER_ATTACHMENT)
    {
        return Err(Error::InvalidTargets);
    }
    Ok([m.width(), m.height()])
}
fn validate(
    extent: [u32; 2],
    water: Option<&WaterTemporalSource<'_>>,
    cloud: Option<&CloudTemporalSource<'_>>,
) -> Result<(), Error> {
    if water.is_some_and(|w| {
        w.extent != extent
            || w.samples != 1
            || w.depth.texture().sample_count() != 1
            || w.depth.texture().format() != wgpu::TextureFormat::Depth32Float
    }) || cloud.is_some_and(|c| c.extent != extent || c.samples != 1)
    {
        return Err(Error::InvalidSource);
    }
    Ok(())
}
pub fn water_shader_source() -> String {
    format!(
        "{}\n{COMMON}\n@fragment fn water_mask(input:Out)->OpticsMaskOutput{{return invalid_optical_history();}}",
        crate::water_gpu::temporal_vertex_source(1)
    )
}
pub fn cloud_shader_source() -> Result<String, Error> {
    let source = crate::cloud_gpu::temporal_composite_source(1);
    let header = "@fragment fn fragment(";
    let output = "->@location(0)vec4<f32>";
    let no_cloud = "if sum<0.00001{return source;}";
    let composed = "cloud/=sum;return vec4(cloud.rgb+source.rgb*cloud.a,source.a);";
    if ![header, output, no_cloud, composed]
        .iter()
        .all(|part| source.contains(part))
    {
        return Err(Error::Shader);
    }
    Ok(format!(
        "{COMMON}\n{}",
        source
            .replace(header, "@fragment fn cloud_mask(")
            .replace(output, "->OpticsMaskOutput")
            .replace(no_cloud, "if sum<0.00001{discard;}")
            .replace(
                composed,
                "cloud/=sum;if cloud.a>=1.0{discard;}return invalid_optical_history();"
            )
    ))
}
fn pipeline(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    source: &str,
    water: bool,
) -> wgpu::RenderPipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("exact current optical history invalidity"),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("borrowed optical source bindings"),
        bind_group_layouts: &[layout],
        push_constant_ranges: &[],
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(if water {
            "water coverage invalidates opaque history"
        } else {
            "actual cloud transmittance invalidates opaque history"
        }),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vertex"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some(if water { "water_mask" } else { "cloud_mask" }),
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
            topology: if water {
                wgpu::PrimitiveTopology::TriangleStrip
            } else {
                wgpu::PrimitiveTopology::TriangleList
            },
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: water.then_some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: false,
            depth_compare: wgpu::CompareFunction::GreaterEqual,
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: Default::default(),
        multiview: None,
        cache: None,
    })
}
impl TemporalOpticsMask {
    #[allow(clippy::too_many_arguments)] // Optional prepared sources and borrowed exact target formats.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        enabled: bool,
        water: Option<&WaterGpu>,
        cloud: Option<&CloudGpu>,
        motion: &wgpu::TextureView,
        surface: &wgpu::TextureView,
    ) -> Result<Option<Self>, Error> {
        if !enabled || water.is_none() && cloud.is_none() {
            return Ok(None);
        }
        let extent = targets(motion, surface)?;
        let ws = water.map(WaterGpu::temporal_source);
        let cs = cloud.map(CloudGpu::temporal_source);
        validate(extent, ws.as_ref(), cs.as_ref())?;
        let cloud_source = cs.as_ref().map(|_| cloud_shader_source()).transpose()?;
        let water =
            water.map(|w| pipeline(device, &w.temporal_layout(), &water_shader_source(), true));
        let cloud = cloud.map(|c| {
            pipeline(
                device,
                &c.temporal_layout(),
                cloud_source.as_ref().unwrap(),
                false,
            )
        });
        Ok(Some(Self {
            water,
            cloud,
            motion: motion.clone(),
            surface: surface.clone(),
            extent,
            timer: GpuPassTimer::new(device, queue, "optical temporal invalidity"),
        }))
    }
    pub fn pending_work(
        &self,
        water: Option<&WaterGpu>,
        cloud: Option<&CloudGpu>,
    ) -> Result<Work, Error> {
        if self.water.is_some() != water.is_some() || self.cloud.is_some() != cloud.is_some() {
            return Err(Error::SourcesChanged);
        }
        let ws = water.map(WaterGpu::temporal_source);
        let cs = cloud.map(CloudGpu::temporal_source);
        validate(self.extent, ws.as_ref(), cs.as_ref())?;
        let water = ws.as_ref().is_some_and(|w| w.enabled && w.quads > 0);
        let passes = u32::from(water) + u32::from(cs.is_some());
        let pixels = u64::from(self.extent[0]) * u64::from(self.extent[1]);
        Ok(Work {
            passes,
            draws: passes,
            pixels: pixels * u64::from(passes),
            texture_loads_bound: if cs.is_some() { pixels * 10 } else { 0 },
            vertex_invocations_bound: ws
                .as_ref()
                .filter(|_| water)
                .map_or(0, |w| u64::from(w.quads) * 4)
                + if cs.is_some() { 3 } else { 0 },
        })
    }
    /// Only after the matching current optical color/depth passes, and before
    /// temporal resolve. Cached UI-only presentation calls clear_timing instead.
    pub fn encode(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        water: Option<&WaterGpu>,
        cloud: Option<&CloudGpu>,
        max_pixels: u64,
    ) -> Result<Work, Error> {
        self.timer.begin();
        let work = self.pending_work(water, cloud)?;
        if work.pixels > max_pixels {
            return Err(Error::WorkBudget);
        }
        let ws = water.map(WaterGpu::temporal_source);
        let cs = cloud.map(CloudGpu::temporal_source);
        if ws
            .as_ref()
            .is_some_and(|w| w.enabled && w.quads > 0 && !w.prepared)
            || cs.as_ref().is_some_and(|c| !c.prepared)
        {
            return Err(Error::NotPrepared);
        }
        let attachments = [
            Some(wgpu::RenderPassColorAttachment {
                view: &self.motion,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            }),
            Some(wgpu::RenderPassColorAttachment {
                view: &self.surface,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            }),
        ];
        if let Some(w) = ws.as_ref().filter(|w| w.enabled && w.quads > 0) {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("exact physical water coverage history invalidation"),
                color_attachments: &attachments,
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: w.depth,
                    depth_ops: None,
                    stencil_ops: None,
                }),
                timestamp_writes: self.timer.render(1),
                occlusion_query_set: None,
            });
            pass.set_pipeline(self.water.as_ref().unwrap());
            pass.set_bind_group(0, w.bind, &[]);
            pass.draw_indirect(w.indirect, 0);
        }
        if let Some(c) = cs.as_ref() {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("exact cloud transport history invalidation"),
                color_attachments: &attachments,
                depth_stencil_attachment: None,
                timestamp_writes: self.timer.render(2),
                occlusion_query_set: None,
            });
            pass.set_pipeline(self.cloud.as_ref().unwrap());
            pass.set_bind_group(0, c.bind, &[]);
            pass.draw(0..3, 0..1);
        }
        self.timer.finish(encoder);
        Ok(work)
    }
    pub fn resize_inputs(
        &mut self,
        motion: &wgpu::TextureView,
        surface: &wgpu::TextureView,
    ) -> Result<(), Error> {
        let extent = targets(motion, surface)?;
        self.motion = motion.clone();
        self.surface = surface.clone();
        self.extent = extent;
        self.clear_timing();
        Ok(())
    }
    pub fn extent(&self) -> [u32; 2] {
        self.extent
    }
    pub fn pipeline_compilations(&self) -> u32 {
        u32::from(self.water.is_some()) + u32::from(self.cloud.is_some())
    }
    pub fn target_bytes(&self) -> u64 {
        0
    }
    pub fn allocation_bytes(&self) -> u64 {
        self.timer
            .timestamp_buffer()
            .map_or(0, |(buffer, _)| buffer.size())
    }
    pub fn graph(&self) -> PassGraph<2> {
        self.timer.graph()
    }
    pub fn clear_timing(&self) {
        self.timer.begin();
    }
    pub fn timing_source(&self) -> Option<TimingSource<'_>> {
        let count = self.timer.graph().count;
        if count == 0 {
            return None;
        }
        let (buffer, period) = self.timer.timestamp_buffer()?;
        Some(TimingSource {
            buffer,
            period,
            kinds: [6; 32],
            count,
        })
    }
}
