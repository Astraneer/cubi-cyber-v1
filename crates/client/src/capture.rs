//! Two fixed GPU readbacks. Mapping never blocks a frame and completion is tied
//! to the original submitted frame. Unsupported or missed measurements stay None.
use render::terrain_gpu::{GpuFrameGraph, GpuPassKind, TerrainGpu};
use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};
use telemetry::flight::FlightRecorder;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CaptureStats {
    pub scheduled: u64,
    pub completed: u64,
    pub unsupported: u64,
    pub full: u64,
    pub expired: u64,
    pub failed: u64,
    pub cancelled: u64,
}
#[derive(Clone, Copy, Debug)]
pub struct CaptureTicket {
    slot: usize,
    generation: u64,
}
struct Slot {
    buffer: wgpu::Buffer,
    state: Arc<AtomicU8>,
    graph: Option<GpuFrameGraph>,
    period: f32,
    frame: u64,
    generation: u64,
}
pub struct FrameGpuCapture {
    slots: [Slot; 2],
    stats: CaptureStats,
}
impl FrameGpuCapture {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            slots: std::array::from_fn(|_| Slot {
                buffer: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("nonblocking frame timings"),
                    size: 512,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                }),
                state: Arc::new(AtomicU8::new(0)),
                graph: None,
                period: 0.0,
                frame: 0,
                generation: 0,
            }),
            stats: CaptureStats::default(),
        }
    }
    /// Call immediately after View.encode and before submission. If submission
    /// is abandoned, call cancel rather than leaving this slot reserved.
    pub fn encode(
        &mut self,
        renderer: &TerrainGpu,
        encoder: &mut wgpu::CommandEncoder,
        frame: u64,
    ) -> Option<CaptureTicket> {
        let Some((queries, period)) = renderer.timestamp_buffer() else {
            self.stats.unsupported += 1;
            return None;
        };
        let graph = renderer.frame_graph();
        if graph.count == 0 {
            return None;
        }
        if graph.count > 32 || !period.is_finite() || period <= 0.0 {
            self.stats.failed += 1;
            return None;
        }
        let Some((index, slot)) = self
            .slots
            .iter_mut()
            .enumerate()
            .find(|(_, slot)| slot.state.load(Ordering::Acquire) == 0)
        else {
            self.stats.full += 1;
            return None;
        };
        let Some(generation) = slot.generation.checked_add(1) else {
            self.stats.failed += 1;
            return None;
        };
        encoder.copy_buffer_to_buffer(queries, 0, &slot.buffer, 0, u64::from(graph.count) * 16);
        slot.graph = Some(graph);
        slot.period = period;
        slot.frame = frame;
        slot.generation = generation;
        slot.state.store(1, Ordering::Release);
        self.stats.scheduled += 1;
        Some(CaptureTicket {
            slot: index,
            generation,
        })
    }
    /// Call only after queue.submit for the encoder carrying this ticket.
    pub fn submitted(&mut self, ticket: CaptureTicket) {
        let slot = &mut self.slots[ticket.slot];
        if slot.generation != ticket.generation || slot.state.load(Ordering::Acquire) != 1 {
            return;
        }
        slot.state.store(2, Ordering::Release);
        let status = slot.state.clone();
        slot.buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                status.store(if result.is_ok() { 3 } else { 4 }, Ordering::Release);
            });
    }
    pub fn cancel(&mut self, ticket: CaptureTicket) {
        let slot = &mut self.slots[ticket.slot];
        if slot.generation == ticket.generation && slot.state.load(Ordering::Acquire) == 1 {
            slot.graph = None;
            slot.state.store(0, Ordering::Release);
            self.stats.cancelled += 1;
        }
    }
    /// Poll callbacks on native with device.poll(Poll); browsers invoke them via
    /// their event loop. This method never waits and creates no user containers.
    pub fn collect(&mut self, recorder: &mut FlightRecorder) {
        for slot in &mut self.slots {
            match slot.state.load(Ordering::Acquire) {
                3 => {
                    let data = slot.buffer.slice(..).get_mapped_range();
                    let measurement =
                        decode(&data, slot.graph.expect("prepared graph"), slot.period);
                    drop(data);
                    slot.buffer.unmap();
                    match measurement {
                        Some((total, passes)) => {
                            self.stats.completed += 1;
                            if !recorder.complete_gpu(slot.frame, total, passes) {
                                self.stats.expired += 1;
                            }
                        }
                        None => self.stats.failed += 1,
                    }
                    slot.graph = None;
                    slot.state.store(0, Ordering::Release);
                }
                4 => {
                    self.stats.failed += 1;
                    slot.graph = None;
                    slot.state.store(0, Ordering::Release);
                }
                _ => {}
            }
        }
    }
    pub fn pending(&self) -> u8 {
        self.slots
            .iter()
            .filter(|slot| slot.state.load(Ordering::Acquire) != 0)
            .count() as u8
    }
    pub const fn stats(&self) -> CaptureStats {
        self.stats
    }
}
fn decode(data: &[u8], graph: GpuFrameGraph, period: f32) -> Option<(u64, [Option<u64>; 16])> {
    let mut passes = [None; 16];
    let mut total = 0_u64;
    for (index, pass) in graph.passes[..graph.count as usize].iter().enumerate() {
        let offset = index * 16;
        let begin = u64::from_le_bytes(data.get(offset..offset + 8)?.try_into().ok()?);
        let end = u64::from_le_bytes(data.get(offset + 8..offset + 16)?.try_into().ok()?);
        let nanos = (end.checked_sub(begin)? as f64 * f64::from(period)).round();
        if !nanos.is_finite() || !(0.0..u64::MAX as f64).contains(&nanos) {
            return None;
        }
        let nanos = nanos as u64;
        total = total.checked_add(nanos)?;
        let slot = match pass {
            GpuPassKind::ResetVisibility | GpuPassKind::CullFrustum => 0,
            GpuPassKind::DepthPrepass => 1,
            GpuPassKind::HiZBase | GpuPassKind::HiZReduce => 2,
            GpuPassKind::CullOcclusion => 3,
            GpuPassKind::Color => 6, // Actual combined sky + terrain color pass.
            GpuPassKind::Presentation => 13,
            GpuPassKind::Unused => continue,
        };
        passes[slot] = Some(passes[slot].unwrap_or(0_u64).checked_add(nanos)?);
    }
    Some((total, passes))
}
