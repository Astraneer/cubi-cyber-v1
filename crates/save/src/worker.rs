//! File IO and checkpoint flattening execute outside the foreground world owner.
use crate::*;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::thread::JoinHandle;
use world::persistence::Checkpoint;

pub struct SnapshotMetadata {
    pub server_tick: Tick,
    pub public_seed: u32,
    pub generator_version: u32,
    pub rules_version: u32,
    pub identities: Vec<Identity>,
    pub players: Vec<SavedPlayer>,
    pub dedup: Vec<(PlayerId, DedupState)>,
    pub runtimes: Vec<RuntimeRecord>,
}
pub enum WriteRequest {
    Batch {
        ticket: u64,
        batch: Batch,
    },
    Checkpoint {
        ticket: u64,
        checkpoint: Checkpoint,
        metadata: SnapshotMetadata,
    },
}
#[derive(Clone, Copy, Debug)]
pub enum Durability {
    Batch { sequence: u64 },
    Checkpoint { sequence: u64 },
}
pub struct Completion {
    pub ticket: u64,
    pub result: Result<Durability, SaveError>,
}

pub struct SaveWorker {
    requests: Option<SyncSender<WriteRequest>>,
    responses: Option<Receiver<Completion>>,
    thread: Option<JoinHandle<()>>,
}
impl SaveWorker {
    pub fn spawn(mut store: Store, capacity: usize) -> Result<Self, SaveError> {
        if capacity == 0 {
            return Err(SaveError::Bounds("writer queue"));
        }
        let (requests, input) = mpsc::sync_channel(capacity);
        let (output, responses) = mpsc::sync_channel(capacity);
        let thread = std::thread::Builder::new()
            .name("cubi-save".into())
            .spawn(move || {
                let mut failed = false;
                while let Ok(request) = input.recv() {
                    let (ticket, result) = match request {
                        WriteRequest::Batch { ticket, batch } => (
                            ticket,
                            if failed {
                                Err(SaveError::Poisoned)
                            } else {
                                store
                                    .append(&batch)
                                    .map(|sequence| Durability::Batch { sequence })
                            },
                        ),
                        WriteRequest::Checkpoint {
                            ticket,
                            checkpoint,
                            metadata,
                        } => (
                            ticket,
                            if failed {
                                Err(SaveError::Poisoned)
                            } else {
                                let sequence = store.last_sequence();
                                let snapshot = Snapshot {
                                    record_sequence: sequence,
                                    server_tick: metadata.server_tick,
                                    public_seed: metadata.public_seed,
                                    generator_version: metadata.generator_version,
                                    rules_version: metadata.rules_version,
                                    world: checkpoint.to_restore_state(),
                                    identities: metadata.identities,
                                    players: metadata.players,
                                    dedup: metadata.dedup,
                                    runtimes: metadata.runtimes,
                                };
                                store
                                    .checkpoint(&snapshot)
                                    .map(|()| Durability::Checkpoint { sequence })
                            },
                        ),
                    };
                    failed |= result.is_err();
                    // On graceful/drop shutdown the receiver may be gone. Already
                    // queued writes are still drained, but no unconfirmed ACK exists.
                    let _ = output.send(Completion { ticket, result });
                }
            })?;
        Ok(Self {
            requests: Some(requests),
            responses: Some(responses),
            thread: Some(thread),
        })
    }
    pub fn try_submit(&self, request: WriteRequest) -> Result<(), Box<TrySendError<WriteRequest>>> {
        self.requests
            .as_ref()
            .expect("live writer")
            .try_send(request)
            .map_err(Box::new)
    }
    pub fn try_complete(&self) -> Result<Completion, TryRecvError> {
        self.responses.as_ref().expect("live writer").try_recv()
    }
}
impl Drop for SaveWorker {
    fn drop(&mut self) {
        self.responses.take();
        self.requests.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
