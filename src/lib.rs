//! A scoped wrapper around [`tokio::task::JoinSet`] allowing non-`'static`
//! futures to be spawned safely.
//!
//! # What `ScopedJoinSet` provides
//!
//! Tokio requires all spawned tasks to be `'static`. `ScopedJoinSet` lifts
//! this restriction by:
//!
//! - allocating each user future on the heap,
//! - pinning it so it never moves,
//! - unsafely casting it to `'static` (valid because the allocation is
//!   retained until the task completes),
//! - storing the task's output in `Box<Option<T>>`,
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
//! 1. `spawn()` allocates a `Box<Option<T>>` for the result and leaks it.
//! 2. The user future is wrapped in `WriteOutput`, which writes its output
//!    into this storage.
//! 3. The wrapper future is pinned and unsafely converted to `'static`.
//! 4. Tokio polls the wrapper; `ScopedJoinSet` tracks the task ID and the
//!    output pointer.
//! 5. `join_next()` retrieves and deallocates the result.
//! 6. `Drop` aborts all active tasks and reclaims leaked storage.
//!
//! There is *no* self-referential struct and no dependence on pinning beyond
//! guaranteeing that the wrapped future remains immovable.

#![feature(ptr_metadata)]

mod future_wrappers;

use crate::future_wrappers::WriteOutput;
use std::{
    any::Any, collections::HashMap, error::Error, fmt, future::Future, hint::spin_loop,
    marker::PhantomData, ptr::NonNull,
};
use tokio::{
    runtime::{Handle, RuntimeFlavor},
    task::{block_in_place, AbortHandle, Id, JoinSet},
};

type TokioJoinError = tokio::task::JoinError;

/// A scoped task set analogous to [`tokio::task::JoinSet`] but requiring
/// only `'scope` instead of `'static` futures.
///
/// All tasks produce an output of type `T`. Results are returned in
/// completion order by `join_next()` and related methods.
#[derive(Default)]
pub struct ScopedJoinSet<'scope, T>
where
    T: 'scope + Send,
{
    join_set: JoinSet<()>,
    holders: HashMap<Id, (AbortHandle, NonNull<Option<T>>)>,
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
            holders: HashMap::new(),
            _marker: PhantomData,
        }
    }

    /// Spawn a non-`'static` future into the set.
    pub fn spawn<F>(&mut self, task: F)
    where
        F: Future<Output = T> + Send + 'scope,
        T: Send,
    {
        // Allocate output slot.
        let output = Box::leak(Box::new(None)).into();

        // Wrap + convert to 'static future.
        let task = WriteOutput::new(task, output);
        let task = unsafe { task.into_static() };

        let handle = self.join_set.spawn(task);
        self.holders.insert(handle.id(), (handle, output));
    }

    /// Returns `true` if no tasks remain.
    pub fn is_empty(&self) -> bool {
        self.join_set.is_empty()
    }

    /// Returns the number of active tasks.
    pub fn len(&self) -> usize {
        self.holders.len()
    }

    /// Wait for the next task to complete and return its result.
    pub async fn join_next(&mut self) -> Option<Result<T, JoinError>> {
        match self.join_set.join_next_with_id().await? {
            Ok((id, _)) => {
                let (_, output) = self.holders.remove(&id)?;
                let output = unsafe { *Box::from_raw(output.as_ptr()) };
                Some(output.ok_or(JoinError::Cancelled))
            }
            Err(error) => {
                let (_, output) = self.holders.remove(&error.id())?;
                unsafe { drop(Box::from_raw(output.as_ptr())) };
                Some(Err(error.into()))
            }
        }
    }

    /// Non-async version of `join_next()`.
    pub fn try_join_next(&mut self) -> Option<Result<T, JoinError>> {
        match self.join_set.try_join_next_with_id()? {
            Ok((id, _)) => {
                let (_, output) = self.holders.remove(&id)?;
                let output = unsafe { *Box::from_raw(output.as_ptr()) };
                Some(output.ok_or(JoinError::Cancelled))
            }
            Err(error) => {
                let (_, output) = self.holders.remove(&error.id())?;
                unsafe { drop(Box::from_raw(output.as_ptr())) };
                Some(Err(error.into()))
            }
        }
    }

    /// Wait for all tasks to complete.
    pub async fn join_all(mut self) -> Vec<Result<T, JoinError>> {
        let mut results = Vec::with_capacity(self.len());
        while let Some(r) = self.join_next().await {
            results.push(r);
        }
        results
    }

    /// Abort all running tasks.
    pub fn abort_all(&mut self) {
        for (handle, _) in self.holders.values_mut() {
            handle.abort();
        }
    }

    /// Abort all tasks and wait for their completion.
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
    fn drop(&mut self) {
        if self.is_empty() {
            return;
        }

        self.abort_all();

        // Current-thread runtime must not block; deallocate eagerly.
        if Handle::current().runtime_flavor() == RuntimeFlavor::CurrentThread {
            self.holders.drain().for_each(|(_, (_, output))| {
                unsafe { drop(Box::from_raw(output.as_ptr())) };
            });
        } else {
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
