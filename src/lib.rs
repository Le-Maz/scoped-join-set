use std::{
    any::Any,
    collections::HashMap,
    error::Error,
    fmt,
    future::Future,
    mem::transmute,
    pin::Pin,
    ptr::NonNull,
    sync::atomic::{AtomicBool, Ordering},
    task::{Context, Poll},
};

use tokio::{
    runtime::{Handle, RuntimeFlavor},
    task::{block_in_place, AbortHandle, Id, JoinSet},
};

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
        let alive: NonNull<AtomicBool> = Box::leak(Box::new(AtomicBool::new(true))).into();
        let future: NonNull<F> = Box::leak(Box::new(task)).into();
        let weak_future = unsafe {
            type Src<'scope, T> = NonNull<dyn Future<Output = T> + 'scope>;
            type Dst<T> = NonNull<dyn Future<Output = T>>;
            let future = transmute::<Src<'scope, T>, Dst<T>>(future);
            WeakFuture { future, alive }
        };
        let handle = self.join_set.spawn(weak_future);
        let holder = FutureHolder {
            abort_handle: handle.clone(),
            future,
            alive,
        };
        self.holders.insert(handle.id(), holder);
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

struct FutureHolder<'scope, T>
where
    T: 'static,
{
    abort_handle: AbortHandle,
    future: NonNull<dyn Future<Output = T> + Send + 'scope>,
    alive: NonNull<AtomicBool>,
}

impl<'scope, T> Drop for FutureHolder<'scope, T> {
    fn drop(&mut self) {
        self.abort_handle.abort();

        if Handle::current().runtime_flavor() == RuntimeFlavor::MultiThread {
            let alive = unsafe { self.alive.as_ref() };
            // Wait until the future is not being polled
            if alive.load(Ordering::Acquire) {
                block_in_place(|| {
                    while alive.load(Ordering::Acquire) {
                        std::hint::spin_loop();
                    }
                });
            }
            drop(unsafe { Box::from_raw(self.future.as_ptr()) });
            drop(unsafe { Box::from_raw(self.alive.as_mut()) });
        } else {
            drop(unsafe { Box::from_raw(self.future.as_ptr()) });
        }
    }
}

struct WeakFuture<T> {
    future: NonNull<dyn Future<Output = T>>,
    alive: NonNull<AtomicBool>,
}

impl<T> Drop for WeakFuture<T> {
    fn drop(&mut self) {
        // Release the polling flag
        if Handle::current().runtime_flavor() == RuntimeFlavor::MultiThread {
            unsafe { self.alive.as_ref() }.store(false, Ordering::Release);
        } else {
            drop(unsafe { Box::from_raw(self.alive.as_mut()) });
        }
    }
}

unsafe impl<T: Send> Send for WeakFuture<T> {}

impl<T> Future for WeakFuture<T> {
    type Output = Option<T>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let future_ref = unsafe { self.future.as_mut() };
        let future_pin = unsafe { Pin::new_unchecked(future_ref) };
        Future::poll(future_pin, cx).map(Some)
    }
}

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
