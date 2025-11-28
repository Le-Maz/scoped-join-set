//! Internal utilities for spawning and managing scoped futures in [`ScopedJoinSet`](crate::ScopedJoinSet).
//!
//! This module enables safe `'static` polling of user futures while keeping ownership and memory
//! management under control. It exposes two main helpers:
//!
//! - [`split_spawn_task`] — takes a user future and a [`JoinSet`], spawns a `'static` reference
//!   of the future into the runtime, and returns a [`FutureHolder`] owning the allocation and the task [`Id`].
//!
//! - [`WeakFuture`] — a `'static` pinned reference to the user future stored in a heap allocation.
//!   This is the future that Tokio actually polls. Coordinated drops with [`FutureHolder`] are handled
//!   via an `AtomicBool` flag, ensuring memory safety.
//!
//! # Mechanism
//!
//! 1. The user future is wrapped in [`WriteOutput`] so its output is stored in a heap-allocated `Option<T>` instead of being returned.
//! 2. A contiguous memory block is allocated containing:
//!    - an `AtomicBool` alive flag  
//!    - the output storage `Option<T>`  
//!    - the pinned user future  
//! 3. [`WeakFuture`] points into this allocation and is spawned into the runtime.  
//! 4. [`FutureHolder`] owns the allocation, can abort the task, waits for the weak future to complete, and deallocates memory safely.

use std::{
    alloc::{alloc, handle_alloc_error, Layout},
    future::Future,
    marker::PhantomData,
    mem::transmute,
    pin::Pin,
    ptr::{metadata, NonNull},
    sync::atomic::{AtomicBool, Ordering},
    task::{Context, Poll},
};

use tokio::{
    runtime::{Handle, RuntimeFlavor},
    task::{block_in_place, AbortHandle, Id, JoinSet},
};

/// Splits a user future into a `WeakFuture` and a `FutureHolder`, spawning the weak reference into the given `JoinSet`.
pub(crate) fn split_spawn_task<'scope, F, T>(
    join_set: &mut JoinSet<()>,
    task: F,
) -> (Id, FutureHolder<'scope, T>)
where
    F: Future<Output = T> + Send + 'scope,
    T: Send + 'scope,
{
    let task_wrapper = WriteOutput::new(task);
    let allocation_info = allocate_future_block(task_wrapper);
    let weak_future = unsafe { WeakFuture::new(allocation_info.alive, allocation_info.future) };
    let handle = join_set.spawn(weak_future);
    let future_holder = FutureHolder::new(handle.clone(), allocation_info);
    (handle.id(), future_holder)
}

/// Metadata and pointers for a heap allocation holding the alive flag, output storage, and user future.
struct AllocationInfo<'scope, T> {
    base: NonNull<u8>,
    alive: NonNull<AtomicBool>,
    output: NonNull<Option<T>>,
    future: NonNull<dyn Future<Output = ()> + Send + 'scope>,
    layout: Layout,
}

/// Allocates memory for a user future wrapped in `WriteOutput` and returns metadata needed for `WeakFuture` and `FutureHolder`.
fn allocate_future_block<'scope, F, T>(
    mut task_wrapper: WriteOutput<F, T>,
) -> AllocationInfo<'scope, T>
where
    F: Future<Output = T> + Send + 'scope,
    T: Send + 'scope,
{
    let source_ptr =
        NonNull::from(&mut task_wrapper) as NonNull<dyn Future<Output = ()> + Send + 'scope>;
    let task_metadata = metadata(source_ptr.as_ptr());

    let layout = Layout::new::<AtomicBool>();
    let (layout, output_offset) = layout
        .extend(Layout::new::<Option<T>>().pad_to_align())
        .unwrap();
    let (layout, future_offset) = layout
        .extend(task_metadata.layout().pad_to_align())
        .unwrap();

    let allocation = unsafe { alloc(layout.pad_to_align()) };
    let allocation = NonNull::new(allocation).unwrap_or_else(|| handle_alloc_error(layout));

    let alive_ptr: NonNull<AtomicBool> = allocation.cast();
    unsafe { alive_ptr.write(AtomicBool::new(true)) };

    let output_ptr: NonNull<Option<T>> = unsafe { allocation.add(output_offset).cast() };
    unsafe { task_wrapper.set_output_ptr(output_ptr) };

    let future_byte_ptr = unsafe { allocation.add(future_offset) };
    let future_ptr: NonNull<dyn Future<Output = ()> + Send + 'scope> =
        NonNull::from_raw_parts(future_byte_ptr, task_metadata);

    unsafe {
        std::ptr::copy_nonoverlapping::<u8>(
            source_ptr.as_ptr().cast(),
            future_ptr.as_ptr().cast(),
            task_metadata.size_of(),
        );
    }

    std::mem::forget(task_wrapper);

    AllocationInfo {
        base: allocation,
        alive: alive_ptr,
        output: output_ptr,
        future: future_ptr,
        layout,
    }
}

/// Wraps a user future and stores its output in heap storage instead of returning it directly.
pub struct WriteOutput<F, T> {
    future: F,
    output_ptr: Option<NonNull<Option<T>>>,
}

unsafe impl<F, T> Send for WriteOutput<F, T> {}

impl<F, T> WriteOutput<F, T> {
    /// Wraps a user future with no output pointer yet.
    pub fn new(future: F) -> Self {
        Self {
            future,
            output_ptr: None,
        }
    }

    /// Sets the pointer to heap storage for the output.
    ///
    /// # Safety
    /// The pointer must remain valid until the future completes or is dropped.
    pub unsafe fn set_output_ptr(&mut self, ptr: NonNull<Option<T>>) {
        self.output_ptr = Some(ptr);
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

        match fut.poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(output) => {
                let ptr = this.output_ptr.expect("output pointer not set");
                unsafe { ptr.as_ptr().write(Some(output)) };
                Poll::Ready(())
            }
        }
    }
}

/// A `'static` future that references a user future in heap memory. Polled by Tokio without owning the allocation.
pub struct WeakFuture {
    alive: NonNull<AtomicBool>,
    future: NonNull<dyn Future<Output = ()> + Send + 'static>,
}

unsafe impl Send for WeakFuture {}

impl WeakFuture {
    /// Creates a weak future from a raw allocation.
    ///
    /// # Safety
    /// The allocation must outlive this weak reference. Drop is coordinated via `alive`.
    unsafe fn new<'scope>(
        alive: NonNull<AtomicBool>,
        future: NonNull<dyn Future<Output = ()> + Send + 'scope>,
    ) -> Self {
        let future: NonNull<dyn Future<Output = ()> + Send + 'static> = transmute(future);
        Self { alive, future }
    }
}

impl Future for WeakFuture {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this = unsafe { self.get_unchecked_mut() };
        let fut = unsafe { Pin::new_unchecked(this.future.as_mut()) };
        fut.poll(cx)
    }
}

impl Drop for WeakFuture {
    fn drop(&mut self) {
        if Handle::current().runtime_flavor() == RuntimeFlavor::MultiThread {
            unsafe { self.alive.as_ref().store(false, Ordering::Release) };
        }
    }
}

/// Owns a heap allocation containing a user future and its output storage.
/// Responsible for aborting the task, waiting for completion, and deallocating memory.
pub(crate) struct FutureHolder<'scope, T>
where
    T: Send + 'scope,
{
    abort_handle: AbortHandle,
    allocation: AllocationInfo<'scope, T>,
    _marker: PhantomData<&'scope ()>,
}

impl<'scope, T: Send + 'scope> FutureHolder<'scope, T> {
    fn new(abort_handle: AbortHandle, allocation: AllocationInfo<'scope, T>) -> Self {
        Self {
            abort_handle,
            allocation,
            _marker: PhantomData,
        }
    }

    /// Consumes the holder and returns the stored output if available.
    pub(crate) fn get_output(mut self) -> Option<T> {
        unsafe { self.allocation.output.as_mut() }.take()
    }

    /// Spins until the weak future signals termination.
    fn spin_until_termination(&self) {
        let alive: &AtomicBool = unsafe { self.allocation.base.cast().as_ref() };
        if alive.load(Ordering::Acquire) {
            block_in_place(|| {
                while alive.load(Ordering::Acquire) {
                    std::hint::spin_loop();
                }
            });
        }
    }

    /// Aborts the running task.
    pub(crate) fn abort(&self) {
        self.abort_handle.abort();
    }
}

impl<'scope, T: Send + 'scope> Drop for FutureHolder<'scope, T> {
    fn drop(&mut self) {
        self.abort_handle.abort();

        if Handle::current().runtime_flavor() == RuntimeFlavor::MultiThread {
            self.spin_until_termination();
        }

        unsafe {
            std::ptr::drop_in_place(self.allocation.output.as_ptr());
            std::ptr::drop_in_place(self.allocation.future.as_ptr());
            std::alloc::dealloc(self.allocation.base.cast().as_ptr(), self.allocation.layout);
        }
    }
}
