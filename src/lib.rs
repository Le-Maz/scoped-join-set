use tokio::{
    runtime::{Handle, RuntimeFlavor},
    task::{AbortHandle, Id},
};

use std::any::Any;
use std::collections::HashMap;
use std::sync::Mutex;
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Weak},
    task::{Context, Poll},
};

use tokio::task::JoinSet;

type TokioJoinError = tokio::task::JoinError;

/// A scoped variant of [`tokio::task::JoinSet`] that ensures all futures outlive `'scope`,
/// while allowing them to be spawned and awaited dynamically.
///
/// This structure tracks all spawned futures internally with a holder, allowing weak references
/// to be used for polling in a way that is memory-safe and supports scoped lifetimes.
///
/// # Safety
///
/// There is only one `FutureHolder` and one `WeakFuture` at a time per task.
/// If the `WeakFuture` successfully upgrades, the future is guaranteed to still be valid
/// because `FutureHolder` holds the strong reference until it is dropped (after task join).
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
    /// Creates a new, empty `ScopedJoinSet`.
    pub fn new() -> Self {
        Self {
            join_set: JoinSet::new(),
            holders: HashMap::new(),
        }
    }

    /// Spawns a new task on the join set.
    ///
    /// The future must be `'scope`-bound and `Send`.
    /// Internally, the future is wrapped in a `FutureHolder` and a weak reference is
    /// passed to the join set for execution.
    ///
    /// # Panics
    ///
    /// This method panics if called outside of a Tokio runtime.
    pub fn spawn<F>(&mut self, task: F)
    where
        F: Future<Output = T> + Send + 'scope,
        T: Send,
    {
        let strong: Arc<Mutex<dyn Future<Output = T> + Send + Unpin + 'scope>> =
            Arc::new(Mutex::new(Box::pin(task)));

        let weak_future = WeakFuture {
            future: unsafe {
                std::mem::transmute::<
                    Weak<Mutex<dyn Future<Output = T> + Send + Unpin + 'scope>>,
                    Weak<Mutex<dyn Future<Output = T> + Send + Unpin>>,
                >(Arc::downgrade(&strong))
            },
        };
        let handle = self.join_set.spawn(weak_future);
        let holder = FutureHolder {
            abort_handle: handle.clone(),
            future: strong,
        };
        self.holders.insert(handle.id(), holder);
    }

    /// Returns `true` if there are no remaining tasks in the join set.
    pub fn is_empty(&self) -> bool {
        self.join_set.is_empty()
    }

    /// Waits for the next task to complete.
    ///
    /// Returns:
    /// - `Some(Ok(T))` if a task completed successfully.
    /// - `Some(Err(JoinError))` if the task was cancelled or panicked.
    /// - `None` if there are no more tasks in the set.
    ///
    /// # Cancel Safety
    ///
    /// This method is **cancellation safe** — if the `join_next().await` is itself
    /// cancelled (e.g., due to timeout or a `select!` branch winning), no state is lost.
    /// The underlying task remains in the join set and will be yielded again on the next
    /// call to `join_next()`.
    ///
    /// Internally, the `JoinSet`'s `join_next_with_id()` ensures that the task is not removed
    /// from the set unless it has completed and its result has been received.
    ///
    /// The associated future holder is only dropped and removed from internal tracking
    /// once the task finishes and is returned from `join_next()`.
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

/// Holds a strong reference to the future so it can be upgraded by the `WeakFuture`.
///
/// Internally uses an `Arc<UnsafeCell<dyn Future>>` to allow safe access from the
/// `poll` method in `WeakFuture`. Only `FutureHolder` owns the strong reference;
/// once dropped, the weak reference becomes invalid and polling returns `None`.
struct FutureHolder<'scope, T>
where
    T: 'static,
{
    abort_handle: AbortHandle,
    future: Arc<Mutex<dyn Future<Output = T> + Send + Unpin + 'scope>>,
}

impl<'scope, T> Drop for FutureHolder<'scope, T> {
    fn drop(&mut self) {
        self.abort_handle.abort();
        if Handle::current().runtime_flavor() == RuntimeFlavor::CurrentThread {
            return;
        }
        if self.future.try_lock().is_err() {
            ::tokio::task::block_in_place(|| drop(self.future.lock()));
        }
    }
}

unsafe impl<'scope, T> Send for FutureHolder<'scope, T> {}

/// A weak reference to a future, used inside the join set.
///
/// If the weak reference can be upgraded, the future is still alive and will be polled.
/// Once the strong reference (`FutureHolder`) is dropped, the weak reference cannot be upgraded,
/// and polling will return `Poll::Ready(None)`.
struct WeakFuture<T> {
    future: Weak<Mutex<dyn Future<Output = T> + Send + Unpin>>,
}

unsafe impl<T> Send for WeakFuture<T> {}

impl<T> Future for WeakFuture<T> {
    type Output = Option<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if let Some(fut) = self.future.upgrade() {
            let mut fut = fut.lock().unwrap();
            Future::poll(Pin::new(&mut *fut), cx).map(Some)
        } else {
            Poll::Ready(None)
        }
    }
}

use std::error::Error;
use std::fmt;

/// Represents failure when joining a task.
#[derive(Debug)]
pub enum JoinError {
    /// The task was cancelled (e.g., dropped before completing).
    Cancelled,

    /// The task panicked.
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
            // Should never happen, but we guard anyway.
            JoinError::Cancelled
        }
    }
}
