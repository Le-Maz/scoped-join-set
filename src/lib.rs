#![feature(async_fn_traits, unboxed_closures)]

mod future_wrappers;

use crate::future_wrappers::{SendPtr, WriteOutput};
use std::{
    any::Any, error::Error, fmt, future::Future, marker::PhantomData, mem::MaybeUninit,
    ptr::NonNull,
};
use tokio::sync::Mutex; // Changed to tokio Mutex for async lock support
use tokio::task::JoinSet;

type TokioJoinError = tokio::task::JoinError;

/// A scoped task set analogous to [`tokio::task::JoinSet`] but accepting
/// non-`'static` futures.
#[derive(Default)]
struct ScopedJoinSet<'env, TaskResult>
where
    TaskResult: 'env + Send,
{
    join_set: JoinSet<SendPtr>,
    _marker: PhantomData<&'env TaskResult>,
}

impl<'env, TaskResult> ScopedJoinSet<'env, TaskResult>
where
    TaskResult: 'env + Send,
{
    fn new() -> Self {
        Self {
            join_set: JoinSet::new(),
            _marker: PhantomData,
        }
    }

    pub fn spawn<F>(&mut self, task: F)
    where
        F: Future<Output = TaskResult> + Send + 'env,
        TaskResult: Send,
    {
        let output_slot: NonNull<MaybeUninit<TaskResult>> =
            Box::leak(Box::new(MaybeUninit::uninit())).into();

        let wrapped_task = WriteOutput::new(task, output_slot.cast());

        let static_task = unsafe { wrapped_task.into_static() };

        self.join_set.spawn(static_task);
    }

    pub fn is_empty(&self) -> bool {
        self.join_set.is_empty()
    }

    pub fn len(&self) -> usize {
        self.join_set.len()
    }

    pub async fn join_next(&mut self) -> Option<Result<TaskResult, JoinError>> {
        match self.join_set.join_next().await? {
            Ok(output_ptr) => {
                let typed_ptr: NonNull<TaskResult> = output_ptr.cast();
                let output_value = unsafe { *Box::from_raw(typed_ptr.as_ptr()) };
                Some(Ok(output_value))
            }
            Err(join_error) => Some(Err(join_error.into())),
        }
    }

    pub fn try_join_next(&mut self) -> Option<Result<TaskResult, JoinError>> {
        match self.join_set.try_join_next()? {
            Ok(output_ptr) => {
                let typed_ptr: NonNull<TaskResult> = output_ptr.cast();
                let output_value = unsafe { *Box::from_raw(typed_ptr.as_ptr()) };
                Some(Ok(output_value))
            }
            Err(join_error) => Some(Err(join_error.into())),
        }
    }

    /// Aborts all running tasks.
    pub fn abort_all(&mut self) {
        self.join_set.abort_all();
    }

    /// Drains all tasks.
    pub async fn join_all(&mut self) -> Vec<Result<TaskResult, JoinError>> {
        let mut results = Vec::with_capacity(self.len());
        while let Some(result) = self.join_next().await {
            results.push(result);
        }
        results
    }
}

unsafe impl<'env, TaskResult: Send> Send for ScopedJoinSet<'env, TaskResult> {}

#[derive(Debug)]
pub enum JoinError {
    Cancelled,
    Panicked(Box<dyn Any + Send + 'static>),
}

impl fmt::Display for JoinError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JoinError::Cancelled => write!(formatter, "task was cancelled"),
            JoinError::Panicked(_) => write!(formatter, "task panicked"),
        }
    }
}

impl Error for JoinError {}

impl From<TokioJoinError> for JoinError {
    fn from(error: TokioJoinError) -> Self {
        if error.is_cancelled() {
            JoinError::Cancelled
        } else if error.is_panic() {
            JoinError::Panicked(error.into_panic())
        } else {
            JoinError::Cancelled
        }
    }
}

/// A handle passed to the closure in [`scope`], allowing the spawning of
/// scoped tasks and interaction with the set.
pub struct Scope<'scope, 'env, TaskResult>
where
    TaskResult: Send + 'env,
{
    task_set: &'scope Mutex<ScopedJoinSet<'env, TaskResult>>,
}

impl<'scope, 'env, TaskResult> Scope<'scope, 'env, TaskResult>
where
    TaskResult: Send + 'env,
{
    /// Spawns a future into the scope.
    pub async fn spawn<F>(&self, future: F)
    where
        F: Future<Output = TaskResult> + Send + 'env,
    {
        self.task_set.lock().await.spawn(future);
    }

    /// Waits for the next task to complete.
    pub async fn join_next(&self) -> Option<Result<TaskResult, JoinError>> {
        self.task_set.lock().await.join_next().await
    }

    /// Tries to join the next task without blocking.
    pub fn try_join_next(&self) -> Option<Result<TaskResult, JoinError>> {
        // We can only try if we can acquire the lock immediately.
        if let Ok(mut set) = self.task_set.try_lock() {
            set.try_join_next()
        } else {
            None
        }
    }

    /// Returns `true` if the set is empty.
    pub async fn is_empty(&self) -> bool {
        self.task_set.lock().await.is_empty()
    }

    /// Returns the number of tasks in the set.
    pub async fn len(&self) -> usize {
        self.task_set.lock().await.len()
    }

    /// Aborts all tasks.
    pub async fn abort_all(&self) {
        self.task_set.lock().await.abort_all();
    }

    /// Joins all tasks and returns their results.
    pub async fn join_all(&self) -> Vec<Result<TaskResult, JoinError>> {
        self.task_set.lock().await.join_all().await
    }
}

/// Creates a scope for spawning non-`'static` futures.
pub async fn scope<'env, TaskResult, Closure, ClosureResult>(closure: Closure) -> ClosureResult
where
    TaskResult: Send + 'env,
    Closure: for<'s> AsyncFnOnce(&'s Scope<'s, 'env, TaskResult>) -> ClosureResult,
{
    let task_set = Mutex::new(ScopedJoinSet::<TaskResult>::new());

    let scope_handle = Scope {
        task_set: &task_set,
    };

    let result = closure(&scope_handle).await;

    task_set.lock().await.abort_all();
    task_set.lock().await.join_all().await;

    result
}
