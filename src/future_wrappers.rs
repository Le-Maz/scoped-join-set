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

pub fn split_spawn_task<'scope, F, T>(
    join_set: &mut JoinSet<Option<T>>,
    task: F,
) -> (Id, FutureHolder<'scope, T>)
where
    F: Future<Output = T> + Send + 'scope,
    T: Send + 'static,
{
    let alive: NonNull<AtomicBool> = Box::leak(Box::new(AtomicBool::new(true))).into();
    let mut future: NonNull<F> = Box::leak(Box::new(task)).into();
    let weak_future = unsafe { WeakFuture::new(alive, &mut future) };
    let handle = join_set.spawn(weak_future);
    (handle.id(), FutureHolder::new(alive, future, &handle))
}

pub(crate) struct FutureHolder<'scope, T>
where
    T: 'static,
{
    abort_handle: AbortHandle,
    future: NonNull<dyn Future<Output = T> + Send + 'scope>,
    alive: NonNull<AtomicBool>,
}

impl<'scope, T> FutureHolder<'scope, T> {
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
        self.abort_handle.abort();

        if Handle::current().runtime_flavor() == RuntimeFlavor::MultiThread {
            self.spin_until_termination();
            drop(unsafe { Box::from_raw(self.alive.as_mut()) });
        }

        drop(unsafe { Box::from_raw(self.future.as_ptr()) });
    }
}

pub(crate) struct WeakFuture<T: 'static> {
    future: Pin<&'static mut dyn Future<Output = T>>,
    alive: NonNull<AtomicBool>,
}

impl<T: 'static> WeakFuture<T> {
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
        Future::poll(self.future.as_mut(), cx).map(Some)
    }
}
