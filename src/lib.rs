//! A scoped, lifetime-aware wrapper around [`tokio::task::JoinSet`].
//!
//! `ScopedJoinSet` allows spawning async tasks whose futures are tied to a specific Rust
//! lifetime `'scope`. Unlike regular Tokio tasks, futures do **not** need to be `'static`.
//!
//! Each spawned task consists of:
//! - A [`FutureHolder`], which owns the heap-allocated future and coordinates memory cleanup.
//! - A [`WeakFuture`] inside the `JoinSet`, which is actually polled by Tokio.
//!
//! When the `ScopedJoinSet` is dropped, all tasks are aborted and memory is reclaimed safely.
//!
//! # Guarantees
//! - Futures cannot outlive `'scope`.
//! - `join_next()` and `try_join_next()` are cancellation-safe.
//! - Dropping the set aborts all tasks.
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

#![feature(ptr_metadata)]

mod future_wrappers;

use crate::future_wrappers::{split_spawn_task, FutureHolder};
use std::{any::Any, collections::HashMap, error::Error, fmt, future::Future, marker::PhantomData};
use tokio::task::{Id, JoinSet};

type TokioJoinError = tokio::task::JoinError;

/// A scoped wrapper around [`tokio::task::JoinSet`] that allows spawning futures with a
/// non-`'static` lifetime `'scope`.
///
/// Futures are safely stored in heap allocations and polled via `WeakFuture`, while
/// `FutureHolder` ensures proper abort and memory management.
///
/// # Type Parameters
/// - `'scope`: The lifetime during which spawned futures must live.
/// - `T`: The output type of each spawned future.
#[derive(Default)]
pub struct ScopedJoinSet<'scope, T>
where
    T: 'scope + Send,
{
    join_set: JoinSet<()>,
    holders: HashMap<Id, FutureHolder<'scope, T>>,
    _marker: PhantomData<T>,
}

impl<'scope, T> ScopedJoinSet<'scope, T>
where
    T: 'scope + Send,
{
    /// Creates a new, empty `ScopedJoinSet`.
    pub fn new() -> Self {
        Self {
            join_set: JoinSet::new(),
            holders: HashMap::new(),
            _marker: PhantomData,
        }
    }

    /// Spawns a new scoped future into the set.
    ///
    /// Accepts any future that is `Send` and lives at least `'scope`.
    pub fn spawn<F>(&mut self, task: F)
    where
        F: Future<Output = T> + Send + 'scope,
        T: Send,
    {
        let (id, holder) = split_spawn_task(&mut self.join_set, task);
        self.holders.insert(id, holder);
    }

    /// Returns true if no tasks remain.
    pub fn is_empty(&self) -> bool {
        self.join_set.is_empty()
    }

    /// Returns the number of tasks currently stored.
    pub fn len(&self) -> usize {
        self.holders.len()
    }

    /// Waits for the next completed task in the set.
    ///
    /// Returns:
    /// - `Some(Ok(T))` if a task completes successfully.
    /// - `Some(Err(JoinError))` if a task was cancelled or panicked.
    /// - `None` if the set is empty.
    ///
    /// Cancellation-safe: dropping the returned future does not remove tasks.
    pub async fn join_next(&mut self) -> Option<Result<T, JoinError>> {
        match self.join_set.join_next_with_id().await? {
            Ok((id, _)) => {
                let holder = self.holders.remove(&id)?;
                let output = holder.get_output().ok_or(JoinError::Cancelled);
                Some(output)
            }
            Err(error) => {
                self.holders.remove(&error.id());
                Some(Err(error.into()))
            }
        }
    }

    /// Attempts to join one completed task without awaiting.
    ///
    /// Returns immediately with:
    /// - `Some(Ok(T))` for a completed task,
    /// - `Some(Err(JoinError))` for a cancelled or panicked task,
    /// - `None` if no tasks have completed or the set is empty.
    pub fn try_join_next(&mut self) -> Option<Result<T, JoinError>> {
        match self.join_set.try_join_next_with_id()? {
            Ok((id, _)) => {
                let holder = self.holders.remove(&id)?;
                let output = holder.get_output().ok_or(JoinError::Cancelled);
                Some(output)
            }
            Err(error) => {
                self.holders.remove(&error.id());
                Some(Err(error.into()))
            }
        }
    }

    /// Waits for all tasks to complete, consuming the set.
    ///
    /// Returns results in completion order. Cancellation-safe.
    pub async fn join_all(mut self) -> Vec<Result<T, JoinError>> {
        let mut results = Vec::with_capacity(self.len());
        while let Some(res) = self.join_next().await {
            results.push(res);
        }
        results
    }

    /// Aborts all currently-running tasks.
    ///
    /// Tasks remain in the set until they are completed or polled again.
    pub fn abort_all(&mut self) {
        for holder in self.holders.values_mut() {
            holder.abort();
        }
    }

    /// Aborts all tasks and waits for them to finish.
    ///
    /// Consumes the set and returns a list of results.
    pub async fn shutdown(mut self) -> Vec<Result<T, JoinError>> {
        self.abort_all();
        self.join_all().await
    }
}

unsafe impl<'scope, T: Send> Send for ScopedJoinSet<'scope, T> {}

/// Errors returned when joining scoped tasks.
#[derive(Debug)]
pub enum JoinError {
    /// Task was cancelled or aborted.
    Cancelled,
    /// Task panicked; payload returned.
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
    /// Returns true if the task was cancelled.
    pub fn is_cancelled(&self) -> bool {
        matches!(self, JoinError::Cancelled)
    }

    /// Returns true if the task panicked.
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
