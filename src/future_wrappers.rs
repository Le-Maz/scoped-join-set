//! Internal utilities for wrapping user futures so they can be spawned
//! `'static` inside `ScopedJoinSet` without requiring users to write
//! `'static` futures.
//!
//! # Overview
//!
//! `WriteOutput<F, T>` wraps a user future `F: Future<Output = T>` and writes
//! its output into a caller-provided heap allocation rather than returning it
//! directly.
//!
//! The wrapper future itself always returns a `SendPtr`, which is simply a
//! raw non-null pointer identifying the output slot. `ScopedJoinSet` uses
//! this pointer to recover and deallocate the stored result after the task
//! completes.
//!
//! # Why this is needed
//!
//! Tokio only allows spawning `'static` futures.
//! `ScopedJoinSet` accepts futures with arbitrary lifetimes `'scope`.
//!
//! To bridge this safely without extra bookkeeping structures:
//!
//! - The user future is wrapped and placed in a pinned `Box`, ensuring it
//!   never moves in memory.
//! - This pinned box is unsafely cast to `'static`; this is sound because
//!   `ScopedJoinSet` guarantees that the allocation outlives the spawned
//!   task.
//! - The wrapper writes the future's output into a dedicated heap
//!   allocation owned by the caller.
//! - The wrapper returns a raw pointer to that storage. No hashmap or
//!   external indexing is needed: the pointer itself uniquely identifies
//!   the result slot.
//! - After the task completes, `ScopedJoinSet` reads the output from that
//!   pointer and frees the allocation.
//!
//! This provides `'static` pollability without requiring `'static` user
//! futures and without maintaining any auxiliary data structures.

use std::{
    future::Future,
    ops::Deref,
    pin::Pin,
    ptr::NonNull,
    task::{ready, Context, Poll},
};

/// A `Send`-safe wrapper around a non-null raw pointer.
///
/// This is used as the `Output` type of a wrapped task. Each completed task
/// returns a `SendPtr` identifying the heap slot containing its output.
///
/// The pointer uniquely identifies the result and no additional lookup
/// structures are required.
#[repr(transparent)]
pub(crate) struct SendPtr {
    /// The underlying raw non-null pointer to the task output.
    inner: NonNull<u8>,
}

impl Deref for SendPtr {
    type Target = NonNull<u8>;

    /// Dereferences the `SendPtr` to access the underlying `NonNull` pointer.
    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl SendPtr {
    /// Creates a new `SendPtr` from a non-null pointer.
    ///
    /// # Safety
    ///
    /// This function is marked `unsafe` because it is an internal constructor
    /// that wraps a raw pointer which must be valid for the duration of its use
    /// in the `ScopedJoinSet`.
    #[inline]
    unsafe fn new(inner: NonNull<u8>) -> Self {
        Self { inner }
    }
}

/// Safety: We assert that transferring this pointer across threads is safe.
/// This relies on the internal logic of `ScopedJoinSet` ensuring thread safety
/// of the data being pointed to.
unsafe impl Send for SendPtr {}

/// Wraps a user future so it can be spawned as `'static` while still
/// producing a non-`'static` result.
///
/// The wrapper:
/// - polls the user future,
/// - writes its output into the provided heap slot,
/// - returns a raw pointer (`SendPtr`) identifying that slot.
///
/// If the future is dropped before completing (e.g., due to task abortion),
/// the wrapper deallocates the unused slot.
pub(crate) struct WriteOutput<FutureType, TaskOutput> {
    /// The actual future provided by the user to be polled.
    future: FutureType,
    /// A pointer to the heap location where the result should be written.
    output_ptr: NonNull<TaskOutput>,
    /// Tracks whether the future completed successfully to handle cleanup on drop.
    success: bool,
}

/// Safety: `WriteOutput` is `Send` if the underlying future is `Send`.
/// The `output_ptr` is considered safe to send because it points to a
/// heap allocation managed exclusively by this wrapper and the `ScopedJoinSet`.
unsafe impl<FutureType, TaskOutput> Send for WriteOutput<FutureType, TaskOutput> {}

impl<'scope, FutureType, TaskOutput> WriteOutput<FutureType, TaskOutput>
where
    FutureType: Future<Output = TaskOutput> + Send + 'scope,
    TaskOutput: 'scope,
{
    /// Creates a new wrapper over the given future and output pointer.
    ///
    /// `output_ptr` must point to a valid, writable heap allocation that will
    /// outlive the spawned task.
    ///
    /// # Safety
    ///
    /// This is an internal function marked `unsafe`. The caller must ensure
    /// that `output_ptr` is valid and that the resulting `WriteOutput` is
    /// managed correctly to prevent memory leaks or use-after-free errors.
    #[inline]
    pub(crate) unsafe fn new(future: FutureType, output_ptr: NonNull<TaskOutput>) -> Self {
        Self {
            future,
            output_ptr,
            success: false,
        }
    }

    /// Converts this wrapper into a `'static` future so that Tokio can spawn it.
    ///
    /// # Safety
    ///
    /// Safe only when the allocation containing the wrapper is guaranteed to
    /// outlive the spawned task. `ScopedJoinSet` enforces this property.
    #[inline]
    pub unsafe fn into_static(self) -> Pin<Box<dyn Future<Output = SendPtr> + Send>> {
        unsafe {
            std::mem::transmute::<
                Pin<Box<dyn Future<Output = SendPtr> + Send + 'scope>>,
                Pin<Box<dyn Future<Output = SendPtr> + Send + 'static>>,
            >(Box::pin(self))
        }
    }
}

impl<FutureType, TaskOutput> Future for WriteOutput<FutureType, TaskOutput>
where
    FutureType: Future<Output = TaskOutput>,
{
    type Output = SendPtr;

    /// Polls the wrapped future. When it completes:
    /// - writes its output into the heap slot,
    /// - marks the wrapper as successfully completed,
    /// - returns a pointer identifying the output slot.
    #[inline]
    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<SendPtr> {
        let this = unsafe { self.get_unchecked_mut() };
        // Create a pinned reference to the inner future to poll it.
        let future_pinned = unsafe { Pin::new_unchecked(&mut this.future) };

        let output = ready!(future_pinned.poll(context));
        unsafe { this.output_ptr.write(output) };
        this.success = true;

        Poll::Ready(unsafe { SendPtr::new(this.output_ptr.cast()) })
    }
}

impl<FutureType, TaskOutput> Drop for WriteOutput<FutureType, TaskOutput> {
    /// On drop, if the user future never completed, the associated heap output
    /// allocation is reclaimed.
    ///
    /// This ensures that aborted tasks do not leak memory.
    #[inline]
    fn drop(&mut self) {
        if !self.success {
            unsafe { drop(Box::from_raw(self.output_ptr.as_ptr())) };
        }
    }
}
