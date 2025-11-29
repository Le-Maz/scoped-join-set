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
//!     set.shutdown().await;
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
//!
//! No hashmaps or external bookkeeping structures are used—the pointer
//! returned by each completed task uniquely identifies its output slot.  
//! There is *no* self-referential struct and no dependence on pinning beyond
//! ensuring that the wrapped future remains immovable.
//!
//! # Important: Explicit Shutdown Required
//!
//! A `ScopedJoinSet` **must be terminated explicitly** using either
//!
//! - [`ScopedJoinSet::shutdown()`], or
//! - [`ScopedJoinSet::join_all()`],
//!
//! before it is dropped.
//!
//! Dropping a `ScopedJoinSet` implicitly (e.g., letting it go out of scope
//! without calling one of the methods above) is considered a logic error.
//!
//! To prevent accidental misuse — which could otherwise cause tasks holding
//! references into the stack to outlive their scope — the destructor prints an
//! error message and **aborts the process**.

mod future_wrappers;

use crate::future_wrappers::{SendPtr, WriteOutput};
use std::{
    any::Any, error::Error, fmt, future::Future, marker::PhantomData, mem::MaybeUninit,
    process::abort, ptr::NonNull,
};
use tokio::task::JoinSet;

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
/// non-`'static` futures.
///
/// # Mandatory explicit shutdown
///
/// A `ScopedJoinSet` **must** be terminated using either
/// [`ScopedJoinSet::shutdown()`] or [`ScopedJoinSet::join_all()`].
///
/// If dropped without having called one of these methods, the destructor
/// prints an error message and **aborts the process** to prevent tasks from
/// escaping the scope.
///
/// This rule ensures that tasks which borrow stack data never outlive that
/// data.
#[derive(Default)]
pub struct ScopedJoinSet<'scope, T>
where
    T: 'scope + Send,
{
    join_set: JoinSet<SendPtr>,
    _marker: PhantomData<&'scope T>,
    _safe_to_drop: bool,
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
            _safe_to_drop: false,
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
        self._safe_to_drop = true;
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
    /// Drops the set.
    ///
    /// # Panics / Aborts
    ///
    /// A `ScopedJoinSet` **cannot** be dropped implicitly.  
    /// If `.join_all()` or `.shutdown()` has not been called beforehand,
    /// this destructor prints a diagnostic and **aborts the process**.
    ///
    /// This is a safety guarantee: tasks may borrow stack data, and allowing
    /// implicit drop would risk those tasks outliving their scope.
    fn drop(&mut self) {
        if !self._safe_to_drop {
            eprintln!("ScopedJoinSet cannot be dropped implicitly. Use `.join_all().await` or `.shutdown().await`.");
            abort();
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
