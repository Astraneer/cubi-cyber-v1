//! Time-windowed, allocation-free-after-startup recorder. Exports are local and
//! contain explicit public replay inputs; credentials/private seeds have no field.
use crate::{AllocationCounts, WorkCounts};
use std::{collections::VecDeque, fmt::Write};
pub const WINDOW_NS: u64 = 10_000_000_000;
pub const CPU_STAGES: [&str; 8] = [
    "input",
    "simulation",
    "jobs",
    "mesh",
    "upload",
    "encode",
    "present",
    "other",
];
pub const GPU_PASSES: [&str; 16] = [
    "frustum",
    "depth",
    "hiz",
    "occlusion",
    "shadows",
    "sky",
    "opaque",
    "cutout",
    "water",
    "clouds",
    "gi",
    "exposure",
    "bloom",
    "tonemap",
    "models",
    "ui",
];
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Observations(pub u32);
impl Observations {
    pub const ALLOCATION: u32 = 1;
    pub const PIPELINE_COMPILATION: u32 = 2;
    pub const SYNC_GENERATION: u32 = 4;
    pub const UPLOAD_BUDGET: u32 = 8;
    pub const OLD_JOB: u32 = 16;
    pub const GPU_WAIT: u32 = 32;
    pub const LIGHT_BUDGET: u32 = 64;
    pub const STORAGE_WAIT: u32 = 128;
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sample {
    pub frame: u64,
    pub monotonic_ns: u64,
    pub interval_ns: u64,
    pub cpu_ns: Option<u64>,
    pub gpu_ns: Option<u64>,
    pub cpu_stages_ns: [Option<u64>; 8],
    pub gpu_passes_ns: [Option<u64>; 16],
    pub work: WorkCounts,
    pub allocations: Option<AllocationCounts>,
    pub observations: Observations,
    pub oldest_job_ticks: u64,
    pub queued_jobs: u32,
    pub in_flight: u8,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReplayInput {
    pub monotonic_ns: u64,
    pub tick: u64,
    pub sequence: u64,
    pub buttons: u32,
    pub yaw_bits: u32,
    pub pitch_bits: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordError {
    ClockWentBackwards,
    OutOfOrder,
    InvalidInput,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Loss {
    pub frames: u64,
    pub inputs: u64,
}
pub struct FlightRecorder {
    frames: VecDeque<Sample>,
    inputs: VecDeque<ReplayInput>,
    frame_limit: usize,
    input_limit: usize,
    loss: Loss,
    last_capacity_loss_ns: Option<u64>,
    started_ns: Option<u64>,
    last_ns: u64,
}
impl FlightRecorder {
    pub fn new(frame_limit: usize, input_limit: usize) -> Result<Self, &'static str> {
        if !(2..=65536).contains(&frame_limit) || !(2..=65536).contains(&input_limit) {
            return Err("recorder capacities outside 2..65536");
        }
        Ok(Self {
            frames: VecDeque::with_capacity(frame_limit),
            inputs: VecDeque::with_capacity(input_limit),
            frame_limit,
            input_limit,
            loss: Loss::default(),
            last_capacity_loss_ns: None,
            started_ns: None,
            last_ns: 0,
        })
    }
    pub fn frames(&self) -> impl ExactSizeIterator<Item = &Sample> {
        self.frames.iter()
    }
    pub fn inputs(&self) -> impl ExactSizeIterator<Item = &ReplayInput> {
        self.inputs.iter()
    }
    pub fn loss(&self) -> Loss {
        self.loss
    }
    pub fn complete_window(&self) -> bool {
        self.started_ns
            .is_some_and(|start| self.last_ns.saturating_sub(start) >= WINDOW_NS)
            && self
                .last_capacity_loss_ns
                .is_none_or(|loss| self.last_ns.saturating_sub(loss) > WINDOW_NS)
    }
    fn check_time(&self, time: u64) -> Result<(), RecordError> {
        if time < self.last_ns {
            Err(RecordError::ClockWentBackwards)
        } else {
            Ok(())
        }
    }
    fn advance(&mut self, time: u64) {
        self.started_ns.get_or_insert(time);
        self.last_ns = time;
        let threshold = time.saturating_sub(WINDOW_NS);
        while self
            .frames
            .front()
            .is_some_and(|frame| frame.monotonic_ns < threshold)
        {
            self.frames.pop_front();
        }
        while self
            .inputs
            .front()
            .is_some_and(|input| input.monotonic_ns < threshold)
        {
            self.inputs.pop_front();
        }
    }
    pub fn record_frame(&mut self, mut sample: Sample) -> Result<(), RecordError> {
        self.check_time(sample.monotonic_ns)?;
        if self
            .frames
            .back()
            .is_some_and(|old| old.frame >= sample.frame)
        {
            return Err(RecordError::OutOfOrder);
        }
        if sample.in_flight > 2 {
            return Err(RecordError::InvalidInput);
        }
        if sample.work.pipeline_compilations > 0 {
            sample.observations.0 |= Observations::PIPELINE_COMPILATION;
        }
        if sample.work.synchronous_generations > 0 {
            sample.observations.0 |= Observations::SYNC_GENERATION;
        }
        if sample.allocations.is_some_and(|a| a.allocating_calls() > 0) {
            sample.observations.0 |= Observations::ALLOCATION;
        }
        self.advance(sample.monotonic_ns);
        if self.frames.len() == self.frame_limit {
            self.frames.pop_front();
            self.loss.frames = self.loss.frames.saturating_add(1);
            self.last_capacity_loss_ns = Some(sample.monotonic_ns);
        }
        self.frames.push_back(sample);
        Ok(())
    }
    pub fn record_input(&mut self, input: ReplayInput) -> Result<(), RecordError> {
        self.check_time(input.monotonic_ns)?;
        if !f32::from_bits(input.yaw_bits).is_finite()
            || !f32::from_bits(input.pitch_bits).is_finite()
            || input.buttons & !0xffff != 0
        {
            return Err(RecordError::InvalidInput);
        }
        if self
            .inputs
            .back()
            .is_some_and(|old| old.sequence >= input.sequence || old.tick > input.tick)
        {
            return Err(RecordError::OutOfOrder);
        }
        self.advance(input.monotonic_ns);
        if self.inputs.len() == self.input_limit {
            self.inputs.pop_front();
            self.loss.inputs = self.loss.inputs.saturating_add(1);
            self.last_capacity_loss_ns = Some(input.monotonic_ns);
        }
        self.inputs.push_back(input);
        Ok(())
    }
    /// Local export is intentionally outside the frame loop and may allocate.
    /// Unknown measurements remain JSON null. Observations are concurrent events,
    /// not an assertion that an allocation caused a measured hitch.
    pub fn export_json(
        &self,
        backend: &str,
        public_seed: u64,
        initial_hash: u64,
        start_tick: u64,
    ) -> String {
        let mut out =
            String::with_capacity(self.frames.len() * 400 + self.inputs.len() * 100 + 512);
        let _ = write!(out, "{{\"schemaVersion\":1,\"backend\":");
        quote(&mut out, backend);
        let _ = write!(
            out,
            ",\"windowNs\":{WINDOW_NS},\"completeWindow\":{},\"lostFrames\":{},\"lostInputs\":{},\"publicSeed\":\"{public_seed:016x}\",\"initialHash\":\"{initial_hash:016x}\",\"startTick\":{start_tick},\"simulationHz\":20,\"cpuStages\":[",
            self.complete_window(),
            self.loss.frames,
            self.loss.inputs
        );
        labels(&mut out, &CPU_STAGES);
        out.push_str("],\"gpuPasses\":[");
        labels(&mut out, &GPU_PASSES);
        out.push_str("],\"frames\":[");
        for (i, s) in self.frames.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            let _ = write!(
                out,
                "{{\"frame\":{},\"timeNs\":{},\"intervalNs\":{},\"cpuNs\":",
                s.frame, s.monotonic_ns, s.interval_ns
            );
            optional(&mut out, s.cpu_ns);
            out.push_str(",\"gpuNs\":");
            optional(&mut out, s.gpu_ns);
            out.push_str(",\"cpu\":[");
            numbers(&mut out, &s.cpu_stages_ns);
            out.push_str("],\"gpu\":[");
            numbers(&mut out, &s.gpu_passes_ns);
            let _ = write!(
                out,
                "],\"observations\":{},\"work\":{{\"cells\":{},\"quads\":{},\"uploads\":{},\"pipelines\":{},\"syncGeneration\":{}}},\"queuedJobs\":{},\"oldestJobTicks\":{},\"inFlight\":{},\"allocations\":",
                s.observations.0,
                s.work.cells_scanned,
                s.work.quads_emitted,
                s.work.upload_bytes,
                s.work.pipeline_compilations,
                s.work.synchronous_generations,
                s.queued_jobs,
                s.oldest_job_ticks,
                s.in_flight
            );
            if let Some(a) = s.allocations {
                let _ = write!(
                    out,
                    "{{\"alloc\":{},\"realloc\":{},\"dealloc\":{},\"allocatedBytes\":{}}}",
                    a.alloc_calls.saturating_add(a.zeroed_alloc_calls),
                    a.realloc_calls,
                    a.dealloc_calls,
                    a.allocated_bytes
                );
            } else {
                out.push_str("null");
            }
            out.push('}');
        }
        out.push_str("],\"inputs\":[");
        for (i, s) in self.inputs.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            let _ = write!(
                out,
                "[{}, {}, {}, {}, {}, {}]",
                s.monotonic_ns, s.tick, s.sequence, s.buttons, s.yaw_bits, s.pitch_bits
            );
        }
        out.push_str("]}");
        out
    }
}
fn optional(out: &mut String, value: Option<u64>) {
    if let Some(value) = value {
        let _ = write!(out, "{value}");
    } else {
        out.push_str("null");
    }
}
fn labels(out: &mut String, values: &[&str]) {
    for (i, value) in values.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        quote(out, value);
    }
}
fn numbers(out: &mut String, values: &[Option<u64>]) {
    for (i, value) in values.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        optional(out, *value);
    }
}
fn quote(out: &mut String, value: &str) {
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c <= '\u{1f}' => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}
