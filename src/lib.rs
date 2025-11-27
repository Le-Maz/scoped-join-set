//! A scoped, lifetime-aware wrapper around [`tokio::task::JoinSet`].
//!
//! `ScopedJoinSet` enables dynamically spawning async tasks whose futures are
//! tied to a specific Rust lifetime `'scope`. This provides a safe alternative
//! to hand-managed `unsafe` approaches while enabling advanced async task
//! orchestration where futures do **not** need to be `'static`.
//!
//! Each spawned task is split into:
//!
//! - A [`FutureHolder`], owning the actual boxed future and ensuring proper drop
//!   ordering.
//! - A [`WeakFuture`] (inside the `JoinSet`), which the Tokio runtime polls.
//!
//! When the [`ScopedJoinSet`] is dropped, all tasks are aborted and all future
//! memory is reclaimed safely.
//!
//! # Guarantees
//!
//! - Futures spawned via [`ScopedJoinSet::spawn`] cannot outlive `'scope`.
//! - `join_next()` is cancellation-safe.
//! - Dropping the entire set aborts all tasks.
//!
//! # Example
//!
//! ```rust
//! use scoped_join_set::ScopedJoinSet;
//!
//! async fn demo<'a>() {
//!     let mut set = ScopedJoinSet::<u32>::new();
//!
//!     set.spawn(async { 5 });
//!
//!     while let Some(result) = set.join_next().await {
//!         assert_eq!(result.unwrap(), 5);
//!     }
//! }
//! ```

mod future_wrappers;

use std::{any::Any, collections::HashMap, error::Error, fmt, future::Future};

use tokio::task::{Id, JoinSet};

use crate::future_wrappers::{split_spawn_task, FutureHolder};

type TokioJoinError = tokio::task::JoinError;

/// A scoped wrapper around [`tokio::task::JoinSet`] for dynamically spawning
/// futures with a lifetime `'scope`.
///
/// Unlike normal Tokio tasks requiring `'static` futures, this set accepts
/// futures tied to a temporary stack lifetime, as long as the
/// `ScopedJoinSet` lives long enough.
///
/// Each call to [`spawn`](Self::spawn) places a non-`'static` future into the
/// runtime via a controlled internal mechanism (`FutureHolder` + `WeakFuture`).
///
/// # Type Parameters
/// - `'scope`: The lifetime during which spawned futures must live.
/// - `T`: The output of each spawned future.
#[derive(Default)]
pub struct ScopedJoinSet<'scope, T>
where
    T: 'static,
{
    join_set: JoinSet<Option<T>>,
    holders: HashMap<Id, FutureHolder<'scope, T>>,
}

impl<'scope, T> ScopedJoinSet<'scope, T>
where
    T: 'static,
{
    /// Creates an empty `ScopedJoinSet`.
    pub fn new() -> Self {
        Self {
            join_set: JoinSet::new(),
            holders: HashMap::new(),
        }
    }

    /// Spawns a new scoped future into the set.
    ///
    /// This function is analogous to [`tokio::spawn`], but accepts futures with
    /// the given scope lifetime.
    ///
    /// # Parameters
    /// - `task`: The user future to spawn.
    ///
    /// # Requirements
    /// - The returned value `T` must be `Send`.
    /// - The future must be `Send` and live at least `'scope`.
    pub fn spawn<F>(&mut self, task: F)
    where
        F: Future<Output = T> + Send + 'scope,
        T: Send,
    {
        let (id, holder) = split_spawn_task(&mut self.join_set, task);
        self.holders.insert(id, holder);
    }

    /// Returns whether no tasks remain.
    ///
    /// Equivalent to [`JoinSet::is_empty`].
    pub fn is_empty(&self) -> bool {
        self.join_set.is_empty()
    }

    /// Waits for the next completed task in the set.
    ///
    /// Returns:
    /// - `Some(Ok(T))` on success.
    /// - `Some(Err(JoinError))` on cancellation or panic.
    /// - `None` when the set is empty.
    ///
    /// This method is **cancellation-safe**: dropping the future returned by
    /// `.await` does not remove the task from the set.
    pub async fn join_next(&mut self) -> Option<Result<T, JoinError>> {
        match self.join_set.join_next_with_id().await? {
            Ok((id, Some(value))) => {
                self.holders.remove(&id);
                Some(Ok(value))
            }
            Ok((id, None)) => {
                self.holders.remove(&id);
                Some(Err(JoinError::Cancelled))
            }
            Err(error) => {
                self.holders.remove(&error.id());
                Some(Err(error.into()))
            }
        }
    }
}

unsafe impl<'scope, T> Send for ScopedJoinSet<'scope, T> {}

/// Errors returned when joining scoped tasks.
#[derive(Debug)]
pub enum JoinError {
    /// The task was cancelled or aborted.
    Cancelled,
    /// The task panicked and its panic payload is returned.
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
    /// Returns `true` if the task was cancelled.
    pub fn is_cancelled(&self) -> bool {
        matches!(self, JoinError::Cancelled)
    }

    /// Returns `true` if the task panicked.
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
