//! Bounded presentation of actual recorder samples. Missing timings remain
//! missing; concurrent observations do not claim a cause for a frame hitch.
use crate::paint::{Painter, Rect, TextBuffer};
use std::fmt::Write;
use telemetry::{
    AllocationCounts, WorkCounts,
    flight::{CPU_STAGES, FlightRecorder, GPU_PASSES, Loss, Observations, WINDOW_NS},
};
pub const GRAPH_BUCKETS: usize = 120;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GraphPoint {
    pub interval_ns: Option<u64>,
    pub cpu_ns: Option<u64>,
    pub gpu_ns: Option<u64>,
    pub observations: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProfilerSnapshot {
    pub measured: bool,
    pub complete_window: bool,
    pub loss: Loss,
    pub frame: u64,
    pub interval_ns: u64,
    pub cpu_ns: Option<u64>,
    pub gpu_ns: Option<u64>,
    pub gpu_total_complete: bool,
    pub cpu_stages_ns: [Option<u64>; 8],
    pub gpu_passes_ns: [Option<u64>; 16],
    pub work: WorkCounts,
    pub allocations: Option<AllocationCounts>,
    pub observations: Observations,
    pub queued_jobs: u32,
    pub oldest_job_ticks: u64,
    pub in_flight: u8,
    pub graph: [GraphPoint; GRAPH_BUCKETS],
}
impl Default for ProfilerSnapshot {
    fn default() -> Self {
        Self {
            measured: false,
            complete_window: false,
            loss: Loss::default(),
            frame: 0,
            interval_ns: 0,
            cpu_ns: None,
            gpu_ns: None,
            gpu_total_complete: false,
            cpu_stages_ns: [None; 8],
            gpu_passes_ns: [None; 16],
            work: WorkCounts::ZERO,
            allocations: None,
            observations: Observations::default(),
            queued_jobs: 0,
            oldest_job_ticks: 0,
            in_flight: 0,
            graph: [GraphPoint::default(); GRAPH_BUCKETS],
        }
    }
}
fn maximum(value: &mut Option<u64>, sample: Option<u64>) {
    if let Some(sample) = sample {
        *value = Some(value.map_or(sample, |old| old.max(sample)));
    }
}
impl ProfilerSnapshot {
    /// Call at the host's debug update cadence. Uses no heap storage and retains
    /// the maximum in each 1/12-second bucket, so a short spike stays visible.
    pub fn from_recorder(recorder: &FlightRecorder) -> Self {
        let Some(last) = recorder.frames().last().copied() else {
            return Self::default();
        };
        let mut result = Self {
            measured: true,
            complete_window: recorder.complete_window(),
            loss: recorder.loss(),
            frame: last.frame,
            interval_ns: last.interval_ns,
            cpu_ns: last.cpu_ns,
            gpu_ns: last.gpu_ns,
            gpu_total_complete: last.gpu_total_complete,
            cpu_stages_ns: last.cpu_stages_ns,
            gpu_passes_ns: last.gpu_passes_ns,
            work: last.work,
            allocations: last.allocations,
            observations: last.observations,
            queued_jobs: last.queued_jobs,
            oldest_job_ticks: last.oldest_job_ticks,
            in_flight: last.in_flight,
            ..Self::default()
        };
        for sample in recorder.frames() {
            let age = last.monotonic_ns.saturating_sub(sample.monotonic_ns);
            if age > WINDOW_NS {
                continue;
            }
            let bucket = GRAPH_BUCKETS
                - 1
                - ((u128::from(age) * GRAPH_BUCKETS as u128 / u128::from(WINDOW_NS))
                    .min((GRAPH_BUCKETS - 1) as u128) as usize);
            let point = &mut result.graph[bucket];
            maximum(&mut point.interval_ns, Some(sample.interval_ns));
            maximum(&mut point.cpu_ns, sample.cpu_ns);
            maximum(&mut point.gpu_ns, sample.gpu_ns);
            point.observations |= sample.observations.0;
        }
        result
    }
    pub fn paint(&self, p: &mut Painter<'_>, width: f32, height: f32) {
        let panel = Rect::new(
            8.0,
            8.0,
            (width - 16.0).min(910.0),
            (height - 16.0).min(416.0),
        );
        p.rect(panel, 0xf0101419);
        p.border(panel, 0xff597b78);
        let previous_clip = p.clip;
        p.clip = panel;
        let x = panel.x + 8.0;
        let y = panel.y + 5.0;
        let mut text = TextBuffer::default();
        let _ = write!(
            text,
            "F3 · кадр {} · окно 10 с{}",
            self.frame,
            if self.complete_window {
                ""
            } else {
                " (неполное)"
            }
        );
        p.text(x, y, text.as_str(), 0xffa9dbc5);
        text.clear();
        let _ = write!(text, "Кадр ");
        timing(&mut text, self.measured.then_some(self.interval_ns));
        let _ = write!(text, " · CPU ");
        timing(&mut text, self.cpu_ns);
        let _ = write!(
            text,
            " · GPU {} ",
            if self.gpu_total_complete {
                "команды"
            } else {
                "часть"
            }
        );
        timing(&mut text, self.gpu_ns);
        p.text(x, y + 19.0, text.as_str(), 0xffe3e5df);
        let column = (panel.w - 16.0) / 3.0;
        for (index, label) in CPU_STAGES.iter().enumerate() {
            timed_line(
                p,
                x,
                y + 45.0 + index as f32 * 18.0,
                label,
                self.cpu_stages_ns[index],
                0xffc9ad82,
            );
        }
        for (index, label) in GPU_PASSES.iter().enumerate() {
            timed_line(
                p,
                x + column * (1 + index / 8) as f32,
                y + 45.0 + (index % 8) as f32 * 18.0,
                match index {
                    6 => "scene color",
                    13 => "Tone/AA",
                    _ => label,
                },
                self.gpu_passes_ns[index],
                0xff8bcbd9,
            );
        }
        let graph = Rect::new(x, y + 196.0, panel.w - 16.0, 60.0);
        p.rect(graph, 0xff171c20);
        // The graph scale is fixed at 50 ms, with a 16.67 ms reference line.
        p.rect(
            Rect::new(
                graph.x,
                graph.y + graph.h * (1.0 - 16.667 / 50.0),
                graph.w,
                1.0,
            ),
            0xff526365,
        );
        for (index, point) in self.graph.iter().enumerate() {
            let dx = graph.x + graph.w * index as f32 / GRAPH_BUCKETS as f32;
            let bar = (graph.w / GRAPH_BUCKETS as f32).max(1.0);
            for (duration, color, fraction) in [
                (point.interval_ns, 0xff798388, 1.0),
                (point.cpu_ns, 0xffc9ad82, 0.67),
                (point.gpu_ns, 0xff8bcbd9, 0.34),
            ] {
                if let Some(duration) = duration {
                    let h = (duration as f64 / 50_000_000.0).min(1.0) as f32 * graph.h;
                    p.rect(
                        Rect::new(dx, graph.y + graph.h - h, bar * fraction, h),
                        color,
                    );
                }
            }
            if point.observations != 0 {
                p.rect(Rect::new(dx, graph.y, bar, 2.0), 0xffe6b76f);
            }
        }
        p.text(
            x,
            y + 260.0,
            "10 с · серый: кадр · песок: CPU · голубой: GPU · шкала 50 мс",
            0xff9babae,
        );
        text.clear();
        let _ = write!(
            text,
            "Очередь {} · возраст {} тиков · в полёте {} · upload {} Б",
            self.queued_jobs, self.oldest_job_ticks, self.in_flight, self.work.upload_bytes
        );
        p.text(x, y + 280.0, text.as_str(), 0xffe3e5df);
        text.clear();
        let _ = write!(
            text,
            "Клетки {} · quads {} · pipelines {} · sync gen {}",
            self.work.cells_scanned,
            self.work.quads_emitted,
            self.work.pipeline_compilations,
            self.work.synchronous_generations
        );
        p.text(x, y + 298.0, text.as_str(), 0xffe3e5df);
        text.clear();
        if let Some(allocations) = self.allocations {
            let _ = write!(
                text,
                "Память: {} выделений / {} Б · освобождений {}",
                allocations.allocating_calls(),
                allocations.allocated_bytes,
                allocations.dealloc_calls
            );
        } else {
            let _ = write!(text, "Память: не измерено");
        }
        p.text(x, y + 316.0, text.as_str(), 0xffe3e5df);
        text.clear();
        let _ = write!(
            text,
            "Наблюдения рядом со скачком: 0x{:02x} · потери кадров {}/ввода {}",
            self.observations.0, self.loss.frames, self.loss.inputs
        );
        p.text(x, y + 336.0, text.as_str(), 0xffe6b76f);
        p.text(
            x,
            y + 354.0,
            "1 alloc · 2 pipeline · 4 sync gen · 8 upload · 16 старое задание",
            0xff9babae,
        );
        p.text(
            x,
            y + 372.0,
            "32 GPU wait · 64 light budget · 128 storage wait · причина не установлена",
            0xff9babae,
        );
        p.clip = previous_clip;
    }
}
fn timing(text: &mut TextBuffer, value: Option<u64>) {
    if let Some(ns) = value {
        let _ = write!(text, "{}.{:02} мс", ns / 1_000_000, ns % 1_000_000 / 10_000);
    } else {
        let _ = write!(text, "не измерено");
    }
}
fn timed_line(p: &mut Painter<'_>, x: f32, y: f32, label: &str, duration: Option<u64>, color: u32) {
    let mut text = TextBuffer::default();
    let _ = write!(text, "{label}: ");
    timing(&mut text, duration);
    p.text(x, y, text.as_str(), color);
}
