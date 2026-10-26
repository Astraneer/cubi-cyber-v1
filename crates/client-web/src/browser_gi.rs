//! Same-origin bounded full-source jobs; the worker receives no World owner.
use client::view::ClientView;
use jobs::{BrowserEvent, BrowserOutcome, BrowserWorkerPool, CancellationToken, JobId};
use std::collections::BTreeMap;
use wasm_bindgen::JsValue;

pub struct BrowserGi {
    workers: BrowserWorkerPool,
    running: BTreeMap<JobId, CancellationToken>,
    restarts: u8,
}
impl BrowserGi {
    pub fn new(worker: &str, module: &str) -> Result<Self, JsValue> {
        Ok(Self {
            workers: BrowserWorkerPool::new(worker, module, "execute_gi_task", 2, 8 * 1024 * 1024)?,
            running: BTreeMap::new(),
            restarts: 0,
        })
    }
    pub fn pump(&mut self, view: &mut ClientView) -> Result<(), JsValue> {
        let mut failed = false;
        for _ in 0..4 {
            let Some(event) = self.workers.try_recv() else {
                break;
            };
            match event {
                BrowserEvent::Completion { id, outcome } => {
                    self.running.remove(&id);
                    match outcome {
                        BrowserOutcome::Bytes(bytes) => view.complete_gi_source_bytes(id, &bytes),
                        BrowserOutcome::Cancelled => view.fail_gi_source_task(id),
                        BrowserOutcome::Failed(_) => {
                            failed = true;
                            view.fail_gi_source_task(id)
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
                return Err(JsValue::from_str("GI source worker repeatedly failed"));
            }
            self.workers.restart_failed()?;
        }
        for (&id, token) in &self.running {
            if token.is_cancelled() {
                self.workers.cancel(id)?;
            }
        }
        for _ in 0..2 {
            if self.running.len() >= 2 || self.workers.available_workers() == 0 {
                break;
            }
            let Some(dispatch) = view.take_gi_source_task().map_err(JsValue::from)? else {
                break;
            };
            let bytes = match view.encode_gi_source_task(&dispatch) {
                Ok(bytes) => bytes,
                Err(error) => {
                    view.fail_gi_source_task(dispatch.id)
                        .map_err(JsValue::from)?;
                    return Err(JsValue::from(error));
                }
            };
            match self
                .workers
                .try_submit(dispatch.id, &bytes, dispatch.spec.retained_bytes)
            {
                Ok(()) => {
                    self.running.insert(dispatch.id, dispatch.cancellation);
                }
                Err(_) => {
                    view.requeue_gi_source_task(dispatch)
                        .map_err(JsValue::from)?;
                    break;
                }
            }
        }
        Ok(())
    }
}
