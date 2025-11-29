# ScopedJoinSet

A **lifetime-aware**, `&mut`-scoped wrapper around [`tokio::task::JoinSet`] that allows you to spawn **non-`'static` async tasks** safely.

`ScopedJoinSet` provides the ergonomic benefits of Tokio’s `JoinSet`, but lifts the `'static` bound on futures in a sound and predictable way. This enables async tasks that borrow stack-allocated data, while ensuring those tasks never outlive their parent scope.

---

## Why ScopedJoinSet?

Tokio’s default task API requires all spawned futures to be `'static`. This makes many patterns impossible without cloning, `Arc`, or complex restructuring.

`ScopedJoinSet` enables:

* async tasks that borrow temporary variables,
* task orchestration within a bounded scope,
* predictable task lifetimes,
* deterministic cleanup.

All without unsafe user code.

---

## Key Guarantees

Every task spawned through `ScopedJoinSet`:

* **cannot outlive** the data it captures (`'scope` is enforced at compile time),
* **completes exactly once**, and its output is recoverable,
* is **cancellation-safe** via `join_next`, `try_join_next`, or `shutdown`,
* is **aborted and cleaned up** when the `ScopedJoinSet` is dropped.

This crate does **not** spawn background tasks.
Tasks always remain within the lifetime of the `ScopedJoinSet`.

---

## ⚠️ Recommendation: Prefer `shutdown().await`

`ScopedJoinSet::shutdown()` allows all tasks to finish or abort **asynchronously**.

If the `ScopedJoinSet` is dropped without shutdown:

* all tasks are aborted immediately,
* and the drop implementation uses a **spin loop (`block_in_place`)** on multithreaded runtimes to wait for completion.

To avoid blocking executor threads, prefer:

```rust
set.shutdown().await;
```

---

## Features

* Spawn futures with **non-`'static` lifetimes**.
* Lightweight and predictable; no hashmaps or intrusive metadata.
* **Cancellation-safe** joining.
* `join_next()` and `try_join_next()` iterate tasks in completion order.
* **Graceful or abrupt shutdown**.
* Verified using Miri to hunt down UB or memory leakage.

---

## Example

```rust
use scoped_join_set::ScopedJoinSet;

#[tokio::main]
async fn main() {
    let value = Box::new(5);
    let borrowed = &value;

    let mut set = ScopedJoinSet::<u32>::new();

    set.spawn(async move {
        println!("Borrowed: {}", borrowed);
        **borrowed
    });

    while let Some(result) = set.join_next().await {
        assert_eq!(result.unwrap(), *value);
    }
}
```

Tasks may borrow stack data safely because they cannot escape the scope of the `ScopedJoinSet`.

---

## How It Works (Briefly)

* Each task’s output is stored in a **heap-allocated slot**.
* The user future is wrapped in a small pinned future that writes into that slot.
* That wrapper is **unsafely cast to `'static`**, but this cast is sound because:
  * the allocation outlives the task,
  * the task cannot outlive the `ScopedJoinSet`,
  * the wrapper future itself never moves once pinned.
* `join_next()` retrieves the raw pointer returned by the task, reads the output, and frees the allocation.

This design keeps the overhead low — typically **within ~5–7%** of Tokio’s native `JoinSet`.

A more detailed explanation lives in the crate docs.

---

## Comparison to Tokio’s `JoinSet`

| Feature                    | Tokio `JoinSet` | Scoped `JoinSet` |
| -------------------------- | --------------- | ---------------- |
| Requires `'static` futures | ✅ Yes           | ❌ No             |
| Allows stack borrows       | ❌ No            | ✅ Yes            |
| Deterministic scope-bound  | ❌ No            | ✅ Yes            |

---

## Installation

```toml
[dependencies]
scoped-join-set = "0.6"
tokio = { version = "1", features = ["full"] }
```

---

## Soundness

`ScopedJoinSet` relies on `unsafe` internally, but:

* all pinned wrappers have stable addresses,
* tasks cannot escape the scope through the API,
* all allocations are freed after task completion or abortion,
* Miri runs show no UB.

Still, this crate has **not** been extensively audited.
See the warning below.

---

## ⚠️ Warning

This crate has **not undergone formal peer review or production-level verification**.
It is tested for memory safety (including Miri), but use in security-critical or untrusted contexts carries risk.

Please **test thoroughly** in your environment before deploying.

---

## License

MIT OR Apache-2.0, at your option.
