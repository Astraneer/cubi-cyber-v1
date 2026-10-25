//! Shared M0 sky pipeline. Platform shells own windows, surfaces, and event loops.
//! Resources and pipelines are prepared at startup and on explicit resize only.

pub const MSAA_SAMPLES: u32 = 4;
pub const MAX_FRAMES_IN_FLIGHT: u32 = 2;
pub const PROBE_WIDTH: u32 = 64;
pub const PROBE_HEIGHT: u32 = 64;

/// Platform backend family for shared offscreen tests and the browser shell.
/// The native shell explicitly attempts D3D12 before Vulkan on Windows.
pub fn instance() -> wgpu::Instance {
    #[cfg(target_arch = "wasm32")]
    let backends = wgpu::Backends::BROWSER_WEBGPU;
    #[cfg(not(target_arch = "wasm32"))]
    let backends = wgpu::Backends::DX12 | wgpu::Backends::VULKAN;
    instance_for(backends)
}

/// Build a restricted-backend instance for an explicit native fallback attempt.
pub fn instance_for(backends: wgpu::Backends) -> wgpu::Instance {
    wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends,
        ..Default::default()
    })
}

pub struct Gpu {
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

impl Gpu {
    pub async fn request(
        instance: &wgpu::Instance,
        surface: Option<&wgpu::Surface<'_>>,
        software: bool,
    ) -> Result<Self, String> {
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: software,
                compatible_surface: surface,
            })
            .await
            .map_err(|error| format!("WebGPU/Vulkan/D3D12 adapter unavailable: {error}"))?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("cubi GPU"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::MemoryUsage,
                trace: wgpu::Trace::Off,
            })
            .await
            .map_err(|error| format!("GPU device initialization failed: {error}"))?;
        Ok(Self {
            adapter,
            device,
            queue,
        })
    }
}

/// Where linear shader radiance is converted for the output attachment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputTransfer {
    /// Native sRGB attachments encode in hardware; unorm probes retain linear bytes.
    Attachment,
    /// The browser unorm canvas receives explicitly encoded sRGB bytes.
    ShaderSrgb,
}

pub struct SkyRenderer {
    pipeline: wgpu::RenderPipeline,
    multisampled: wgpu::Texture,
    multisampled_view: wgpu::TextureView,
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
    work: telemetry::WorkCounters,
}

impl SkyRenderer {
    /// The sole M0 pipeline is compiled before the first frame.
    pub fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
    ) -> Self {
        Self::new_with_transfer(device, format, width, height, OutputTransfer::Attachment)
    }

    /// Choose the final output transfer before pipeline creation, never per frame.
    pub fn new_with_transfer(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
        transfer: OutputTransfer,
    ) -> Self {
        assert!(
            transfer != OutputTransfer::ShaderSrgb || !format.is_srgb(),
            "explicit sRGB transfer requires an unorm attachment to avoid double encoding",
        );
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shared procedural sky"),
            source: wgpu::ShaderSource::Wgsl(include_str!("sky.wgsl").into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sky layout"),
            bind_group_layouts: &[],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("sky MSAA4 pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState {
                count: MSAA_SAMPLES,
                ..Default::default()
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some(match transfer {
                    OutputTransfer::Attachment => "fragment",
                    OutputTransfer::ShaderSrgb => "fragment_srgb_output",
                }),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
            cache: None,
        });
        let mut work = telemetry::WorkCounters::new();
        work.record(telemetry::WorkCounts {
            pipeline_compilations: 1,
            ..telemetry::WorkCounts::ZERO
        });
        let (multisampled, multisampled_view) = Self::attachment(device, format, width, height);
        Self {
            pipeline,
            multisampled,
            multisampled_view,
            format,
            width,
            height,
            work,
        }
    }

    /// Consume observed setup/frame work; frame submission never compiles pipelines.
    pub fn take_work(&mut self) -> telemetry::WorkCounts {
        self.work.take()
    }

    fn attachment(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
    ) -> (wgpu::Texture, wgpu::TextureView) {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("sky MSAA4 color"),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: MSAA_SAMPLES,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        (texture, view)
    }

    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        if width == 0 || height == 0 || (width == self.width && height == self.height) {
            return;
        }
        (self.multisampled, self.multisampled_view) =
            Self::attachment(device, self.format, width, height);
        self.width = width;
        self.height = height;
    }

    /// Record one sky pass; no user-side containers or per-frame vertex generation.
    pub fn encode(&self, encoder: &mut wgpu::CommandEncoder, output: &wgpu::TextureView) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("sky"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.multisampled_view,
                depth_slice: None,
                resolve_target: Some(output),
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Discard,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.draw(0..3, 0..1);
    }

    pub fn render(&self, device: &wgpu::Device, queue: &wgpu::Queue, output: &wgpu::TextureView) {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("sky frame"),
        });
        self.encode(&mut encoder, output);
        queue.submit([encoder.finish()]);
    }
}

/// GPU readback exists for startup/CI validation, outside the steady-state frame loop.
pub struct Readback {
    pub buffer: wgpu::Buffer,
    pub padded_bytes_per_row: u32,
    pub width: u32,
    pub height: u32,
    pub setup_work: telemetry::WorkCounts,
    pub frame_work: telemetry::WorkCounts,
    pub output_transfer: OutputTransfer,
}

pub fn render_probe(device: &wgpu::Device, queue: &wgpu::Queue) -> Readback {
    render_probe_with_transfer(device, queue, OutputTransfer::Attachment)
}

/// Exercise the same output variant used by the actual clients on a software GPU.
pub fn render_probe_with_transfer(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    transfer: OutputTransfer,
) -> Readback {
    let width = PROBE_WIDTH;
    let height = PROBE_HEIGHT;
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("sky probe output"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let mut renderer = SkyRenderer::new_with_transfer(device, format, width, height, transfer);
    let setup_work = renderer.take_work();
    let output = texture.create_view(&Default::default());
    let padded_bytes_per_row = (width * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("sky probe readback"),
        size: u64::from(padded_bytes_per_row) * u64::from(height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    renderer.encode(&mut encoder, &output);
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bytes_per_row),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);
    Readback {
        buffer,
        padded_bytes_per_row,
        width,
        height,
        setup_work,
        frame_work: renderer.take_work(),
        output_transfer: transfer,
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SkyProbe {
    pub top: [u8; 4],
    pub bottom: [u8; 4],
}

impl Readback {
    /// Verify the shared shader against its linear reference, not merely a nonblank image.
    pub fn validate(&self, bytes: &[u8]) -> Result<SkyProbe, String> {
        let pixel = |x: u32, y: u32| -> [u8; 4] {
            let offset = (y * self.padded_bytes_per_row + x * 4) as usize;
            bytes[offset..offset + 4].try_into().expect("four channels")
        };
        let expected_len = (self.padded_bytes_per_row * self.height) as usize;
        if bytes.len() != expected_len {
            return Err(format!(
                "sky readback length {} != {expected_len}",
                bytes.len()
            ));
        }
        for y in 0..self.height {
            let t = (y as f32 + 0.5) / self.height as f32;
            let blend = t * t * (3.0 - 2.0 * t);
            let zenith = [0.08, 0.25, 0.60];
            let horizon = [0.65, 0.82, 1.0];
            for x in 0..self.width {
                let actual = pixel(x, y);
                for channel in 0..3 {
                    let linear: f32 =
                        zenith[channel] + (horizon[channel] - zenith[channel]) * blend;
                    let encoded = if self.output_transfer == OutputTransfer::ShaderSrgb {
                        if linear <= 0.0031308 {
                            12.92 * linear
                        } else {
                            1.055 * linear.powf(1.0 / 2.4) - 0.055
                        }
                    } else {
                        linear
                    };
                    let reference = (encoded * 255.0).round() as u8;
                    if actual[channel].abs_diff(reference) > 2 {
                        return Err(format!(
                            "sky mismatch at ({x},{y}) channel {channel}: {} vs {reference}",
                            actual[channel]
                        ));
                    }
                }
                if actual[3] != 255 {
                    return Err(format!("sky alpha at ({x},{y}) is {}", actual[3]));
                }
            }
        }
        Ok(SkyProbe {
            top: pixel(self.width / 2, 0),
            bottom: pixel(self.width / 2, self.height - 1),
        })
    }
}
