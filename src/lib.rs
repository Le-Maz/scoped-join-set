mod future_wrappers;

use std::{any::Any, collections::HashMap, error::Error, fmt, future::Future};

use tokio::task::{Id, JoinSet};

use crate::future_wrappers::{split_spawn_task, FutureHolder};

type TokioJoinError = tokio::task::JoinError;

/// A scoped wrapper around [`tokio::task::JoinSet`] for dynamically spawning
/// and awaiting multiple asynchronous tasks.
///
/// `ScopedJoinSet` allows you to spawn futures that are bound to a specific lifetime `'scope`,
/// ensuring all tasks outlive the scope in which they are spawned. Unlike a standard
/// `JoinSet`, this structure internally tracks uniquely-owned pinned futures via `FutureHolder`,
/// while exposing a weak reference (`WeakFuture`) for safe polling.
///
/// # Features
/// - **Scoped Futures**: Ensures futures cannot outlive the provided `'scope`.
/// - **Dynamic Spawning**: Tasks can be spawned dynamically at runtime.
/// - **Cancellation Safe**: `join_next()` is cancellation safe; tasks remain in the set
///   until they complete and are awaited.
/// - **Automatic Abort**: Dropping a `FutureHolder` aborts its associated task.
///
/// # Safety
/// - Each spawned task has exactly one `FutureHolder` (owning the `Pin<Box>` of the future)
///   and one `WeakFuture` (polling handle).  
/// - `FutureHolder` tracks whether the future is alive using an `AtomicBool`.  
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
    pub fn new() -> Self {
        Self {
            join_set: JoinSet::new(),
            holders: HashMap::new(),
        }
    }

    pub fn spawn<F>(&mut self, task: F)
    where
        F: Future<Output = T> + Send + 'scope,
        T: Send,
    {
        let (id, holder) = split_spawn_task(&mut self.join_set, task);
        self.holders.insert(id, holder);
    }

    pub fn is_empty(&self) -> bool {
        self.join_set.is_empty()
    }

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
