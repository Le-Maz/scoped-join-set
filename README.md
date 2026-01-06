# ScopedJoinSet

A **lifetime-aware**, scoped concurrency wrapper for [`tokio::task::JoinSet`](https://docs.rs/tokio/latest/tokio/task/struct.JoinSet.html).

This crate enables spawning **non-`'static` futures** that can borrow data from the stack, allowing for safe, efficient, and scoped parallel execution without the need for `Arc`, `Mutex`, or cloning.

It provides a functional `scope` API (similar to `std::thread::scope`) that ensures all tasks complete before the scope exits.

---

## ⚠️ Critical Safety Warning: Cancellation Safety

The `scope` function provided by this crate is **not cancellation safe**.

If the future returned by `scope(...)` is dropped before it completes (for example, if wrapped in a `tokio::time::timeout` or `tokio::select!` branch that gets cancelled), the process will **abort** immediately.

### Why?
This strict behavior is required for soundness. Tasks spawned within the scope may borrow local variables from the surrounding stack. If the scope were dropped implicitly without waiting for tasks to finish, those tasks could continue running and access invalid memory (use-after-free). The library enforces an abort to prevent this undefined behavior.

---

## Features

-   **Spawn non-`'static` futures:** Tasks can borrow local variables directly from the stack.
-   **Functional `scope` API:** A high-level, easy-to-use closure-based API.
-   **No `Arc` required:** Avoids the overhead and complexity of reference counting for local data.
-   **Type-Safe Results:** Tasks return typed results, recovered safely upon completion.
-   **Zero-Cost Abstractions:** Uses strict pinning and pointer casting internally to avoid hash maps or heavy tracking structures.

---

## Usage

The primary entry point is the `scope` function. It ensures that all spawned tasks are joined before the scope exits.

```rust
use scoped_join_set::scope;

#[tokio::main]
async fn main() {
    let inputs = vec![1, 2, 3, 4, 5];
    let multiplier = 10;

    // We can borrow 'inputs' and 'multiplier' inside the scope
    // without cloning or wrapping them in Arc.
    let sum_result = scope(|scope_handle| async move {
        for input_item in &inputs {
            scope_handle.spawn(async move {
                // Borrowing 'multiplier' from the stack
                input_item * multiplier
            });
        }

        let mut total = 0;
        // Join tasks as they complete
        while let Some(result) = scope_handle.join_next().await {
            match result {
                Ok(value) => total += value,
                Err(e) => eprintln!("Task failed: {}", e),
            }
        }
        total
    }).await;

    assert_eq!(sum_result, 150);
}
```

---

## How It Works

Tokio requires all spawned tasks to be `'static`. To bypass this limitation safely, `scope` employs the following mechanism:

1.  **Heap Allocation:** When a task is spawned, a slot for its result is allocated on the heap.
2.  **Pointer Erasure:** The user's future is wrapped in a structure that holds a raw pointer (`SendPtr`) to the result slot.
3.  **Static Promotion:** The wrapped future is pinned and unsafely cast to `'static`. This is sound because the `scope` guarantees the environment outlives the task execution.
4.  **Completion:** Upon task completion, the result is written to the pointer. The scope retrieves this result and deallocates the slot.
5.  **Safety Guard:** An `AbortOnDrop` guard is active during the `scope` execution. If the scope future is dropped (cancelled) while tasks are running, the guard triggers `std::process::abort()` to prevent invalid memory access.

---

## Comparison with Tokio `JoinSet`

| Feature | Tokio `JoinSet` | `scoped-join-set` |
| :--- | :--- | :--- |
| **Spawn non-`'static` tasks** | ❌ No | ✅ Yes |
| **Borrow from stack** | ❌ No | ✅ Yes |
| **Scoped lifetime** | ❌ No | ✅ Yes |
| **Drop Behavior** | Safe (Detaches/Cancels) | **Aborts Process** if dropped early |

---

## License

MIT OR Apache-2.0, at your option.
