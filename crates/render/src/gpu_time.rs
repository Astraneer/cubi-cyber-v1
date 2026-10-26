//! Fixed-capacity pass timing; no frame allocations and no queries when unsupported.
use std::cell::Cell;

/// An owned pass mapping borrowing one resolved timestamp buffer. Kinds refer
/// to telemetry::flight::GPU_PASSES; 255 retains only the measured total.
#[derive(Clone, Copy, Debug)]
pub struct TimingSource<'a> {
    pub buffer: &'a wgpu::Buffer,
    pub period: f32,
    pub kinds: [u8; 32],
    pub count: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct PassGraph<const N: usize> {
    pub kinds: [u32; N],
    pub count: u32,
    pub has_timestamps: bool,
}
struct Queries {
    set: wgpu::QuerySet,
    resolved: wgpu::Buffer,
    period: f32,
}
pub struct GpuPassTimer<const N: usize> {
    queries: Option<Queries>,
    count: Cell<u32>,
    kinds: Cell<[u32; N]>,
}
impl<const N: usize> GpuPassTimer<N> {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, label: &str) -> Self {
        assert!(N > 0 && N <= 64);
        let queries = device
            .features()
            .contains(wgpu::Features::TIMESTAMP_QUERY)
            .then(|| Queries {
                set: device.create_query_set(&wgpu::QuerySetDescriptor {
                    label: Some(label),
                    ty: wgpu::QueryType::Timestamp,
                    count: N as u32 * 2,
                }),
                resolved: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(label),
                    size: (N as u64 * 16).div_ceil(256) * 256,
                    usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                }),
                period: queue.get_timestamp_period(),
            });
        Self {
            queries,
            count: Cell::new(0),
            kinds: Cell::new([0; N]),
        }
    }
    pub fn begin(&self) {
        self.count.set(0);
        self.kinds.set([0; N]);
    }
    fn record(&self, kind: u32) -> u32 {
        let index = self.count.get();
        assert!((index as usize) < N, "pass timer capacity");
        let mut kinds = self.kinds.get();
        kinds[index as usize] = kind;
        self.kinds.set(kinds);
        self.count.set(index + 1);
        index
    }
    pub fn compute(&self, kind: u32) -> Option<wgpu::ComputePassTimestampWrites<'_>> {
        let index = self.record(kind);
        self.queries
            .as_ref()
            .map(|queries| wgpu::ComputePassTimestampWrites {
                query_set: &queries.set,
                beginning_of_pass_write_index: Some(index * 2),
                end_of_pass_write_index: Some(index * 2 + 1),
            })
    }
    pub fn render(&self, kind: u32) -> Option<wgpu::RenderPassTimestampWrites<'_>> {
        let index = self.record(kind);
        self.queries
            .as_ref()
            .map(|queries| wgpu::RenderPassTimestampWrites {
                query_set: &queries.set,
                beginning_of_pass_write_index: Some(index * 2),
                end_of_pass_write_index: Some(index * 2 + 1),
            })
    }
    pub fn finish(&self, encoder: &mut wgpu::CommandEncoder) {
        if let Some(queries) = &self.queries
            && self.count.get() > 0
        {
            encoder.resolve_query_set(&queries.set, 0..self.count.get() * 2, &queries.resolved, 0);
        }
    }
    pub fn graph(&self) -> PassGraph<N> {
        PassGraph {
            kinds: self.kinds.get(),
            count: self.count.get(),
            has_timestamps: self.queries.is_some(),
        }
    }
    pub fn timestamp_buffer(&self) -> Option<(&wgpu::Buffer, f32)> {
        self.queries
            .as_ref()
            .map(|queries| (&queries.resolved, queries.period))
    }
    /// Resolve offset alignment is handled in the private buffer; ordinary copies
    /// can append these timings to a larger frame graph at any four-byte offset.
    pub fn copy_into(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        destination: &wgpu::Buffer,
        offset: u64,
    ) {
        if let Some(queries) = &self.queries
            && self.count.get() > 0
        {
            encoder.copy_buffer_to_buffer(
                &queries.resolved,
                0,
                destination,
                offset,
                u64::from(self.count.get()) * 16,
            );
        }
    }
}

/// Whole command-graph scope, including copies and subsystem-owned passes.
/// Timestamp-only boundary passes work with WebGPU TIMESTAMP_QUERY; no optional
/// inside-encoder timestamp feature or frame-time pipeline is required.
pub struct GpuGraphTimer {
    queries: Option<Queries>,
    started: Cell<bool>,
}
impl GpuGraphTimer {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, label: &str) -> Self {
        let queries = device
            .features()
            .contains(wgpu::Features::TIMESTAMP_QUERY)
            .then(|| Queries {
                set: device.create_query_set(&wgpu::QuerySetDescriptor {
                    label: Some(label),
                    ty: wgpu::QueryType::Timestamp,
                    count: 2,
                }),
                resolved: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(label),
                    size: 256,
                    usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                }),
                period: queue.get_timestamp_period(),
            });
        Self {
            queries,
            started: Cell::new(false),
        }
    }
    pub fn begin_frame(&self) {
        self.started.set(false);
    }
    pub fn start(&self, encoder: &mut wgpu::CommandEncoder) {
        if self.started.replace(true) {
            return;
        }
        if let Some(queries) = &self.queries {
            let _pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("whole GPU graph begins"),
                timestamp_writes: Some(wgpu::ComputePassTimestampWrites {
                    query_set: &queries.set,
                    beginning_of_pass_write_index: Some(0),
                    end_of_pass_write_index: None,
                }),
            });
        }
    }
    pub fn finish(&self, encoder: &mut wgpu::CommandEncoder) {
        if self.started.get()
            && let Some(queries) = &self.queries
        {
            {
                let _pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("whole GPU graph ends"),
                    timestamp_writes: Some(wgpu::ComputePassTimestampWrites {
                        query_set: &queries.set,
                        beginning_of_pass_write_index: None,
                        end_of_pass_write_index: Some(1),
                    }),
                });
            }
            encoder.resolve_query_set(&queries.set, 0..2, &queries.resolved, 0);
        }
    }
    pub fn timestamp_buffer(&self) -> Option<(&wgpu::Buffer, f32)> {
        self.queries
            .as_ref()
            .filter(|_| self.started.get())
            .map(|q| (&q.resolved, q.period))
    }
}
