//! Browser snapshot worker transport. Scheduling and publication stay in Rust.
use crate::JobId;
use js_sys::{Array, ArrayBuffer, Int32Array, Object, Reflect, SharedArrayBuffer, Uint8Array};
use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
    rc::Rc,
};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use web_sys::{ErrorEvent, Event, MessageEvent, Worker, WorkerOptions, WorkerType};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowserTransport {
    Shared,
    Transfer,
}

#[derive(Debug)]
pub enum BrowserOutcome {
    Bytes(Vec<u8>),
    Cancelled,
    Failed(String),
}

#[derive(Debug)]
pub enum BrowserEvent {
    Completion { id: JobId, outcome: BrowserOutcome },
    WorkerFailed { worker: usize, message: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowserSubmitError {
    NoReadyWorker,
    ByteLimit,
    DuplicateId,
    InvalidId,
}

#[derive(Clone, Copy, Debug)]
struct Slot {
    generation: u64,
    ready: bool,
    active: Option<JobId>,
    completed: bool,
    failed: bool,
    failure_reported: bool,
}

struct Pending {
    worker: usize,
    retained_bytes: usize,
    output_limit: usize,
    control: Option<Int32Array>,
    cancelled: bool,
}

struct State {
    slots: Vec<Slot>,
    pending: BTreeMap<JobId, Pending>,
    events: VecDeque<BrowserEvent>,
    retained_bytes: usize,
}

impl State {
    fn complete(&mut self, index: usize, outcome: BrowserOutcome) {
        let slot = &mut self.slots[index];
        if slot.completed {
            return;
        }
        let Some(id) = slot.active else {
            return;
        };
        let Some(pending) = self.pending.get(&id) else {
            return;
        };
        let outcome = if pending.cancelled {
            BrowserOutcome::Cancelled
        } else {
            outcome
        };
        slot.completed = true;
        self.events
            .push_back(BrowserEvent::Completion { id, outcome });
    }

    fn fail(&mut self, index: usize, message: String) {
        let slot = &mut self.slots[index];
        slot.failed = true;
        slot.ready = false;
        if slot.active.is_some() {
            self.complete(index, BrowserOutcome::Failed(message));
        } else if !slot.failure_reported {
            slot.failure_reported = true;
            self.events.push_back(BrowserEvent::WorkerFailed {
                worker: index,
                message,
            });
        }
    }
}

struct WorkerHandle {
    worker: Worker,
    _message: Closure<dyn FnMut(MessageEvent)>,
    _error: Closure<dyn FnMut(ErrorEvent)>,
    _message_error: Closure<dyn FnMut(Event)>,
}

impl Drop for WorkerHandle {
    fn drop(&mut self) {
        self.worker.set_onmessage(None);
        self.worker.set_onerror(None);
        self.worker.set_onmessageerror(None);
        self.worker.terminate();
    }
}

/// Actual browser module workers. No work executes on the rendering thread.
///
/// Workers load a same-origin WASM/JS module and call its named snapshot export.
/// The export receives `(Uint8Array, Int32Array | null)`, returning a Uint8Array
/// (or a Promise thereof). In shared mode the control array's first atomic word
/// is a cancellation flag. Transfer mode cancels by replacing the owned worker.
///
/// Completed jobs retain their slot and declared byte quota until collected, so
/// an owner that stops polling cannot create an unbounded result backlog.
pub struct BrowserWorkerPool {
    handles: Vec<WorkerHandle>,
    state: Rc<RefCell<State>>,
    transport: BrowserTransport,
    worker_url: String,
    module_url: String,
    export_name: String,
    max_bytes: usize,
}

impl BrowserWorkerPool {
    pub fn new(
        worker_url: &str,
        module_url: &str,
        export_name: &str,
        workers: usize,
        max_bytes: usize,
    ) -> Result<Self, JsValue> {
        if workers == 0 || max_bytes == 0 || max_bytes > u32::MAX as usize {
            return Err(JsValue::from_str("invalid browser worker capacity"));
        }
        let global = js_sys::global();
        let isolated = Reflect::get(&global, &"crossOriginIsolated".into())
            .ok()
            .and_then(|value| value.as_bool())
            == Some(true);
        let shared_available = Reflect::get(&global, &"SharedArrayBuffer".into())
            .is_ok_and(|value| value.is_function());
        let transport = if isolated && shared_available {
            BrowserTransport::Shared
        } else {
            BrowserTransport::Transfer
        };
        let state = Rc::new(RefCell::new(State {
            slots: vec![
                Slot {
                    generation: 0,
                    ready: false,
                    active: None,
                    completed: false,
                    failed: false,
                    failure_reported: false
                };
                workers
            ],
            pending: BTreeMap::new(),
            events: VecDeque::with_capacity(workers),
            retained_bytes: 0,
        }));
        let mut pool = Self {
            handles: Vec::with_capacity(workers),
            state,
            transport,
            worker_url: worker_url.to_owned(),
            module_url: module_url.to_owned(),
            export_name: export_name.to_owned(),
            max_bytes,
        };
        for index in 0..workers {
            pool.handles.push(pool.create_worker(index)?);
        }
        Ok(pool)
    }

    pub const fn transport(&self) -> BrowserTransport {
        self.transport
    }
    pub fn retained_bytes(&self) -> usize {
        self.state.borrow().retained_bytes
    }
    pub fn pending_len(&self) -> usize {
        self.state.borrow().pending.len()
    }
    pub fn completed_len(&self) -> usize {
        self.state.borrow().events.len()
    }
    pub fn available_workers(&self) -> usize {
        self.state
            .borrow()
            .slots
            .iter()
            .filter(|slot| slot.ready && !slot.failed && slot.active.is_none())
            .count()
    }

    /// `retained_bytes` must budget the request, expected output and kernel scratch.
    pub fn try_submit(
        &mut self,
        id: JobId,
        payload: &[u8],
        retained_bytes: usize,
    ) -> Result<(), BrowserSubmitError> {
        if id.0 == 0 {
            return Err(BrowserSubmitError::InvalidId);
        }
        let mut state = self.state.borrow_mut();
        if state.pending.contains_key(&id) {
            return Err(BrowserSubmitError::DuplicateId);
        }
        if retained_bytes < payload.len()
            || retained_bytes > self.max_bytes.saturating_sub(state.retained_bytes)
        {
            return Err(BrowserSubmitError::ByteLimit);
        }
        let Some(index) = state
            .slots
            .iter()
            .position(|slot| slot.ready && !slot.failed && slot.active.is_none())
        else {
            return Err(BrowserSubmitError::NoReadyWorker);
        };
        let output_limit = retained_bytes - payload.len();
        let request = Object::new();
        let transfer = Array::new();
        let control = match self.transport {
            BrowserTransport::Shared => {
                let data = SharedArrayBuffer::new(payload.len() as u32);
                Uint8Array::new(&data).copy_from(payload);
                let control = Int32Array::new(&SharedArrayBuffer::new(4));
                set(&request, "payload", &data).map_err(|_| BrowserSubmitError::ByteLimit)?;
                set(&request, "control", &control.buffer())
                    .map_err(|_| BrowserSubmitError::ByteLimit)?;
                Some(control)
            }
            BrowserTransport::Transfer => {
                let data = Uint8Array::new_with_length(payload.len() as u32);
                data.copy_from(payload);
                set(&request, "payload", &data.buffer())
                    .map_err(|_| BrowserSubmitError::ByteLimit)?;
                set(&request, "control", &JsValue::NULL)
                    .map_err(|_| BrowserSubmitError::ByteLimit)?;
                transfer.push(&data.buffer());
                None
            }
        };
        set(&request, "type", &"execute".into()).map_err(|_| BrowserSubmitError::InvalidId)?;
        set(&request, "id", &id.0.to_string().into()).map_err(|_| BrowserSubmitError::InvalidId)?;
        set(
            &request,
            "transport",
            &match self.transport {
                BrowserTransport::Shared => "shared",
                BrowserTransport::Transfer => "transfer",
            }
            .into(),
        )
        .map_err(|_| BrowserSubmitError::InvalidId)?;
        set(
            &request,
            "outputLimit",
            &JsValue::from_f64(output_limit as f64),
        )
        .map_err(|_| BrowserSubmitError::ByteLimit)?;
        state.slots[index].active = Some(id);
        state.slots[index].completed = false;
        state.retained_bytes += retained_bytes;
        state.pending.insert(
            id,
            Pending {
                worker: index,
                retained_bytes,
                output_limit,
                control,
                cancelled: false,
            },
        );
        if self.handles[index]
            .worker
            .post_message_with_transfer(&request, &transfer)
            .is_err()
        {
            state.fail(index, "posting snapshot to worker failed".to_owned());
        }
        Ok(())
    }

    pub fn try_recv(&mut self) -> Option<BrowserEvent> {
        let mut state = self.state.borrow_mut();
        let event = state.events.pop_front()?;
        if let BrowserEvent::Completion { id, .. } = &event
            && let Some(pending) = state.pending.remove(id)
        {
            state.retained_bytes -= pending.retained_bytes;
            let slot = &mut state.slots[pending.worker];
            if slot.active == Some(*id) {
                slot.active = None;
                slot.completed = false;
            }
        }
        Some(event)
    }

    /// Shared workers use a cooperative atomic flag; fallback workers are restarted.
    pub fn cancel(&mut self, id: JobId) -> Result<bool, JsValue> {
        let (index, restart) = {
            let mut state = self.state.borrow_mut();
            let Some(pending) = state.pending.get_mut(&id) else {
                return Ok(false);
            };
            pending.cancelled = true;
            let index = pending.worker;
            let shared = if let Some(control) = &pending.control {
                js_sys::Atomics::store(control, 0, 1)?;
                true
            } else {
                false
            };
            if state.slots[index].completed {
                for event in &mut state.events {
                    if let BrowserEvent::Completion {
                        id: queued,
                        outcome,
                    } = event
                        && *queued == id
                    {
                        *outcome = BrowserOutcome::Cancelled;
                    }
                }
                return Ok(true);
            }
            if shared {
                (index, false)
            } else {
                state.complete(index, BrowserOutcome::Cancelled);
                (index, true)
            }
        };
        if restart {
            self.restart_worker(index)?;
        }
        Ok(true)
    }

    /// Restart only this pool's failed workers. Existing completions are still collected.
    pub fn restart_failed(&mut self) -> Result<usize, JsValue> {
        let state = self.state.borrow();
        let failed: Vec<_> = state.slots
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| {
                let pending_notification = state.events.iter().any(|event| matches!(event, BrowserEvent::WorkerFailed { worker, .. } if *worker == index));
                (slot.failed && !pending_notification).then_some(index)
            })
            .collect();
        drop(state);
        for index in &failed {
            self.restart_worker(*index)?;
        }
        Ok(failed.len())
    }

    fn restart_worker(&mut self, index: usize) -> Result<(), JsValue> {
        self.handles[index].worker.terminate();
        self.handles[index].worker.set_onmessage(None);
        self.handles[index].worker.set_onerror(None);
        self.handles[index].worker.set_onmessageerror(None);
        {
            let mut state = self.state.borrow_mut();
            let slot = &mut state.slots[index];
            slot.generation = slot
                .generation
                .checked_add(1)
                .ok_or("worker generation exhausted")?;
            slot.ready = false;
            slot.failed = false;
            slot.failure_reported = false;
        }
        match self.create_worker(index) {
            Ok(worker) => {
                self.handles[index] = worker;
                Ok(())
            }
            Err(error) => {
                self.state
                    .borrow_mut()
                    .fail(index, "worker restart failed".to_owned());
                Err(error)
            }
        }
    }

    fn create_worker(&self, index: usize) -> Result<WorkerHandle, JsValue> {
        let options = WorkerOptions::new();
        options.set_type(WorkerType::Module);
        options.set_name(&format!("cubi-snapshot-{index}"));
        let worker = Worker::new_with_options(&self.worker_url, &options)?;
        let generation = self.state.borrow().slots[index].generation;
        let state = self.state.clone();
        let message = Closure::wrap(Box::new(move |event: MessageEvent| {
            let mut state = state.borrow_mut();
            if state.slots[index].generation != generation {
                return;
            }
            handle_message(&mut state, index, event.data());
        }) as Box<dyn FnMut(MessageEvent)>);
        worker.set_onmessage(Some(message.as_ref().unchecked_ref()));
        let state = self.state.clone();
        let error = Closure::wrap(Box::new(move |event: ErrorEvent| {
            event.prevent_default();
            let mut state = state.borrow_mut();
            if state.slots[index].generation == generation {
                state.fail(index, format!("worker error: {}", event.message()));
            }
        }) as Box<dyn FnMut(ErrorEvent)>);
        worker.set_onerror(Some(error.as_ref().unchecked_ref()));
        let state = self.state.clone();
        let message_error = Closure::wrap(Box::new(move |_: Event| {
            let mut state = state.borrow_mut();
            if state.slots[index].generation == generation {
                state.fail(index, "worker message could not be decoded".to_owned());
            }
        }) as Box<dyn FnMut(Event)>);
        worker.set_onmessageerror(Some(message_error.as_ref().unchecked_ref()));
        let handle = WorkerHandle {
            worker,
            _message: message,
            _error: error,
            _message_error: message_error,
        };
        let init = Object::new();
        set(&init, "type", &"init".into())?;
        set(&init, "module", &self.module_url.clone().into())?;
        set(&init, "export", &self.export_name.clone().into())?;
        handle.worker.post_message(&init)?;
        Ok(handle)
    }
}

fn set(object: &Object, name: &str, value: &JsValue) -> Result<(), JsValue> {
    Reflect::set(object, &name.into(), value).map(|_| ())
}

fn property(data: &JsValue, name: &str) -> Option<JsValue> {
    Reflect::get(data, &name.into()).ok()
}

fn handle_message(state: &mut State, index: usize, data: JsValue) {
    let kind = property(&data, "type").and_then(|value| value.as_string());
    if kind.as_deref() == Some("ready") {
        if !state.slots[index].failed {
            state.slots[index].ready = true;
        }
        return;
    }
    let id = property(&data, "id")
        .and_then(|value| value.as_string())
        .and_then(|id| id.parse::<u64>().ok())
        .map(JobId);
    if id.is_none() && kind.as_deref() == Some("error") {
        state.fail(
            index,
            property(&data, "error")
                .and_then(|value| value.as_string())
                .unwrap_or_else(|| "worker initialization failed".to_owned()),
        );
        return;
    }
    if id != state.slots[index].active {
        state.fail(index, "worker returned an unknown job identity".to_owned());
        return;
    }
    let Some(id) = id else {
        state.fail(index, "worker returned a malformed job identity".to_owned());
        return;
    };
    match kind.as_deref() {
        Some("result") => {
            let Some(payload) = property(&data, "payload") else {
                state.fail(index, "worker result has no buffer".to_owned());
                return;
            };
            let shared_available = Reflect::get(&js_sys::global(), &"SharedArrayBuffer".into())
                .is_ok_and(|value| value.is_function());
            if !(payload.is_instance_of::<ArrayBuffer>()
                || shared_available && payload.is_instance_of::<SharedArrayBuffer>())
            {
                state.fail(index, "worker result is not a byte buffer".to_owned());
                return;
            }
            let bytes = Uint8Array::new(&payload);
            if bytes.length() as usize > state.pending[&id].output_limit {
                state.fail(
                    index,
                    "worker result exceeds declared byte limit".to_owned(),
                );
                return;
            }
            state.complete(index, BrowserOutcome::Bytes(bytes.to_vec()));
        }
        Some("cancelled") => state.complete(index, BrowserOutcome::Cancelled),
        Some("error") => state.complete(
            index,
            BrowserOutcome::Failed(
                property(&data, "error")
                    .and_then(|value| value.as_string())
                    .unwrap_or_else(|| "snapshot export failed".to_owned()),
            ),
        ),
        _ => state.fail(index, "worker returned an unknown message type".to_owned()),
    }
}
