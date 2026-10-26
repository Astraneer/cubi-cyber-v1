//! A separate bounded worker transport: scene and horizon JobIds never share a
//! pool. All planning, validation and publication remain in shared Rust.
use client::view::{ClientView, horizon};
use jobs::{BrowserEvent, BrowserOutcome, BrowserWorkerPool, CancellationToken, JobId};
use std::collections::BTreeMap;
use wasm_bindgen::JsValue;

pub struct BrowserHorizon {
    workers: BrowserWorkerPool,
    running: BTreeMap<JobId, CancellationToken>,
    restarts: u8,
}
impl BrowserHorizon {
    pub fn new(worker: &str, module: &str) -> Result<Self, JsValue> {
        Ok(Self {
            workers: BrowserWorkerPool::new(
                worker,
                module,
                "execute_horizon_task",
                1,
                16 * 1024 * 1024,
            )?,
            running: BTreeMap::new(),
            restarts: 0,
        })
    }
    pub fn pump(&mut self, view: &mut ClientView) -> Result<(), JsValue> {
        let mut failed = false;
        for _ in 0..8 {
            let Some(event) = self.workers.try_recv() else {
                break;
            };
            match event {
                BrowserEvent::Completion { id, outcome } => {
                    self.running.remove(&id);
                    match outcome {
                        BrowserOutcome::Bytes(bytes) => view.complete_horizon_bytes(id, &bytes),
                        BrowserOutcome::Cancelled => view.complete_horizon_task(jobs::Completion {
                            id,
                            outcome: jobs::JobOutcome::Cancelled,
                        }),
                        BrowserOutcome::Failed(_) => {
                            failed = true;
                            view.fail_horizon_task(id)
                        }
                    }
                    .map_err(JsValue::from)?;
                }
                BrowserEvent::WorkerFailed { .. } => failed = true,
            }
        }
        if failed {
            self.restarts = self.restarts.saturating_add(1);
            if self.restarts > 3 {
                return Err(JsValue::from_str("horizon worker repeatedly failed"));
            }
            self.workers.restart_failed()?;
        }
        for (&id, token) in &self.running {
            if token.is_cancelled() {
                self.workers.cancel(id)?;
            }
        }
        for _ in 0..8 {
            if self.running.len() >= 8 || self.workers.available_workers() == 0 {
                break;
            }
            let Some(dispatch) = view.take_horizon_task() else {
                break;
            };
            let bytes = horizon::encode_background_task(&dispatch.payload)
                .map_err(|error| JsValue::from_str(&error.to_string()))?;
            match self
                .workers
                .try_submit(dispatch.id, &bytes, dispatch.spec.retained_bytes)
            {
                Ok(()) => {
                    self.running.insert(dispatch.id, dispatch.cancellation);
                }
                Err(_) => {
                    view.requeue_horizon_task(dispatch).map_err(JsValue::from)?;
                    break;
                }
            }
        }
        Ok(())
    }
}
