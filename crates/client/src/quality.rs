//! Runtime quality decisions use completed scene frames and their full encoder
//! timestamps. Saved preferences and proposals awaiting a GPU receipt are kept
//! separate. Every container is reserved in the fixed-size owner below.
use telemetry::flight::Sample;
use ui::settings::{AutoQuality, Reduction, SCHEMA, Section, Setting, Settings};

const PENDING_FRAMES: usize = 64;
const WINDOW_FRAMES: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QualityRequest {
    pub id: u64,
    pub reduction: Reduction,
    pub restoring: bool,
    pub target: Settings,
}
struct Proposal {
    request: QualityRequest,
    quality: AutoQuality,
    baseline_gpu_us: u32,
}
#[derive(Clone, Copy, Default)]
struct PendingFrame {
    frame: u64,
    time_ns: u64,
    interval_us: u32,
    present: bool,
}
#[derive(Clone, Copy, Default)]
struct MeasuredFrame {
    interval_us: u32,
    gpu_us: u32,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QualityStats {
    pub complete_frames: u64,
    pub incomplete_frames: u64,
    pub lost_frames: u64,
    pub windows: u64,
    pub proposals: u64,
    pub applied: u64,
    pub failed: u64,
    pub measured_savings: u64,
    pub frame_p95_us: u32,
    pub gpu_p95_us: u32,
}
struct SavingMeasurement {
    before_gpu_us: u32,
    after_gpu_us: [u32; 3],
    windows: usize,
}

pub struct QualityRuntime {
    saved: Settings,
    applied: AutoQuality,
    target: Settings,
    proposal: Option<Proposal>,
    next_id: u64,
    active: bool,
    pending_frames: [PendingFrame; PENDING_FRAMES],
    measured: [MeasuredFrame; WINDOW_FRAMES],
    measured_len: usize,
    window_start_ns: Option<u64>,
    window_end_ns: u64,
    measurement: Option<SavingMeasurement>,
    stats: QualityStats,
}
impl QualityRuntime {
    pub fn new(saved: &Settings) -> Self {
        Self {
            saved: saved.clone(),
            applied: AutoQuality::new(saved),
            target: saved.clone(),
            proposal: None,
            next_id: 0,
            active: false,
            pending_frames: [PendingFrame::default(); PENDING_FRAMES],
            measured: [MeasuredFrame::default(); WINDOW_FRAMES],
            measured_len: 0,
            window_start_ns: None,
            window_end_ns: 0,
            measurement: None,
            stats: QualityStats::default(),
        }
    }
    pub fn target(&self) -> &Settings {
        &self.target
    }
    pub fn confirmed(&self, setting: Setting) -> i32 {
        if SCHEMA[setting as usize].section == Section::Graphics {
            self.applied.effective(setting)
        } else {
            self.saved.get(setting)
        }
    }
    pub fn reductions(&self) -> impl Iterator<Item = &Reduction> {
        self.applied.reductions()
    }
    pub fn pending(&self) -> Option<&QualityRequest> {
        self.proposal.as_ref().map(|proposal| &proposal.request)
    }
    pub fn stats(&self) -> QualityStats {
        self.stats
    }
    /// A graphics edit invalidates outstanding proposals. Other sections keep
    /// temporary graphics choices while inheriting the user's new preferences.
    pub fn update_saved(&mut self, saved: &Settings, now_ms: u64) {
        if self.saved == *saved {
            return;
        }
        let graphics_changed = self.saved.preset != saved.preset
            || SCHEMA.iter().any(|definition| {
                definition.section == Section::Graphics
                    && self.saved.get(definition.id) != saved.get(definition.id)
            });
        self.saved = saved.clone();
        if !graphics_changed {
            self.target = if let Some(proposal) = &mut self.proposal {
                proposal.request.target = proposal.quality.effective_settings(saved);
                proposal.request.target.clone()
            } else {
                self.applied.effective_settings(saved)
            };
            self.clear_window();
            return;
        }
        self.target = saved.clone();
        self.applied.reset(saved);
        self.applied.acknowledged_at(now_ms);
        self.proposal = None;
        self.measurement = None;
        self.clear_window();
    }
    /// Menus, loading, paused frames and benchmark routes do not provide
    /// performance evidence. Pending resource preparation may still be ACKed.
    pub fn set_active(&mut self, active: bool, now_ms: u64) {
        if self.active != active {
            self.active = active;
            self.clear_window();
            self.applied.acknowledged_at(now_ms);
        }
    }
    fn clear_window(&mut self) {
        self.pending_frames.fill(PendingFrame::default());
        self.measured_len = 0;
        self.window_start_ns = None;
        self.window_end_ns = 0;
    }
    /// Record only actual presentations, using the same frame identity as the
    /// flight recorder. Event-loop CPU continuations are not presentations.
    pub fn record_present(&mut self, sample: &Sample) {
        if !self.active || self.proposal.is_some() || sample.interval_ns == 0 {
            return;
        }
        let slot = &mut self.pending_frames[sample.frame as usize % PENDING_FRAMES];
        if slot.present {
            self.stats.lost_frames = self.stats.lost_frames.saturating_add(1);
        }
        *slot = PendingFrame {
            frame: sample.frame,
            time_ns: sample.monotonic_ns,
            interval_us: microseconds(sample.interval_ns),
            present: true,
        };
    }
    /// Called by FrameGpuCapture::collect_with. A partial sum or a stale callback
    /// cannot become GPU headroom or a measured saving.
    pub fn complete_gpu(&mut self, frame: u64, gpu_ns: u64, complete: bool) {
        let slot = &mut self.pending_frames[frame as usize % PENDING_FRAMES];
        if !slot.present || slot.frame != frame {
            return;
        }
        let pending = *slot;
        slot.present = false;
        if !self.active || self.proposal.is_some() || !complete || gpu_ns == 0 {
            self.stats.incomplete_frames = self.stats.incomplete_frames.saturating_add(1);
            return;
        }
        if self.measured_len == WINDOW_FRAMES {
            self.stats.lost_frames = self.stats.lost_frames.saturating_add(1);
            return;
        }
        self.window_start_ns = Some(
            self.window_start_ns
                .map_or(pending.time_ns, |start| start.min(pending.time_ns)),
        );
        self.window_end_ns = self.window_end_ns.max(pending.time_ns);
        self.measured[self.measured_len] = MeasuredFrame {
            interval_us: pending.interval_us,
            gpu_us: microseconds(gpu_ns),
        };
        self.measured_len += 1;
        self.stats.complete_frames = self.stats.complete_frames.saturating_add(1);
    }
    /// At least eight completed frames and one elapsed second form an evidence
    /// window. Slower rendering extends the window; it cannot fabricate frames.
    /// A backend supplies the mask of levers it can actually prepare and apply.
    pub fn poll(&mut self, now_ms: u64, supported: u32) -> Option<QualityRequest> {
        if !self.active
            || self.proposal.is_some()
            || self.measured_len < 8
            || self
                .window_start_ns
                .is_none_or(|start| self.window_end_ns.saturating_sub(start) < 1_000_000_000)
        {
            return None;
        }
        let mut frame_us = [0; WINDOW_FRAMES];
        let mut gpu_us = [0; WINDOW_FRAMES];
        let len = self.measured_len;
        for (index, sample) in self.measured[..len].iter().enumerate() {
            frame_us[index] = sample.interval_us;
            gpu_us[index] = sample.gpu_us;
        }
        frame_us[..len].sort_unstable();
        gpu_us[..len].sort_unstable();
        let percentile = (len * 95).div_ceil(100) - 1;
        let frame_p95 = frame_us[percentile];
        let gpu_p95 = gpu_us[percentile];
        self.clear_window();
        self.stats.windows = self.stats.windows.saturating_add(1);
        self.stats.frame_p95_us = frame_p95;
        self.stats.gpu_p95_us = gpu_p95;
        if let Some(measurement) = &mut self.measurement {
            measurement.after_gpu_us[measurement.windows] = gpu_p95;
            measurement.windows += 1;
            if measurement.windows == 3 {
                measurement.after_gpu_us.sort_unstable();
                let saving = measurement
                    .before_gpu_us
                    .saturating_sub(measurement.after_gpu_us[1]);
                self.applied.measured_saving(saving);
                self.measurement = None;
                self.stats.measured_savings = self.stats.measured_savings.saturating_add(1);
            }
        }
        let mut trial = self.applied.clone();
        let reduction = trial.observe_supported(
            &self.saved,
            now_ms,
            len as u16,
            frame_p95,
            16667_u32.saturating_sub(gpu_p95),
            supported,
        );
        let Some(reduction) = reduction else {
            // Slow/fast window hysteresis must advance even without a change.
            self.applied = trial;
            return None;
        };
        let id = self.next_id.checked_add(1)?;
        self.next_id = id;
        let request = QualityRequest {
            id,
            reduction,
            restoring: trial.reductions().count() < self.applied.reductions().count(),
            target: trial.effective_settings(&self.saved),
        };
        self.target = request.target.clone();
        self.proposal = Some(Proposal {
            request: request.clone(),
            quality: trial,
            baseline_gpu_us: gpu_p95,
        });
        self.stats.proposals = self.stats.proposals.saturating_add(1);
        Some(request)
    }
    /// The caller checks its current request ID and the backend's actual value
    /// receipt. A saved menu value alone is insufficient acknowledgement.
    pub fn acknowledge(&mut self, id: u64, now_ms: u64, applied_mask: u32) -> bool {
        let Some(proposal) = &self.proposal else {
            return false;
        };
        if proposal.request.id != id
            || applied_mask & (1 << proposal.request.reduction.setting as u32) == 0
        {
            return false;
        }
        let proposal = self.proposal.take().expect("validated proposal");
        self.applied = proposal.quality;
        self.applied.acknowledged_at(now_ms);
        self.measurement = (!proposal.request.restoring).then_some(SavingMeasurement {
            before_gpu_us: proposal.baseline_gpu_us,
            after_gpu_us: [0; 3],
            windows: 0,
        });
        self.clear_window();
        self.stats.applied = self.stats.applied.saturating_add(1);
        true
    }
    pub fn failed(&mut self, id: u64, now_ms: u64) -> bool {
        if self.pending().is_none_or(|request| request.id != id) {
            return false;
        }
        self.proposal = None;
        self.target = self.applied.effective_settings(&self.saved);
        self.applied.acknowledged_at(now_ms);
        self.clear_window();
        self.stats.failed = self.stats.failed.saturating_add(1);
        true
    }
}
fn microseconds(ns: u64) -> u32 {
    ns.div_ceil(1000).min(u32::MAX as u64) as u32
}
