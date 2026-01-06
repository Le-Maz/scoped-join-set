#![feature(async_fn_traits, unboxed_closures)]

//! # Scoped Async Tasks
//!
//! This module provides a mechanism for spawning scoped asynchronous tasks.
//! Unlike standard `tokio::spawn`, these tasks can borrow from the surrounding
//! environment (non-`'static` futures) because they are guaranteed to complete
//! before the scope exits.

mod future_wrappers;

use crate::future_wrappers::{SendPtr, WriteOutput};
use std::{
    any::Any, error::Error, fmt, future::Future, marker::PhantomData, mem::MaybeUninit,
    ptr::NonNull,
};
use tokio::task::JoinSet;

/// Alias for the specific JoinError type returned by Tokio.
type TokioJoinError = tokio::task::JoinError;

/// A scoped task set analogous to [`tokio::task::JoinSet`] but accepting
/// non-`'static` futures.
///
/// This structure manages the lifetime of the tasks and ensures their outputs
/// are correctly typed and returned.
#[derive(Default)]
pub struct ScopedJoinSet<'env, TaskResult>
where
    TaskResult: 'env + Send,
{
    /// The underlying Tokio JoinSet handling the actual execution.
    /// It stores `SendPtr` to type-erase the task output while maintaining `Send`.
    join_set: JoinSet<SendPtr>,
    /// Marker to ensure the struct respects the lifetime of the environment
    /// and the task result type.
    _marker: PhantomData<&'env TaskResult>,
}

impl<'env, TaskResult> ScopedJoinSet<'env, TaskResult>
where
    TaskResult: 'env + Send,
{
    /// Creates a new, empty `ScopedJoinSet`.
    fn new() -> Self {
        Self {
            join_set: JoinSet::new(),
            _marker: PhantomData,
        }
    }

    /// Spawns a new task into the set.
    ///
    /// The provided future must be `Send` and live for at least `'env`.
    /// The output of the task is stored in a heap-allocated slot to be retrieved later.
    pub fn spawn<F>(&mut self, task: F)
    where
        F: Future<Output = TaskResult> + Send + 'env,
        TaskResult: Send,
    {
        // Allocate a slot for the result. We leak the box to ensure the memory stays
        // valid until the task completes. The pointer is passed to the task wrapper.
        // It is reclaimed in `join_next` (or cleaned up by Tokio if aborted).
        let output_slot: NonNull<MaybeUninit<TaskResult>> =
            Box::leak(Box::new(MaybeUninit::uninit())).into();

        // Wrap the user's task to write the output to the allocated slot upon completion.
        let wrapped_task = WriteOutput::new(task, output_slot.cast());

        // Safety: We are casting lifetimes to 'static to satisfy Tokio's API.
        // The safety is guaranteed by `scope` awaiting all tasks before returning,
        // preventing any use-after-free of the environment.
        let static_task = unsafe { wrapped_task.into_static() };

        self.join_set.spawn(static_task);
    }

    /// Returns `true` if the set contains no tasks.
    pub fn is_empty(&self) -> bool {
        self.join_set.is_empty()
    }

    /// Returns the number of tasks currently in the set.
    pub fn len(&self) -> usize {
        self.join_set.len()
    }

    /// Waits for the next task to complete and returns its result.
    ///
    /// Returns `None` if the set is empty.
    pub async fn join_next(&mut self) -> Option<Result<TaskResult, JoinError>> {
        match self.join_set.join_next().await? {
            Ok(output_ptr) => {
                // Reconstruct the typed pointer from the raw SendPtr.
                let typed_ptr: NonNull<TaskResult> = output_ptr.cast();
                // Safety: We know this pointer was created in `spawn` via `Box::leak`.
                // We take ownership back to drop the box and retrieve the value.
                let output_value = unsafe { *Box::from_raw(typed_ptr.as_ptr()) };
                Some(Ok(output_value))
            }
            Err(join_error) => Some(Err(join_error.into())),
        }
    }

    /// Tries to join the next task without blocking.
    ///
    /// Returns `None` if no tasks have completed or the set is empty.
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

    /// Aborts all running tasks in the set.
    pub fn abort_all(&mut self) {
        self.join_set.abort_all();
    }

    /// Drains all tasks, waiting for each to complete.
    ///
    /// Returns a vector containing the results of all tasks.
    pub async fn join_all(&mut self) -> Vec<Result<TaskResult, JoinError>> {
        let mut results = Vec::with_capacity(self.len());
        while let Some(result) = self.join_next().await {
            results.push(result);
        }
        results
    }
}

/// Safety: The `ScopedJoinSet` is `Send` if the `TaskResult` is `Send`.
/// This allows the set to be moved between threads (though tasks themselves
/// run on the Tokio runtime).
unsafe impl<'env, TaskResult: Send> Send for ScopedJoinSet<'env, TaskResult> {}

/// Errors that can occur when joining a scoped task.
#[derive(Debug)]
pub enum JoinError {
    /// The task was cancelled before it could complete.
    Cancelled,
    /// The task panicked during execution.
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
///
/// This handle wraps a mutable reference to the `ScopedJoinSet`, ensuring
/// exclusive access to the set for operations like spawning and joining.
pub struct Scope<'scope, 'env, TaskResult>
where
    TaskResult: Send + 'env,
{
    /// Mutable reference to the underlying task set.
    task_set: &'scope mut ScopedJoinSet<'env, TaskResult>,
}

impl<'scope, 'env, TaskResult> Scope<'scope, 'env, TaskResult>
where
    TaskResult: Send + 'env,
{
    /// Spawns a future into the scope.
    ///
    /// This method is synchronous and non-blocking.
    pub fn spawn<F>(&mut self, future: F)
    where
        F: Future<Output = TaskResult> + Send + 'env,
    {
        self.task_set.spawn(future);
    }

    /// Waits for the next task to complete.
    pub async fn join_next(&mut self) -> Option<Result<TaskResult, JoinError>> {
        self.task_set.join_next().await
    }

    /// Tries to join the next task without blocking.
    pub fn try_join_next(&mut self) -> Option<Result<TaskResult, JoinError>> {
        self.task_set.try_join_next()
    }

    /// Returns `true` if the set is empty.
    pub fn is_empty(&self) -> bool {
        self.task_set.is_empty()
    }

    /// Returns the number of tasks in the set.
    pub fn len(&self) -> usize {
        self.task_set.len()
    }

    /// Aborts all tasks.
    pub fn abort_all(&mut self) {
        self.task_set.abort_all();
    }

    /// Joins all tasks and returns their results.
    pub async fn join_all(&mut self) -> Vec<Result<TaskResult, JoinError>> {
        self.task_set.join_all().await
    }
}

/// Creates a scope for spawning non-`'static` futures.
///
/// The provided `closure` receives a `Scope` handle which can be used to spawn
/// tasks. The scope guarantees that all spawned tasks will be joined or aborted
/// before the function returns.
///
/// # Arguments
///
/// * `closure` - An async closure that takes a mutable reference to `Scope`.
pub async fn scope<'env, TaskResult, Closure, ClosureResult>(closure: Closure) -> ClosureResult
where
    TaskResult: Send + 'env,
    // The closure must accept a mutable reference to Scope with the lifetime 's.
    Closure: for<'s> AsyncFnOnce(&'s mut Scope<'s, 'env, TaskResult>) -> ClosureResult,
{
    let mut task_set = ScopedJoinSet::<TaskResult>::new();

    let mut scope_handle = Scope {
        task_set: &mut task_set,
    };

    let result = closure(&mut scope_handle).await;

    // Ensure all tasks are cleaned up before returning.
    task_set.abort_all();
    task_set.join_all().await;

    result
}
