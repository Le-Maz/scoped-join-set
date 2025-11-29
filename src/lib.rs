//! A scoped wrapper around [`tokio::task::JoinSet`] allowing non-`'static`
//! futures to be spawned safely.
//!
//! # What `ScopedJoinSet` provides
//!
//! Tokio requires all spawned tasks to be `'static`. `ScopedJoinSet` lifts
//! this restriction by:
//!
//! - allocating each user future and its output slot on the heap,
//! - pinning the wrapper future so it never moves,
//! - unsafely casting it to `'static` (sound because the allocation is kept
//!   alive until the task completes),
//! - storing each task’s output in `Box<Option<T>>`,
//! - retrieving and deallocating that output when the task completes.
//!
//! This lets users write:
//!
//! # Example
//!
//! ```rust
//! use scoped_join_set::ScopedJoinSet;
//!
//! #[tokio::main]
//! async fn main() {
//!     let temporary_value = Box::new(5);
//!     let value_ref = &temporary_value;
//!
//!     let mut set = ScopedJoinSet::<u32>::new();
//!
//!     set.spawn(async {
//!         println!("I have a {value_ref}");
//!         **value_ref
//!     });
//!
//!     while let Some(result) = set.join_next().await {
//!         assert_eq!(result.unwrap(), *temporary_value);
//!     }
//! }
//! ```
//!
//! without requiring the future to be `'static`.
//!
//! # How it works internally
//!
//! 1. `spawn()` allocates a `Box<MaybeUninit<T>>` for the result and leaks it.
//! 2. The user future is wrapped in `WriteOutput`, which writes its output
//!    into this slot.
//! 3. The wrapper future is pinned and unsafely converted to `'static`.
//! 4. Tokio polls the wrapper and returns a raw pointer identifying the
//!    output slot.
//! 5. `join_next()` converts that pointer back, reads the output, and frees
//!    the allocation.
//! 6. `Drop` aborts any remaining tasks and reclaims all leaked storage.
//!
//! No hashmaps or external bookkeeping structures are used—the pointer
//! returned by each completed task uniquely identifies its output slot.  
//! There is *no* self-referential struct and no dependence on pinning beyond
//! ensuring that the wrapped future remains immovable.

mod future_wrappers;

use crate::future_wrappers::{SendPtr, WriteOutput};
use std::{
    any::Any, error::Error, fmt, future::Future, hint::spin_loop, marker::PhantomData,
    mem::MaybeUninit, ptr::NonNull,
};
use tokio::{
    runtime::{Handle, RuntimeFlavor},
    task::{block_in_place, JoinSet},
};

type TokioJoinError = tokio::task::JoinError;

/// A scoped task set analogous to [`tokio::task::JoinSet`] but accepting
/// non-`'static` futures.
///
/// `ScopedJoinSet` allows spawning futures that borrow data with lifetime
/// `'scope`, while still being executed by Tokio, which normally requires
/// `'static` futures.  
///
/// Each task’s result is written into a dedicated heap allocation and later
/// recovered by `join_next()`. No maps, arenas, or auxiliary structures are
/// used—the completed task returns a raw pointer identifying its output slot.
///
/// Results are yielded in completion order.
#[derive(Default)]
pub struct ScopedJoinSet<'scope, T>
where
    T: 'scope + Send,
{
    join_set: JoinSet<SendPtr>,
    _marker: PhantomData<&'scope T>,
}

impl<'scope, T> ScopedJoinSet<'scope, T>
where
    T: 'scope + Send,
{
    /// Create an empty `ScopedJoinSet`.
    pub fn new() -> Self {
        Self {
            join_set: JoinSet::new(),
            _marker: PhantomData,
        }
    }

    /// Spawns a non-`'static` future into the set.
    ///
    /// The future's output is written into a heap-allocated slot owned by the
    /// set. The future itself is wrapped and safely promoted to `'static`
    /// because `ScopedJoinSet` guarantees that its allocation outlives the task.
    pub fn spawn<F>(&mut self, task: F)
    where
        F: Future<Output = T> + Send + 'scope,
        T: Send,
    {
        // Allocate output slot.
        let output: NonNull<MaybeUninit<T>> = Box::leak(Box::new(MaybeUninit::uninit())).into();

        // Wrap + convert to 'static future.
        let task = WriteOutput::new(task, output.cast());
        let task = unsafe { task.into_static() };

        self.join_set.spawn(task);
    }

    /// Returns `true` if no tasks remain.
    pub fn is_empty(&self) -> bool {
        self.join_set.is_empty()
    }

    /// Returns the number of active tasks.
    pub fn len(&self) -> usize {
        self.join_set.len()
    }

    /// Waits for the next task to complete and returns its result.
    ///
    /// On success, returns the output value produced by the task.  
    /// On failure, returns a [`JoinError`].  
    ///
    /// The associated output allocation is reclaimed automatically.
    pub async fn join_next(&mut self) -> Option<Result<T, JoinError>> {
        match self.join_set.join_next().await? {
            Ok(output) => {
                let output: NonNull<T> = output.cast();
                let output = unsafe { *Box::from_raw(output.as_ptr()) };
                Some(Ok(output))
            }
            Err(error) => Some(Err(error.into())),
        }
    }

    /// Non-async version of [`join_next`].
    ///
    /// Returns immediately if no task has finished polling.
    pub fn try_join_next(&mut self) -> Option<Result<T, JoinError>> {
        match self.join_set.try_join_next()? {
            Ok(output) => {
                let output: NonNull<T> = output.cast();
                let output = unsafe { *Box::from_raw(output.as_ptr()) };
                Some(Ok(output))
            }
            Err(error) => Some(Err(error.into())),
        }
    }

    /// Waits for the completion of all tasks and returns their results in
    /// completion order.
    pub async fn join_all(mut self) -> Vec<Result<T, JoinError>> {
        let mut results = Vec::with_capacity(self.len());
        while let Some(r) = self.join_next().await {
            results.push(r);
        }
        results
    }

    /// Aborts all running tasks.
    ///
    /// Any output slots associated with incomplete tasks are cleaned up
    /// when the task wrapper is dropped.
    pub fn abort_all(&mut self) {
        self.join_set.abort_all();
    }

    /// Aborts all tasks and then waits for them to finish.
    ///
    /// Returns results for tasks that completed before the abort took effect.
    pub async fn shutdown(mut self) -> Vec<Result<T, JoinError>> {
        self.abort_all();
        self.join_all().await
    }
}

unsafe impl<'scope, T: Send> Send for ScopedJoinSet<'scope, T> {}

impl<'scope, T> Drop for ScopedJoinSet<'scope, T>
where
    T: Send,
{
    /// Drops the set, aborting all remaining tasks.
    ///
    /// Remaining output allocations are reclaimed as aborted task wrappers
    /// are dropped.  
    ///
    /// In a multi-threaded Tokio runtime this may block the thread until
    /// abort notifications are processed.
    fn drop(&mut self) {
        if self.is_empty() {
            return;
        }

        self.abort_all();

        if Handle::current().runtime_flavor() == RuntimeFlavor::MultiThread {
            // Multi-threaded runtime: wait for abort completions.
            block_in_place(|| {
                while !self.is_empty() {
                    self.try_join_next();
                    spin_loop();
                }
            });
        }
    }
}

/// Errors returned by scoped tasks.
///
/// A task may be cancelled (e.g., via `abort_all`) or may panic.
/// Both cases are detected and surfaced by `ScopedJoinSet`.
#[derive(Debug)]
pub enum JoinError {
    Cancelled,
    Panicked(Box<dyn Any + Send + 'static>),
}

impl fmt::Display for JoinError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JoinError::Cancelled => write!(f, "task was cancelled"),
            JoinError::Panicked(_) => write!(f, "task panicked"),
        }
    }
}

impl Error for JoinError {}

impl JoinError {
    pub fn is_cancelled(&self) -> bool {
        matches!(self, JoinError::Cancelled)
    }

    pub fn is_panic(&self) -> bool {
        matches!(self, JoinError::Panicked(_))
    }
}

impl From<TokioJoinError> for JoinError {
    fn from(err: TokioJoinError) -> Self {
        if err.is_cancelled() {
            JoinError::Cancelled
        } else if err.is_panic() {
            JoinError::Panicked(err.into_panic())
        } else {
            JoinError::Cancelled
        }
    }
}
