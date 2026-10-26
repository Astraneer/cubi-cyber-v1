//! Freshness wrapper around the shared WebGPU command-graph timer. All resources
//! are prepared during loading; an abandoned encoder never creates a sample.
use render::gpu_time::{GpuGraphTimer, TimingSource};

pub(super) struct WholeFrame {
    timer: GpuGraphTimer,
    active: bool,
    ready: bool,
}
impl WholeFrame {
    pub(super) fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        Self {
            timer: GpuGraphTimer::new(device, queue, "complete frame interval"),
            active: false,
            ready: false,
        }
    }
    pub(super) fn begin(&mut self, encoder: &mut wgpu::CommandEncoder, period: f32) -> bool {
        self.ready = false;
        self.active = false;
        self.timer.begin_frame();
        if !period.is_finite() || period <= 0. {
            return false;
        }
        self.timer.start(encoder);
        self.active = self
            .timer
            .timestamp_buffer()
            .is_some_and(|(_, actual)| actual == period);
        self.active
    }
    pub(super) fn end(&mut self, encoder: &mut wgpu::CommandEncoder) {
        self.ready = false;
        if !self.active {
            return;
        }
        self.active = false;
        self.timer.finish(encoder);
        self.ready = true;
    }
    pub(super) fn source(&self) -> Option<TimingSource<'_>> {
        if !self.ready {
            return None;
        }
        let (buffer, period) = self.timer.timestamp_buffer()?;
        let mut kinds = [255; 32];
        kinds[0] = 254;
        Some(TimingSource {
            buffer,
            period,
            kinds,
            count: 1,
        })
    }
}
