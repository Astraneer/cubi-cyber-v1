use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::fmt;
use std::marker::PhantomData;

/// Allocation calls and successfully transferred bytes within one scope.
///
/// A successful realloc releases `old_layout.size()` bytes and allocates `new_size`
/// bytes. A failed realloc changes neither byte count. Released bytes may exceed
/// allocated bytes when the scope frees a buffer created before measurement.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AllocationCounts {
    pub alloc_calls: u64,
    pub zeroed_alloc_calls: u64,
    pub realloc_calls: u64,
    pub dealloc_calls: u64,
    pub failed_calls: u64,
    pub allocated_bytes: u64,
    pub released_bytes: u64,
}

impl AllocationCounts {
    pub const ZERO: Self = Self {
        alloc_calls: 0,
        zeroed_alloc_calls: 0,
        realloc_calls: 0,
        dealloc_calls: 0,
        failed_calls: 0,
        allocated_bytes: 0,
        released_bytes: 0,
    };

    pub const fn allocating_calls(self) -> u64 {
        self.alloc_calls
            .saturating_add(self.zeroed_alloc_calls)
            .saturating_add(self.realloc_calls)
    }
}

#[derive(Clone, Copy)]
struct State {
    active: bool,
    counts: AllocationCounts,
}

thread_local! {
    // Const TLS initialization and Cell access cannot allocate or borrow recursively.
    static STATE: Cell<State> = const { Cell::new(State { active: false, counts: AllocationCounts::ZERO }) };
}

/// Wrap the process global allocator to observe opt-in per-thread scopes.
///
/// ```
/// use std::alloc::System;
/// use telemetry::CountingAllocator;
/// #[global_allocator]
/// static ALLOCATOR: CountingAllocator<System> = CountingAllocator::new(System);
/// ```
pub struct CountingAllocator<A = System> {
    inner: A,
}

impl<A> CountingAllocator<A> {
    pub const fn new(inner: A) -> Self {
        Self { inner }
    }
}

fn record(update: impl FnOnce(&mut AllocationCounts)) {
    // An allocation during TLS destruction must still reach the wrapped allocator.
    // try_with ignores unavailable measurement storage and does not panic.
    let _ = STATE.try_with(|state| {
        let mut current = state.get();
        if current.active {
            update(&mut current.counts);
            state.set(current);
        }
    });
}

// SAFETY: Every operation preserves GlobalAlloc's layout/pointer contract, forwards
// exactly once to the inner allocator, and returns its result unchanged. Recording
// uses non-allocating const-initialized TLS, never unwinds, and mutates no allocation.
unsafe impl<A: GlobalAlloc> GlobalAlloc for CountingAllocator<A> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: The caller's GlobalAlloc preconditions are passed to the inner allocator.
        let ptr = unsafe { self.inner.alloc(layout) };
        record(|counts| {
            counts.alloc_calls = counts.alloc_calls.saturating_add(1);
            if ptr.is_null() {
                counts.failed_calls = counts.failed_calls.saturating_add(1);
            } else {
                counts.allocated_bytes =
                    counts.allocated_bytes.saturating_add(layout.size() as u64);
            }
        });
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: Same layout contract, including zero initialization, delegated unchanged.
        let ptr = unsafe { self.inner.alloc_zeroed(layout) };
        record(|counts| {
            counts.zeroed_alloc_calls = counts.zeroed_alloc_calls.saturating_add(1);
            if ptr.is_null() {
                counts.failed_calls = counts.failed_calls.saturating_add(1);
            } else {
                counts.allocated_bytes =
                    counts.allocated_bytes.saturating_add(layout.size() as u64);
            }
        });
        ptr
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: Caller guarantees ptr/layout provenance and a valid nonzero new_size.
        let new_ptr = unsafe { self.inner.realloc(ptr, layout, new_size) };
        record(|counts| {
            counts.realloc_calls = counts.realloc_calls.saturating_add(1);
            if new_ptr.is_null() {
                counts.failed_calls = counts.failed_calls.saturating_add(1);
            } else {
                counts.allocated_bytes = counts.allocated_bytes.saturating_add(new_size as u64);
                counts.released_bytes = counts.released_bytes.saturating_add(layout.size() as u64);
            }
        });
        new_ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: Caller guarantees ptr/layout describe an allocation of the inner allocator.
        unsafe { self.inner.dealloc(ptr, layout) };
        record(|counts| {
            counts.dealloc_calls = counts.dealloc_calls.saturating_add(1);
            counts.released_bytes = counts.released_bytes.saturating_add(layout.size() as u64);
        });
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScopeError {
    AlreadyActive,
    ThreadLocalUnavailable,
}

impl fmt::Display for ScopeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::AlreadyActive => "an allocation scope is already active on this thread",
            Self::ThreadLocalUnavailable => {
                "allocation measurement thread-local state is unavailable"
            }
        })
    }
}

impl std::error::Error for ScopeError {}

/// RAII guard. Nested scopes are rejected and panic unwinding disables measurement.
/// The guard is deliberately neither Send nor Sync: it owns only this thread's scope.
#[must_use = "dropping the guard stops allocation measurement"]
pub struct AllocationScope {
    active: bool,
    same_thread: PhantomData<*mut ()>,
}

impl AllocationScope {
    pub fn begin() -> Result<Self, ScopeError> {
        STATE
            .try_with(|state| {
                if state.get().active {
                    return Err(ScopeError::AlreadyActive);
                }
                state.set(State {
                    active: true,
                    counts: AllocationCounts::ZERO,
                });
                Ok(Self {
                    active: true,
                    same_thread: PhantomData,
                })
            })
            .map_err(|_| ScopeError::ThreadLocalUnavailable)?
    }

    pub fn snapshot(&self) -> AllocationCounts {
        STATE
            .try_with(|state| state.get().counts)
            .unwrap_or(AllocationCounts::ZERO)
    }

    pub fn finish(mut self) -> AllocationCounts {
        self.stop()
    }

    fn stop(&mut self) -> AllocationCounts {
        if !self.active {
            return AllocationCounts::ZERO;
        }
        self.active = false;
        STATE
            .try_with(|state| {
                let current = state.get();
                state.set(State {
                    active: false,
                    counts: AllocationCounts::ZERO,
                });
                current.counts
            })
            .unwrap_or(AllocationCounts::ZERO)
    }
}

impl Drop for AllocationScope {
    fn drop(&mut self) {
        self.stop();
    }
}
