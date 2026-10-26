//! Optional face-resolved first bounce from the actual atmosphere and shadows.
use crate::{atmosphere::AtmosphereGpu, gi::GiError, shadows::ShadowGpu};
use bytemuck::{Pod, Zeroable};

/// Six faces use terrain's Down, Up, North, South, West, East order.
/// Albedo is linear UNORM8; sky and full RGB8 are the public M4 light channels.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
pub struct GiSurface {
    words: [u32; 12],
}
impl GiSurface {
    pub const EMPTY: Self = Self { words: [0; 12] };
    pub fn words(&self) -> &[u32; 12] {
        &self.words
    }
    pub fn from_words(words: [u32; 12]) -> Option<Self> {
        words
            .chunks_exact(2)
            .all(|pair| {
                pair[0] & 0xe000_0000 == 0
                    && pair[1] & 0xff00_0000 == 0
                    && ((pair[0] & 0x1000_0000) != 0 || (pair[0] == 0 && pair[1] == 0))
            })
            .then_some(Self { words })
    }
    pub fn with_face(mut self, face: u8, albedo: [f32; 3], sky: u8, rgb: [u8; 3]) -> Option<Self> {
        if face >= 6
            || sky > 15
            || albedo
                .iter()
                .any(|value| !value.is_finite() || !(0.0..=1.0).contains(value))
        {
            return None;
        }
        let color = albedo.map(|value| (value * 255.).round() as u32);
        self.words[usize::from(face) * 2] =
            color[0] | (color[1] << 8) | (color[2] << 16) | (u32::from(sky) << 24) | (1 << 28);
        self.words[usize::from(face) * 2 + 1] =
            u32::from(rgb[0]) | (u32::from(rgb[1]) << 8) | (u32::from(rgb[2]) << 16);
        Some(self)
    }
}
#[derive(Clone, Copy, Debug)]
pub struct SurfaceUpdate {
    pub local: u16,
    pub value: GiSurface,
}
pub(crate) struct Directional {
    pub surfaces: wgpu::Buffer,
    pub staging: Box<[GiSurface; 4096]>,
    pub bind: wgpu::BindGroup,
    pub reset: wgpu::ComputePipeline,
    pub trace: wgpu::ComputePipeline,
}
pub(crate) struct Sources<'a> {
    pub uniform: &'a wgpu::Buffer,
    pub world: &'a wgpu::Buffer,
    pub probes: &'a wgpu::Buffer,
    pub jobs: &'a wgpu::Buffer,
    pub rays: &'a wgpu::Buffer,
    pub statistics: &'a wgpu::Buffer,
}
impl Directional {
    pub fn new(
        device: &wgpu::Device,
        pages: usize,
        sources: Sources<'_>,
        atmosphere: &AtmosphereGpu,
        shadows: &ShadowGpu,
    ) -> Result<Self, GiError> {
        let bytes = (pages as u64)
            .checked_mul(4096 * 48)
            .ok_or(GiError::Capacity)?;
        if bytes > device.limits().max_buffer_size
            || bytes > u64::from(device.limits().max_storage_buffer_binding_size)
        {
            return Err(GiError::Capacity);
        }
        let surfaces = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("optional GI face-resolved albedo and public light"),
            size: bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let buffer =
            |binding, uniform, read_only| super::gi::buffer_entry(binding, uniform, read_only);
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("directional atmosphere first-bounce GI bindings"),
            entries: &[
                buffer(0, true, true),
                buffer(1, false, true),
                buffer(2, false, false),
                buffer(3, false, true),
                buffer(4, false, true),
                buffer(5, false, false),
                buffer(6, false, true),
                buffer(7, false, true),
                buffer(8, true, true),
                wgpu::BindGroupLayoutEntry {
                    binding: 9,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 10,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                    count: None,
                },
            ],
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("live atmosphere and cached shadows to GI"),
            layout: &layout,
            entries: &[
                super::gi::binding(0, sources.uniform),
                super::gi::binding(1, sources.world),
                super::gi::binding(2, sources.probes),
                super::gi::binding(3, sources.jobs),
                super::gi::binding(4, sources.rays),
                super::gi::binding(5, sources.statistics),
                super::gi::binding(6, &surfaces),
                super::gi::binding(7, atmosphere.lighting_buffer()),
                super::gi::binding(8, shadows.sampling_buffer()),
                wgpu::BindGroupEntry {
                    binding: 9,
                    resource: wgpu::BindingResource::TextureView(shadows.depth_view()),
                },
                wgpu::BindGroupEntry {
                    binding: 10,
                    resource: wgpu::BindingResource::Sampler(shadows.sampler()),
                },
            ],
        });
        let shadow = include_str!("shadow_sampling.wgsl")
            .replace("@binding(9)", "@binding(8)")
            .replace("@binding(10)", "@binding(9)")
            .replace("@binding(11)", "@binding(10)");
        let atmosphere =
            include_str!("atmosphere_lighting.wgsl").replace("@binding(20)", "@binding(7)");
        let code = format!(
            "{atmosphere}\n{shadow}\n{}\n{}",
            include_str!("gi_directional.wgsl"),
            include_str!("gi.wgsl")
        );
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("bounded atmosphere and shadow-certified surface GI DDA"),
            source: wgpu::ShaderSource::Wgsl(code.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("optional directional GI pipeline layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = |entry| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&pipeline_layout),
                module: &shader,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        Ok(Self {
            surfaces,
            staging: Box::new([GiSurface::EMPTY; 4096]),
            bind,
            reset: pipeline("reset_statistics"),
            trace: pipeline("trace_probes"),
        })
    }
}
