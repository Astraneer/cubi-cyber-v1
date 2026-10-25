use core_types::{PageId, SpaceId, Tick, Version};
use jobs::*;

fn page(x: i32) -> PageId {
    PageId {
        space: SpaceId(0),
        x,
        y: 0,
        z: 0,
    }
}
fn key(x: i32) -> JobKey {
    JobKey {
        page: page(x),
        kind: 1,
    }
}
fn limits(jobs: usize, bytes: usize) -> Limits {
    Limits {
        max_jobs: jobs,
        max_bytes: bytes,
        max_age_ticks: 40,
        reserved_interactive_jobs: 0,
        reserved_interactive_bytes: 0,
    }
}
fn budget() -> WorkBudget {
    WorkBudget::unrestricted(WorkCost {
        cells: u64::MAX,
        quads: u64::MAX,
        bytes: u64::MAX,
        rays: u64::MAX,
    })
}
fn event(tick: u64, bytes: usize) -> JobSpec {
    JobSpec::event(
        Priority::Normal,
        WorkCost {
            cells: 1,
            ..WorkCost::ZERO
        },
        bytes,
        Tick(tick),
    )
}
fn ready<T>(take: Take<T>) -> Dispatch<T> {
    if let Take::Ready(dispatch) = take {
        dispatch
    } else {
        panic!("expected ready job")
    }
}

#[test]
fn hundred_thousand_visual_edits_build_once_and_keep_the_last_snapshot() {
    let mut scheduler = Scheduler::new(limits(8, 1024)).unwrap();
    for version in 0..100_000 {
        scheduler
            .enqueue(
                JobSpec::derived(
                    key(1),
                    Priority::Normal,
                    WorkCost {
                        cells: 4096,
                        ..WorkCost::ZERO
                    },
                    64,
                    Tick(0),
                )
                .with_input(page(1), Some(Version(version))),
                version,
            )
            .unwrap();
    }
    assert_eq!(scheduler.pending_len(), 1);
    assert_eq!(scheduler.retained_bytes(), 64);
    let task = ready(scheduler.take_next(Tick(1), &mut budget(), |_| Some(Version(99_999))));
    assert_eq!(task.payload, 99_999);
    assert!(matches!(
        scheduler.finish(
            Completion {
                id: task.id,
                outcome: JobOutcome::Success(task.payload)
            },
            |_| Some(Version(99_999))
        ),
        Finish::Published(99_999)
    ));
    assert_eq!(scheduler.stats().coalesced, 99_999);
    assert_eq!(scheduler.stats().dispatched, 1);
    assert_eq!(scheduler.stats().published, 1);
    assert_eq!(scheduler.retained_bytes(), 0);
}

#[test]
fn gameplay_events_are_neither_merged_nor_reordered() {
    let mut scheduler = Scheduler::new(limits(1024, 4096)).unwrap();
    for value in 0..1000 {
        scheduler.enqueue(event(0, 4), value).unwrap();
    }
    assert_eq!(scheduler.pending_len(), 1000);
    let mut observed = Vec::new();
    while let Take::Ready(job) = scheduler.take_next(Tick(1), &mut budget(), |_| None) {
        if let Finish::Published(value) = scheduler.finish(
            Completion {
                id: job.id,
                outcome: JobOutcome::Success(job.payload),
            },
            |_| None,
        ) {
            observed.push(value);
        }
    }
    assert_eq!(observed, (0..1000).collect::<Vec<_>>());
    assert_eq!(scheduler.stats().coalesced, 0);
}

#[test]
fn capacity_includes_running_results_and_protects_interactive_reserve() {
    let mut policy = limits(3, 30);
    policy.reserved_interactive_jobs = 1;
    policy.reserved_interactive_bytes = 10;
    let mut scheduler = Scheduler::new(policy).unwrap();
    scheduler.enqueue(event(0, 10), 1).unwrap();
    scheduler.enqueue(event(0, 10), 2).unwrap();
    let rejected = scheduler.enqueue(event(0, 10), 3).unwrap_err();
    assert_eq!(rejected.reason, RejectReason::JobLimit);
    assert_eq!(rejected.payload, 3);
    let mut near = event(0, 10);
    near.priority = Priority::Interactive;
    scheduler.enqueue(near.clone(), 4).unwrap();
    let task = ready(scheduler.take_next(Tick(1), &mut budget(), |_| None));
    assert_eq!(task.payload, 4);
    assert_eq!(scheduler.running_len(), 1);
    assert_eq!(
        scheduler.enqueue(near.clone(), 5).unwrap_err().reason,
        RejectReason::JobLimit
    );
    assert!(matches!(
        scheduler.finish(
            Completion {
                id: task.id,
                outcome: JobOutcome::Success(())
            },
            |_| None
        ),
        Finish::Published(())
    ));
    scheduler.enqueue(near, 5).unwrap();
    assert_eq!(scheduler.retained_bytes(), 30);
}

#[test]
fn byte_capacity_is_independent_from_job_slots_and_replacement_is_transactional() {
    let mut scheduler = Scheduler::new(limits(10, 100)).unwrap();
    let spec = JobSpec::derived(key(0), Priority::Normal, WorkCost::ZERO, 80, Tick(0));
    scheduler.enqueue(spec.clone(), "original").unwrap();
    let mut excessive = spec.clone();
    excessive.retained_bytes = 101;
    assert_eq!(
        scheduler.enqueue(excessive, "rejected").unwrap_err().reason,
        RejectReason::ByteLimit
    );
    assert_eq!(scheduler.retained_bytes(), 80);
    assert_eq!(
        ready(scheduler.take_next(Tick(1), &mut budget(), |_| None)).payload,
        "original"
    );
}

#[test]
fn running_old_versions_are_cancelled_and_never_published() {
    let mut scheduler = Scheduler::new(limits(4, 1000)).unwrap();
    let spec = |v| {
        JobSpec::derived(key(0), Priority::Interactive, WorkCost::ZERO, 100, Tick(0))
            .with_input(page(0), Some(Version(v)))
    };
    scheduler.enqueue(spec(1), 1).unwrap();
    let old = ready(scheduler.take_next(Tick(0), &mut budget(), |_| Some(Version(1))));
    scheduler.enqueue(spec(2), 2).unwrap();
    assert!(old.cancellation.is_cancelled());
    assert!(matches!(
        scheduler.finish(
            Completion {
                id: old.id,
                outcome: JobOutcome::Success(1)
            },
            |_| Some(Version(2))
        ),
        Finish::Cancelled
    ));
    let current = ready(scheduler.take_next(Tick(0), &mut budget(), |_| Some(Version(2))));
    assert!(matches!(
        scheduler.finish(
            Completion {
                id: current.id,
                outcome: JobOutcome::Success(2)
            },
            |_| Some(Version(3))
        ),
        Finish::Stale
    ));
    assert_eq!(scheduler.stats().published, 0);
    assert_eq!(scheduler.retained_bytes(), 0);
}

#[test]
fn unknown_generation_input_cannot_overwrite_a_page_loaded_later() {
    let mut scheduler = Scheduler::new(limits(2, 100)).unwrap();
    scheduler
        .enqueue(
            JobSpec::derived(key(0), Priority::Normal, WorkCost::ZERO, 10, Tick(0))
                .with_input(page(0), None),
            (),
        )
        .unwrap();
    let task = ready(scheduler.take_next(Tick(0), &mut budget(), |_| None));
    assert!(matches!(
        scheduler.finish(
            Completion {
                id: task.id,
                outcome: JobOutcome::Success("generated")
            },
            |_| Some(Version(0))
        ),
        Finish::Stale
    ));
}

#[test]
fn pending_stale_visual_work_does_not_spend_any_budget() {
    let mut scheduler = Scheduler::new(limits(2, 100)).unwrap();
    scheduler
        .enqueue(
            JobSpec::derived(
                key(0),
                Priority::Normal,
                WorkCost {
                    cells: 4096,
                    ..WorkCost::ZERO
                },
                10,
                Tick(0),
            )
            .with_input(page(0), Some(Version(1))),
            (),
        )
        .unwrap();
    let mut allowance = WorkBudget::unrestricted(WorkCost {
        cells: 4096,
        ..WorkCost::ZERO
    });
    assert!(matches!(
        scheduler.take_next(Tick(0), &mut allowance, |_| Some(Version(2))),
        Take::Empty
    ));
    assert_eq!(allowance.remaining().cells, 4096);
    assert_eq!(scheduler.stats().stale, 1);
}

#[test]
fn expiry_returns_gameplay_payload_instead_of_dropping_it() {
    let mut scheduler = Scheduler::new(limits(1, 100)).unwrap();
    scheduler
        .enqueue(event(0, 50), String::from("place-block action 37"))
        .unwrap();
    match scheduler.take_next(Tick(40), &mut budget(), |_| None) {
        Take::Expired(expired) => {
            assert_eq!(expired.payload, "place-block action 37");
            assert_eq!(expired.age_ticks, 40);
            scheduler.enqueue(event(40, 50), expired.payload).unwrap();
        }
        _ => panic!("expired event must be returned"),
    }
    assert_eq!(scheduler.pending_len(), 1);
    assert_eq!(scheduler.retained_bytes(), 50);
}

#[test]
fn repeated_changes_do_not_reset_the_queue_age_and_old_background_work_is_promoted() {
    let mut scheduler = Scheduler::new(limits(4, 100)).unwrap();
    let spec =
        |tick| JobSpec::derived(key(0), Priority::Background, WorkCost::ZERO, 10, Tick(tick));
    for tick in 0..20 {
        scheduler.enqueue(spec(tick), tick).unwrap();
    }
    assert_eq!(scheduler.oldest_pending_age(Tick(20)), 20);
    scheduler
        .enqueue(
            JobSpec::event(Priority::Interactive, WorkCost::ZERO, 10, Tick(20)),
            100,
        )
        .unwrap();
    let task = ready(scheduler.take_next(Tick(20), &mut budget(), |_| None));
    assert_eq!(task.payload, 19);
}

#[test]
fn logical_budgets_enforce_every_dimension_and_do_not_block_smaller_work() {
    let mut scheduler = Scheduler::new(limits(4, 100)).unwrap();
    scheduler
        .enqueue(
            JobSpec::event(
                Priority::Interactive,
                WorkCost {
                    cells: 1,
                    bytes: 11,
                    ..WorkCost::ZERO
                },
                10,
                Tick(0),
            ),
            "too big",
        )
        .unwrap();
    scheduler
        .enqueue(
            JobSpec::event(
                Priority::Normal,
                WorkCost {
                    cells: 2,
                    bytes: 3,
                    rays: 4,
                    ..WorkCost::ZERO
                },
                10,
                Tick(0),
            ),
            "fits",
        )
        .unwrap();
    let mut allowance = WorkBudget::unrestricted(WorkCost {
        cells: 2,
        bytes: 10,
        rays: 4,
        ..WorkCost::ZERO
    });
    assert_eq!(
        ready(scheduler.take_next(Tick(0), &mut allowance, |_| None)).payload,
        "fits"
    );
    assert_eq!(
        allowance.remaining(),
        WorkCost {
            cells: 0,
            bytes: 7,
            rays: 0,
            quads: 0
        }
    );
    assert!(matches!(
        scheduler.take_next(Tick(0), &mut allowance, |_| None),
        Take::BudgetBlocked
    ));
}

#[test]
fn interactive_work_consumes_its_reserved_budget_and_background_cannot_steal_it() {
    let mut scheduler = Scheduler::new(limits(4, 100)).unwrap();
    scheduler
        .enqueue(
            JobSpec::event(
                Priority::Normal,
                WorkCost {
                    cells: 7,
                    ..WorkCost::ZERO
                },
                10,
                Tick(0),
            ),
            "background",
        )
        .unwrap();
    let mut allowance = WorkBudget::new(
        WorkCost {
            cells: 10,
            ..WorkCost::ZERO
        },
        WorkCost {
            cells: 4,
            ..WorkCost::ZERO
        },
    );
    assert!(matches!(
        scheduler.take_next(Tick(0), &mut allowance, |_| None),
        Take::BudgetBlocked
    ));
    scheduler
        .enqueue(
            JobSpec::event(
                Priority::Interactive,
                WorkCost {
                    cells: 3,
                    ..WorkCost::ZERO
                },
                10,
                Tick(0),
            ),
            "near",
        )
        .unwrap();
    assert_eq!(
        ready(scheduler.take_next(Tick(0), &mut allowance, |_| None)).payload,
        "near"
    );
    allowance.release_interactive_reserve();
    assert_eq!(
        ready(scheduler.take_next(Tick(0), &mut allowance, |_| None)).payload,
        "background"
    );
    assert_eq!(allowance.remaining().cells, 0);
}

#[test]
fn malformed_and_older_replacements_preserve_the_current_snapshot() {
    let mut scheduler = Scheduler::new(limits(4, 100)).unwrap();
    let spec = JobSpec::derived(key(0), Priority::Normal, WorkCost::ZERO, 10, Tick(0))
        .with_input(page(0), Some(Version(3)));
    scheduler.enqueue(spec.clone(), 3).unwrap();
    let mut duplicate = spec.clone();
    duplicate.input_versions.push(duplicate.input_versions[0]);
    assert_eq!(
        scheduler.enqueue(duplicate, 0).unwrap_err().reason,
        RejectReason::DuplicateInput
    );
    let mut old = spec;
    old.input_versions[0].version = Some(Version(2));
    assert_eq!(
        scheduler.enqueue(old, 2).unwrap_err().reason,
        RejectReason::OlderInputs
    );
    assert_eq!(
        ready(scheduler.take_next(Tick(0), &mut budget(), |_| Some(Version(3)))).payload,
        3
    );
}

#[test]
fn executor_backpressure_can_requeue_without_duplicating_an_event() {
    let mut scheduler = Scheduler::new(limits(1, 100)).unwrap();
    let id = scheduler.enqueue(event(0, 25), 37).unwrap();
    let dispatch = ready(scheduler.take_next(Tick(0), &mut budget(), |_| None));
    scheduler.requeue(dispatch).unwrap();
    assert_eq!(scheduler.pending_len(), 1);
    assert_eq!(scheduler.running_len(), 0);
    assert_eq!(scheduler.retained_bytes(), 25);
    let dispatch = ready(scheduler.take_next(Tick(1), &mut budget(), |_| None));
    assert_eq!((dispatch.id, dispatch.payload), (id, 37));
    assert!(matches!(
        scheduler.finish(
            Completion {
                id,
                outcome: JobOutcome::Success(37)
            },
            |_| None
        ),
        Finish::Published(37)
    ));
    assert!(matches!(
        scheduler.finish(
            Completion {
                id,
                outcome: JobOutcome::Success(37)
            },
            |_| None
        ),
        Finish::UnknownJob
    ));
}

#[test]
fn invalid_limits_are_rejected_before_allocating_queues() {
    assert!(Scheduler::<()>::new(limits(0, 100)).is_err());
    let mut invalid = limits(1, 100);
    invalid.reserved_interactive_jobs = 2;
    assert!(Scheduler::<()>::new(invalid).is_err());
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn worker_panic_and_error_produce_completions_without_killing_the_pool() {
    use std::time::{Duration, Instant};
    let mut scheduler = Scheduler::new(limits(8, 100)).unwrap();
    for n in 0..3 {
        scheduler.enqueue(event(0, 1), n).unwrap();
    }
    let pool = NativePool::new(1, 1, |value, _| match value {
        0 => panic!("intentional snapshot task panic"),
        1 => Err("snapshot rejected".to_owned()),
        value => Ok(value * 2),
    })
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut outcomes = Vec::new();
    while outcomes.len() < 3 && Instant::now() < deadline {
        if let Take::Ready(dispatch) = scheduler.take_next(Tick(0), &mut budget(), |_| None) {
            match pool.try_submit(dispatch) {
                Ok(()) => {}
                Err(SubmitError::Full(dispatch) | SubmitError::Closed(dispatch)) => {
                    scheduler.requeue(dispatch).unwrap();
                }
            }
        }
        while let Some(completion) = pool.try_recv() {
            outcomes.push(scheduler.finish(completion, |_| None));
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(outcomes.len(), 3);
    assert!(
        matches!(&outcomes[0], Finish::Failed(JobFailure::Panic(message)) if message.contains("intentional"))
    );
    assert!(
        matches!(&outcomes[1], Finish::Failed(JobFailure::Error(message)) if message == "snapshot rejected")
    );
    assert!(matches!(outcomes[2], Finish::Published(4)));
    assert_eq!(pool.worker_count(), 1);
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn dropping_pool_with_full_results_and_pending_work_does_not_deadlock() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    };
    use std::time::{Duration, Instant};
    let executed = Arc::new(AtomicUsize::new(0));
    let count = executed.clone();
    let pool = NativePool::new(1, 1, move |value, _| {
        count.fetch_add(1, Ordering::SeqCst);
        Ok(value)
    })
    .unwrap();
    let mut scheduler = Scheduler::new(limits(8, 100)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut submitted = 0;
    while submitted < 4 && Instant::now() < deadline {
        if scheduler.pending_len() == 0 {
            scheduler.enqueue(event(0, 1), submitted).unwrap();
        }
        let job = ready(scheduler.take_next(Tick(0), &mut budget(), |_| None));
        match pool.try_submit(job) {
            Ok(()) => submitted += 1,
            Err(SubmitError::Full(job) | SubmitError::Closed(job)) => {
                scheduler.requeue(job).unwrap()
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(submitted, 4);
    while executed.load(Ordering::SeqCst) < 3 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(executed.load(Ordering::SeqCst), 3);
    let (done, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        drop(pool);
        let _ = done.send(());
    });
    receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("own workers joined despite a full result queue");
}
