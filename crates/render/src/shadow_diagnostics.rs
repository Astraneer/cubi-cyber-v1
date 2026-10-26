//! Opt-in native node attribution without splitting the measured shadow passes.
use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

use super::CASCADES;

#[derive(Clone, Copy, Debug)]
#[repr(u32)]
pub enum ShadowNode {
    StaticCull,
    Clear,
    StaticDraw,
    Cull,
    Restore,
    HeightDraw,
    OpaqueDraw,
    CutoutDraw,
}
impl ShadowNode {
    pub const ALL: [Self; 8] = [
        Self::StaticCull,
        Self::Clear,
        Self::StaticDraw,
        Self::Cull,
        Self::Restore,
        Self::HeightDraw,
        Self::OpaqueDraw,
        Self::CutoutDraw,
    ];
    pub const fn label(self) -> &'static str {
        match self {
            Self::StaticCull => "static_cull",
            Self::Clear => "clear",
            Self::StaticDraw => "static_draw",
            Self::Cull => "caster_cull",
            Self::Restore => "restore",
            Self::HeightDraw => "height_draw",
            Self::OpaqueDraw => "opaque_draw",
            Self::CutoutDraw => "cutout_draw",
        }
    }
    pub const fn slot(self, cascade: usize) -> usize {
        cascade * Self::ALL.len() + self as usize
    }
}

/// Explicitly prepared diagnostic resource. Ordinary shadow construction and
/// encoding use `None`, requesting no extra features, queries or buffers.
#[derive(Clone)]
pub struct ShadowNodeTimings {
    set: wgpu::QuerySet,
    resolved: wgpu::Buffer,
    written: Arc<AtomicU32>,
}
impl ShadowNodeTimings {
    pub const FEATURES: wgpu::Features = wgpu::Features::TIMESTAMP_QUERY
        .union(wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS)
        .union(wgpu::Features::TIMESTAMP_QUERY_INSIDE_PASSES);
    pub const SLOTS: usize = CASCADES * ShadowNode::ALL.len();
    pub const CAPTURE_BYTES: u64 = Self::SLOTS as u64 * 16;

    pub fn new(device: &wgpu::Device) -> Option<Self> {
        device.features().contains(Self::FEATURES).then(|| Self {
            set: device.create_query_set(&wgpu::QuerySetDescriptor {
                label: Some("optional disjoint shadow node timestamps"),
                ty: wgpu::QueryType::Timestamp,
                count: Self::SLOTS as u32 * 2,
            }),
            resolved: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("optional aligned shadow node resolutions"),
                size: Self::SLOTS as u64 * wgpu::QUERY_RESOLVE_BUFFER_ALIGNMENT,
                usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }),
            written: Arc::new(AtomicU32::new(0)),
        })
    }
    pub fn begin_frame(&self) {
        self.written.store(0, Ordering::Release);
    }
    pub fn written_slots(&self) -> u32 {
        self.written.load(Ordering::Acquire)
    }
    pub(crate) fn encoder_begin(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        cascade: usize,
        node: ShadowNode,
    ) {
        encoder.write_timestamp(&self.set, node.slot(cascade) as u32 * 2);
    }
    pub(crate) fn encoder_end(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        cascade: usize,
        node: ShadowNode,
    ) {
        let slot = node.slot(cascade) as u32;
        encoder.write_timestamp(&self.set, slot * 2 + 1);
        self.written.fetch_or(1 << slot, Ordering::Release);
    }
    pub(crate) fn render_begin(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        cascade: usize,
        node: ShadowNode,
    ) {
        pass.write_timestamp(&self.set, node.slot(cascade) as u32 * 2);
    }
    pub(crate) fn render_end(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        cascade: usize,
        node: ShadowNode,
    ) {
        let slot = node.slot(cascade) as u32;
        pass.write_timestamp(&self.set, slot * 2 + 1);
        self.written.fetch_or(1 << slot, Ordering::Release);
    }
    /// Resolve only scopes actually encoded this frame; unwritten query slots
    /// are never resolved. Each scope has its own required 256-byte alignment.
    pub fn copy_into(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::Buffer,
        offset: u64,
    ) {
        let written = self.written_slots();
        for slot in 0..Self::SLOTS as u32 {
            if written & (1 << slot) == 0 {
                continue;
            }
            let resolved_offset = u64::from(slot) * wgpu::QUERY_RESOLVE_BUFFER_ALIGNMENT;
            encoder.resolve_query_set(
                &self.set,
                slot * 2..slot * 2 + 2,
                &self.resolved,
                resolved_offset,
            );
            encoder.copy_buffer_to_buffer(
                &self.resolved,
                resolved_offset,
                target,
                offset + u64::from(slot) * 16,
                16,
            );
        }
    }
}
