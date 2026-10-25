//! Two fixed GPU readbacks. Mapping never blocks a frame and completion is tied
//! to the original submitted frame. Unsupported or missed measurements stay None.
use render::{
    gpu_time::TimingSource,
    terrain_gpu::{GpuPassKind, TerrainGpu},
};
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
#[derive(Clone, Copy)]
struct FrameQueryGraph {
    kinds: [u8; 64],
    periods: [f32; 64],
    count: usize,
}
struct Slot {
    buffer: wgpu::Buffer,
    state: Arc<AtomicU8>,
    graph: Option<FrameQueryGraph>,
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
                    size: 1024,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                }),
                state: Arc::new(AtomicU8::new(0)),
                graph: None,
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
        self.encode_terrain_and(renderer, &[], encoder, frame)
    }
    pub fn encode_terrain_and(
        &mut self,
        renderer: &TerrainGpu,
        extra: &[TimingSource<'_>],
        encoder: &mut wgpu::CommandEncoder,
        frame: u64,
    ) -> Option<CaptureTicket> {
        let terrain = renderer.timestamp_buffer().map(|(buffer, period)| {
            let graph = renderer.frame_graph();
            let kinds = std::array::from_fn(|index| terrain_kind(graph.passes[index]));
            TimingSource {
                buffer,
                period,
                kinds,
                count: graph.count,
            }
        });
        self.encode_sources(
            terrain.into_iter().chain(extra.iter().copied()),
            encoder,
            frame,
        )
    }
    /// Copy up to 64 queries from at most 16 frame-graph nodes, without waiting
    /// or allocating containers. Each pass can use its own timestamp period.
    pub fn encode_sources<'a>(
        &mut self,
        sources: impl IntoIterator<Item = TimingSource<'a>>,
        encoder: &mut wgpu::CommandEncoder,
        frame: u64,
    ) -> Option<CaptureTicket> {
        let mut graph = FrameQueryGraph {
            kinds: [255; 64],
            periods: [0.0; 64],
            count: 0,
        };
        let mut buffers = [None; 16];
        let mut source_count = 0;
        for source in sources {
            let count = source.count as usize;
            if source_count == buffers.len()
                || count > 32
                || graph.count + count > 64
                || !source.period.is_finite()
                || source.period <= 0.0
                || source.buffer.size() < source.count as u64 * 16
                || !source.buffer.usage().contains(wgpu::BufferUsages::COPY_SRC)
                || source.kinds[..count]
                    .iter()
                    .any(|&kind| kind >= 16 && kind != 255)
            {
                self.stats.failed += 1;
                return None;
            }
            graph.kinds[graph.count..graph.count + count].copy_from_slice(&source.kinds[..count]);
            graph.periods[graph.count..graph.count + count].fill(source.period);
            graph.count += count;
            buffers[source_count] = Some(source);
            source_count += 1;
        }
        if source_count == 0 {
            self.stats.unsupported += 1;
            return None;
        }
        if graph.count == 0 {
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
        let mut offset = 0;
        for source in buffers[..source_count].iter().flatten() {
            let bytes = u64::from(source.count) * 16;
            if bytes != 0 {
                encoder.copy_buffer_to_buffer(source.buffer, 0, &slot.buffer, offset, bytes);
            }
            offset += bytes;
        }
        slot.graph = Some(graph);
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
                    let measurement = decode(&data, slot.graph.expect("prepared graph"));
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
fn decode(data: &[u8], graph: FrameQueryGraph) -> Option<(u64, [Option<u64>; 16])> {
    let mut passes = [None; 16];
    let mut total = 0_u64;
    for (index, pass) in graph.kinds[..graph.count].iter().enumerate() {
        let offset = index * 16;
        let begin = u64::from_le_bytes(data.get(offset..offset + 8)?.try_into().ok()?);
        let end = u64::from_le_bytes(data.get(offset + 8..offset + 16)?.try_into().ok()?);
        let nanos = (end.checked_sub(begin)? as f64 * f64::from(graph.periods[index])).round();
        if !nanos.is_finite() || !(0.0..u64::MAX as f64).contains(&nanos) {
            return None;
        }
        let nanos = nanos as u64;
        total = total.checked_add(nanos)?;
        if *pass == 255 {
            continue;
        }
        let slot = *pass as usize;
        passes[slot] = Some(passes[slot].unwrap_or(0_u64).checked_add(nanos)?);
    }
    Some((total, passes))
}

fn terrain_kind(pass: GpuPassKind) -> u8 {
    match pass {
        GpuPassKind::ResetVisibility | GpuPassKind::CullFrustum => 0,
        GpuPassKind::DepthPrepass => 1,
        GpuPassKind::HiZBase | GpuPassKind::HiZReduce => 2,
        GpuPassKind::CullOcclusion => 3,
        GpuPassKind::Color => 6,
        GpuPassKind::Presentation => 13,
        GpuPassKind::Unused => 255,
    }
}
