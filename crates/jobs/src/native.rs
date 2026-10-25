use crate::{CancellationToken, Completion, Dispatch, JobFailure, JobId, JobOutcome};
use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};

#[derive(Debug)]
pub enum PoolError {
    InvalidCapacity,
    Spawn(std::io::Error),
}

/// The rejected dispatch stays owned by the caller and can be requeued unchanged.
#[derive(Debug)]
pub enum SubmitError<T> {
    Full(Dispatch<T>),
    Closed(Dispatch<T>),
}

/// Bounded snapshot executor. Results occupy capacity until the owner receives them.
///
/// Dropping the pool cancels its own tasks, closes result delivery, and joins its own
/// threads. Computations must check the cancellation token at bounded checkpoints;
/// arbitrary noncooperative user closures cannot be forcibly preempted in safe Rust.
pub struct NativePool<T, R> {
    sender: Option<mpsc::SyncSender<Dispatch<T>>>,
    receiver: Option<mpsc::Receiver<Completion<R>>>,
    workers: Vec<JoinHandle<()>>,
    tokens: Arc<Mutex<BTreeMap<JobId, CancellationToken>>>,
}

impl<T: Send + 'static, R: Send + 'static> NativePool<T, R> {
    pub fn new(
        workers: usize,
        queue_capacity: usize,
        execute: impl Fn(T, &CancellationToken) -> Result<R, String> + Send + Sync + 'static,
    ) -> Result<Self, PoolError> {
        if workers == 0 || queue_capacity == 0 || workers.checked_add(queue_capacity).is_none() {
            return Err(PoolError::InvalidCapacity);
        }
        let (sender, jobs) = mpsc::sync_channel::<Dispatch<T>>(queue_capacity);
        let (results, receiver) = mpsc::sync_channel(workers + queue_capacity);
        let jobs = Arc::new(Mutex::new(jobs));
        let execute = Arc::new(execute);
        let tokens = Arc::new(Mutex::new(BTreeMap::new()));
        let mut pool = Self {
            sender: Some(sender),
            receiver: Some(receiver),
            workers: Vec::with_capacity(workers),
            tokens,
        };
        for index in 0..workers {
            let jobs = jobs.clone();
            let results = results.clone();
            let execute = execute.clone();
            let tokens = pool.tokens.clone();
            let worker = thread::Builder::new()
                .name(format!("cubi-worker-{index}"))
                .spawn(move || {
                    loop {
                        // Receiver is only locked while waiting/dequeueing, never during computation.
                        let job = match jobs
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .recv()
                        {
                            Ok(job) => job,
                            Err(_) => return,
                        };
                        let outcome = if job.cancellation.is_cancelled() {
                            JobOutcome::Cancelled
                        } else {
                            match catch_unwind(AssertUnwindSafe(|| {
                                execute(job.payload, &job.cancellation)
                            })) {
                                Ok(Ok(value)) => {
                                    if job.cancellation.is_cancelled() {
                                        JobOutcome::Cancelled
                                    } else {
                                        JobOutcome::Success(value)
                                    }
                                }
                                Ok(Err(error)) => JobOutcome::Failed(JobFailure::Error(error)),
                                Err(panic) => {
                                    let message = panic
                                        .downcast_ref::<String>()
                                        .cloned()
                                        .or_else(|| {
                                            panic
                                                .downcast_ref::<&str>()
                                                .map(|value| (*value).to_owned())
                                        })
                                        .unwrap_or_else(|| {
                                            "worker panic with non-string payload".to_owned()
                                        });
                                    JobOutcome::Failed(JobFailure::Panic(message))
                                }
                            }
                        };
                        tokens
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .remove(&job.id);
                        if results
                            .send(Completion {
                                id: job.id,
                                outcome,
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                });
            match worker {
                Ok(worker) => pool.workers.push(worker),
                Err(error) => return Err(PoolError::Spawn(error)),
            }
        }
        Ok(pool)
    }

    pub fn try_submit(&self, dispatch: Dispatch<T>) -> Result<(), SubmitError<T>> {
        let Some(sender) = &self.sender else {
            return Err(SubmitError::Closed(dispatch));
        };
        let id = dispatch.id;
        let token = dispatch.cancellation.clone();
        let mut tokens = self
            .tokens
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        match sender.try_send(dispatch) {
            Ok(()) => {
                tokens.insert(id, token);
                Ok(())
            }
            Err(mpsc::TrySendError::Full(job)) => Err(SubmitError::Full(job)),
            Err(mpsc::TrySendError::Disconnected(job)) => Err(SubmitError::Closed(job)),
        }
    }

    pub fn try_recv(&self) -> Option<Completion<R>> {
        self.receiver.as_ref()?.try_recv().ok()
    }

    pub fn worker_count(&self) -> usize {
        self.workers.len()
    }

    /// An orderly shutdown can call this before continuing to drain completions.
    pub fn cancel_all(&self) {
        for token in self
            .tokens
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .values()
        {
            token.cancel();
        }
    }
}

impl<T, R> Drop for NativePool<T, R> {
    fn drop(&mut self) {
        for token in self
            .tokens
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .values()
        {
            token.cancel();
        }
        // Closing result delivery first releases any worker blocked on a full result queue.
        self.receiver.take();
        self.sender.take();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}
