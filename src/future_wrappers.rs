//! Internal future wrapper utilities for [`ScopedJoinSet`](crate::ScopedJoinSet).
//!
//! This module exposes two primary helpers:
//!
//! - [`split_spawn_task`] — accepts a user future and a [`JoinSet`] and returns
//!   both an [`Id`] and a [`FutureHolder`] which owns the real future.
//!
//! - [`WeakFuture`] — a `'static` pinned reference to the future stored inside
//!   `FutureHolder`. This is the value that Tokio actually polls. It is safe
//!   only because each `WeakFuture` shares an `AtomicBool` with the
//!   corresponding `FutureHolder`, allowing both to coordinate their drop
//!   semantics.
//!
//! ## Safety
//!
//! - Each spawned scoped task owns exactly one pinned future (`FutureHolder`)
//!   and one `'static` reference used by Tokio (`WeakFuture`).
//! - The `'static` cast in [`WeakFuture::new`] is safe **only** because
//!   `FutureHolder` guarantees the real boxed future outlives the spawned task,
//!   and the task is aborted/dropped before the box is reclaimed.

use std::{
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

/// Spawns a scoped future inside a [`JoinSet`], returning both its Tokio `Id`
/// and a [`FutureHolder`] which owns the actual boxed future.
///
/// This function splits future ownership into:
///
/// - `WeakFuture`: a `'static` future handle inserted into the `JoinSet`.
/// - `FutureHolder`: the real owner of the pinned boxed future.
///
/// The `'static` handle is created via a controlled and documented safe
/// transmute, with safety guaranteed by the coordinated drop behavior between
/// the two wrappers.
///
/// # Parameters
/// - `join_set`: The [`JoinSet`] into which the future should be spawned.
/// - `task`: The user future tied to the `'scope` lifetime.
///
/// # Returns
/// `(task_id, future_holder)`
///
/// # Safety
/// Safe only because `FutureHolder` ensures the future is not dropped until the
/// weak future has finished executing and the runtime has completed its polling.
pub(crate) fn split_spawn_task<'scope, F, T>(
    join_set: &mut JoinSet<Option<T>>,
    task: F,
) -> (Id, FutureHolder<'scope, T>)
where
    F: Future<Output = T> + Send + 'scope,
    T: Send + 'static,
{
    // Shared flag indicating whether WeakFuture is still alive.
    let alive: NonNull<AtomicBool> = Box::leak(Box::new(AtomicBool::new(true))).into();

    // Allocate the true owning future.
    let mut future: NonNull<F> = Box::leak(Box::new(task)).into();

    // Create the weak polling future.
    let weak_future = unsafe { WeakFuture::new(alive, &mut future) };

    // Spawn the WeakFuture into the JoinSet.
    let handle = join_set.spawn(weak_future);

    (handle.id(), FutureHolder::new(alive, future, &handle))
}

/// Owns the actual pinned boxed future and ensures correct destruction
/// ordering relative to the corresponding [`WeakFuture`].
///
/// Dropping a `FutureHolder` aborts its task, then optionally waits for the
/// `WeakFuture` to acknowledge termination via its shared `AtomicBool`.
pub(crate) struct FutureHolder<'scope, T>
where
    T: 'static,
{
    abort_handle: AbortHandle,
    future: NonNull<dyn Future<Output = T> + Send + 'scope>,
    alive: NonNull<AtomicBool>,
}

impl<'scope, T> FutureHolder<'scope, T> {
    /// Creates a new `FutureHolder`.
    ///
    /// # Parameters
    /// - `alive`: Pointer to the shared liveness flag.
    /// - `future`: Pointer to the boxed future being owned.
    /// - `handle`: The abort handle for the associated task.
    fn new<F>(
        alive: NonNull<AtomicBool>,
        future: NonNull<F>,
        handle: &AbortHandle,
    ) -> FutureHolder<'scope, T>
    where
        F: Future<Output = T> + 'scope + Send,
    {
        FutureHolder {
            abort_handle: handle.clone(),
            future,
            alive,
        }
    }

    /// Spins until the weak future signals that polling has terminated.
    ///
    /// Used only in the multithreaded runtime; single-threaded runtimes drop
    /// synchronously and cannot require spinning.
    fn spin_until_termination(&mut self) {
        let alive = unsafe { self.alive.as_ref() };
        if alive.load(Ordering::Acquire) {
            block_in_place(|| {
                while alive.load(Ordering::Acquire) {
                    std::hint::spin_loop();
                }
            });
        }
    }
}

impl<'scope, T> Drop for FutureHolder<'scope, T> {
    fn drop(&mut self) {
        // Abort the running task.
        self.abort_handle.abort();

        // Wait for WeakFuture to finish if we are multithreaded.
        if Handle::current().runtime_flavor() == RuntimeFlavor::MultiThread {
            self.spin_until_termination();
            // Now free the liveness flag.
            drop(unsafe { Box::from_raw(self.alive.as_mut()) });
        }

        // Finally reclaim the future itself.
        drop(unsafe { Box::from_raw(self.future.as_ptr()) });
    }
}

/// A `'static` weak reference to a user future.
///
/// This wrapper is the object passed to Tokio's scheduler. Its existence allows
/// the real future to remain owned by [`FutureHolder`] while maintaining a
/// `'static` reference in the executor.
///
/// # Safety
/// The `'static` lifetime is sound because:
/// - The future is allocated inside a leaked `Box`, owned by `FutureHolder`.
/// - `FutureHolder` waits for this `WeakFuture` to drop before reclaiming the
///   box.
/// - The executor cannot outlive the scope where `FutureHolder` is dropped.
pub(crate) struct WeakFuture<T: 'static> {
    /// Placed behind a `'static` pinned reference via controlled transmute.
    future: Pin<&'static mut dyn Future<Output = T>>,
    /// Indication of whether this weak future is still alive.
    alive: NonNull<AtomicBool>,
}

impl<T: 'static> WeakFuture<T> {
    /// Constructs a new `WeakFuture` from a non-null pointer to a scoped
    /// future.
    ///
    /// # Safety
    ///
    /// - `future` must point to a valid boxed future whose lifetime exceeds that
    ///   of the returned `WeakFuture`.
    /// - The returned `'static` reference is only sound because the caller
    ///   ensures coordinated destruction with `FutureHolder`.
    unsafe fn new<'scope, F>(alive: NonNull<AtomicBool>, future: &mut NonNull<F>) -> WeakFuture<T>
    where
        F: Future<Output = T> + 'scope,
    {
        type Src<'scope, T> = &'scope mut dyn Future<Output = T>;
        type Dst<T> = &'static mut dyn Future<Output = T>;

        let future = unsafe { future.as_mut() };
        let future = unsafe { transmute::<Src<'scope, T>, Dst<T>>(future) };

        WeakFuture {
            future: unsafe { Pin::new_unchecked(future) },
            alive,
        }
    }
}

impl<T> Drop for WeakFuture<T> {
    fn drop(&mut self) {
        // Release the polling flag when dropping.
        if Handle::current().runtime_flavor() == RuntimeFlavor::MultiThread {
            unsafe { self.alive.as_ref() }.store(false, Ordering::Release);
        } else {
            // Single-thread runtime: synchronous, free immediately.
            drop(unsafe { Box::from_raw(self.alive.as_mut()) });
        }
    }
}

unsafe impl<T: Send> Send for WeakFuture<T> {}

impl<T> Future for WeakFuture<T> {
    type Output = Option<T>;

    /// Polls the underlying future, wrapping the result in `Some`.
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Future::poll(self.future.as_mut(), cx).map(Some)
    }
}
