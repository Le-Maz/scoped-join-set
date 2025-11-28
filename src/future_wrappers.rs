//! Internal utilities for wrapping user futures so they can be spawned
//! `'static` inside `ScopedJoinSet` without requiring users to write
//! `'static` futures.
//!
//! # Overview
//!
//! `WriteOutput<F, T>` wraps a user future `F: Future<Output = T>` and stores
//! its output in a heap-allocated `Option<T>` instead of returning it
//! directly.  
//!
//! The wrapper future always returns `()` — the result is written into the
//! provided pointer and later extracted by `ScopedJoinSet`.
//!
//! # Why this is needed
//!
//! Tokio requires all spawned tasks to be `'static`.  
//! `ScopedJoinSet` accepts futures with any lifetime `'scope`.  
//!
//! To make this work safely:
//!
//! - The user future is moved into a pinned `Box`, ensuring it never moves.
//! - We unsafely cast that pinned box to `'static`. This is valid because
//!   `ScopedJoinSet` guarantees that the boxed allocation lives until after
//!   the future completes or is aborted.
//! - The future writes its output into `Box<Option<T>>`, which the join
//!   logic takes ownership of.
//!
//! This gives `'static` pollability without `'static` user requirements.

use std::{
    future::Future,
    pin::Pin,
    ptr::NonNull,
    task::{ready, Context, Poll},
};

/// Wraps a user future and stores its output in heap storage instead of
/// returning it directly.
pub struct WriteOutput<F, T> {
    future: F,
    output_ptr: NonNull<Option<T>>,
}

unsafe impl<F, T> Send for WriteOutput<F, T> {}

impl<'scope, F, T> WriteOutput<F, T>
where
    F: Future<Output = T> + Send + 'scope,
    T: 'scope,
{
    /// Create a new wrapper around the user future, paired with an output
    /// storage pointer.
    pub fn new(future: F, output_ptr: NonNull<Option<T>>) -> Self {
        Self { future, output_ptr }
    }

    /// Converts this wrapper future into a `'static` future so Tokio can spawn it.
    ///
    /// SAFETY: This is safe in the context of `ScopedJoinSet` only if:
    /// - the boxed allocation containing the future lives for the duration
    ///   of the spawned task.
    /// - no references inside the future outlive `'scope`.
    ///
    /// `ScopedJoinSet` guarantees these properties.
    pub unsafe fn into_static(self) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        unsafe {
            std::mem::transmute::<
                Pin<Box<dyn Future<Output = ()> + Send + 'scope>>,
                Pin<Box<dyn Future<Output = ()> + Send + 'static>>,
            >(Box::pin(self))
        }
    }
}

impl<F, T> Future for WriteOutput<F, T>
where
    F: Future<Output = T>,
{
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this = unsafe { self.get_unchecked_mut() };
        let fut = unsafe { Pin::new_unchecked(&mut this.future) };

        let output = ready!(fut.poll(cx));
        unsafe { this.output_ptr.write(Some(output)) };
        Poll::Ready(())
    }
}
