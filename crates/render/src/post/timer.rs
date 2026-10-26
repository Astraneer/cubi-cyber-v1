//! Keep the original query resources in the default MSAA4 profile.
use crate::gpu_time::{GpuPassTimer, PassGraph};
pub(super) enum PostTimer {
    Legacy(GpuPassTimer<12>),
    Smaa(GpuPassTimer<16>),
}
impl PostTimer {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, smaa: bool) -> Self {
        if smaa {
            Self::Smaa(GpuPassTimer::new(
                device,
                queue,
                "HDR post and SMAA timings",
            ))
        } else {
            Self::Legacy(GpuPassTimer::new(device, queue, "HDR post timings"))
        }
    }
    pub fn begin(&self) {
        match self {
            Self::Legacy(t) => t.begin(),
            Self::Smaa(t) => t.begin(),
        }
    }
    pub fn compute(&self, kind: u32) -> Option<wgpu::ComputePassTimestampWrites<'_>> {
        match self {
            Self::Legacy(t) => t.compute(kind),
            Self::Smaa(t) => t.compute(kind),
        }
    }
    pub fn render(&self, kind: u32) -> Option<wgpu::RenderPassTimestampWrites<'_>> {
        match self {
            Self::Legacy(t) => t.render(kind),
            Self::Smaa(t) => t.render(kind),
        }
    }
    pub fn smaa(&self) -> Option<&GpuPassTimer<16>> {
        match self {
            Self::Smaa(t) => Some(t),
            _ => None,
        }
    }
    pub fn finish(&self, encoder: &mut wgpu::CommandEncoder) {
        match self {
            Self::Legacy(t) => t.finish(encoder),
            Self::Smaa(t) => t.finish(encoder),
        }
    }
    pub fn timestamp_buffer(&self) -> Option<(&wgpu::Buffer, f32)> {
        match self {
            Self::Legacy(t) => t.timestamp_buffer(),
            Self::Smaa(t) => t.timestamp_buffer(),
        }
    }
    pub fn copy_into(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        destination: &wgpu::Buffer,
        offset: u64,
    ) {
        match self {
            Self::Legacy(t) => t.copy_into(encoder, destination, offset),
            Self::Smaa(t) => t.copy_into(encoder, destination, offset),
        }
    }
    pub fn graph(&self) -> PassGraph<16> {
        match self {
            Self::Smaa(t) => t.graph(),
            Self::Legacy(t) => {
                let g = t.graph();
                let mut kinds = [0; 16];
                kinds[..12].copy_from_slice(&g.kinds);
                PassGraph {
                    kinds,
                    count: g.count,
                    has_timestamps: g.has_timestamps,
                }
            }
        }
    }
}
