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
//! ```
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

mod future_wrappers;
mod lifetime_obfuscation;

use std::{any::Any, collections::HashMap, error::Error, fmt, future::Future, marker::PhantomData};

use tokio::task::{Id, JoinSet};

use crate::{
    future_wrappers::{split_spawn_task, FutureHolder},
    lifetime_obfuscation::{from_bytes_unchecked, obfuscate_lifetime},
};

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
    T: 'scope + Send,
{
    join_set: JoinSet<Option<Box<[u8]>>>,
    holders: HashMap<Id, FutureHolder<'scope, Box<[u8]>>>,
    _marker: PhantomData<T>,
}

impl<'scope, T> ScopedJoinSet<'scope, T>
where
    T: 'scope + Send,
{
    /// Creates an empty `ScopedJoinSet`.
    pub fn new() -> Self {
        Self {
            join_set: JoinSet::new(),
            holders: HashMap::new(),
            _marker: PhantomData,
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
        let task = obfuscate_lifetime(task);
        let (id, holder) = split_spawn_task(&mut self.join_set, task);
        self.holders.insert(id, holder);
    }

    /// Returns whether no tasks remain.
    ///
    /// Equivalent to [`JoinSet::is_empty`].
    pub fn is_empty(&self) -> bool {
        self.join_set.is_empty()
    }

    /// Returns the number of tasks currently stored in the set.
    ///
    /// Equivalent to [`JoinSet::len`].
    pub fn len(&self) -> usize {
        self.holders.len()
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
            Ok((id, Some(bytes))) => {
                self.holders.remove(&id);

                // SAFETY: By having `T` as the type argument, reconstruction needs to be successful.
                let value = unsafe { from_bytes_unchecked(&bytes) };

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

    /// Attempts to join one completed task without awaiting.
    ///
    /// This method is **non-blocking** and returns immediately.
    ///
    /// Returns:
    /// - `Some(Ok(T))` if a task has already completed successfully,
    /// - `Some(Err(JoinError))` if a completed task was cancelled or panicked,
    /// - `None` if **no** tasks have completed yet **or** the set is empty.
    ///
    /// This differs from [`join_next`](Self::join_next):
    /// - `join_next()` **awaits** the next completed task.
    /// - `try_join_next()` returns immediately without awaiting.
    ///
    /// # Cancellation Safety
    ///
    /// This method is always cancellation-safe, because it does not perform
    /// any async operations and does not remove tasks unless they have
    /// already finished.
    pub fn try_join_next(&mut self) -> Option<Result<T, JoinError>> {
        match self.join_set.try_join_next_with_id()? {
            Ok((id, Some(bytes))) => {
                // Remove the holder before reconstructing the value.
                self.holders.remove(&id);

                // SAFETY: The bytes originated from a valid instance of T.
                let value = unsafe { from_bytes_unchecked(&bytes) };
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

    /// Waits for all tasks in the set to complete, consuming the set and
    /// returning a list of results.
    ///
    /// The returned results are in **completion order**, not spawn order.
    ///
    /// # Cancellation Safety
    /// If this future is dropped, remaining tasks stay inside the set.
    pub async fn join_all(mut self) -> Vec<Result<T, JoinError>> {
        let mut results = Vec::with_capacity(self.len());
        while let Some(res) = self.join_next().await {
            results.push(res);
        }
        results
    }

    /// Aborts all currently-running tasks.
    ///
    /// This does **not** await task completion; it merely issues abort signals.
    /// To also wait for them, call [`join_remaining`](#method.join_remaining)
    /// or drop the entire set.
    ///
    /// After calling `abort_all`, the set will still contain all tasks until
    /// they complete their aborts and are polled again.
    pub fn abort_all(&mut self) {
        for holder in self.holders.values_mut() {
            holder.abort();
        }
    }

    /// Aborts all tasks *and then waits for them to finish*.
    ///
    /// Unlike dropping the set, `shutdown` consumes it gracefully.
    pub async fn shutdown(mut self) -> Vec<Result<T, JoinError>> {
        self.abort_all();
        self.join_all().await
    }
}

unsafe impl<'scope, T: Send> Send for ScopedJoinSet<'scope, T> {}

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
