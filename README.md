# ScopedJoinSet

A **lifetime-aware**, `&mut`-scoped wrapper around [`tokio::task::JoinSet`] that allows you to spawn **non-`'static` futures** safely.

`ScopedJoinSet` is a drop-in, scope-bounded alternative to Tokio’s `JoinSet`. It enables async tasks that borrow data from the stack—without requiring cloning, `Arc`, or complicated ownership gymnastics—while guaranteeing the tasks cannot outlive the scope in which they were spawned.

---

## 🚨 Explicit Shutdown Required

A `ScopedJoinSet` **must not be dropped implicitly**.  
You **must** terminate it with **one** of:

- `set.shutdown().await`, or
- `set.join_all().await`.

If dropped without calling either method:

> **The process aborts.**

This rule is essential for soundness: spawned tasks may borrow stack data, so implicit drop is forbidden to prevent them from outliving their scope.

---

## Why ScopedJoinSet?

Tokio requires all spawned futures to be `'static`.  
This crate lifts that restriction:

- spawn tasks that borrow local variables,
- maintain predictable scope-bound lifetime,
- avoid unnecessary `Arc` or cloning,
- work entirely within safe Rust on the user side.

Scoped tasks behave like normal async tasks—just with lifetime guarantees.

---

## Key Guarantees

- Tasks may borrow from the stack (`'scope` is enforced by the API).
- Each task is executed by Tokio but **cannot escape its scope**.
- Result values are safely recovered in completion order.
- Tasks are automatically cleaned up during `shutdown` or `join_all`.
- Implicit drop is prohibited and enforced by an abort.

No background tasks. No `'static` requirement. No unsafe user code.

---

## Quick Example

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

    // Required to avoid abort:
    set.shutdown().await;
}
```

---

## Features

- Spawn **non-`'static`** futures.
- Deterministic scope-bounded lifetime.
- `join_next()` + `try_join_next()` yield tasks in completion order.
- Low overhead (no maps, indices, or intrusive metadata).
- Soundness validated with Miri.

---

## How It Works (Overview)

- Every task output lives in a unique heap allocation.
- The user future is wrapped in a pinned future that writes to that slot.
- The wrapper is unsafely promoted to `'static`, but soundly:
  - the allocation outlives the task,
  - tasks cannot outlive the `ScopedJoinSet`,
  - the wrapper future never moves after being pinned.
- The completed task returns a raw pointer identifying its output slot.
- `join_next()` reads the value and frees the allocation.

Simple, fast, and predictable.

---

## Comparison with Tokio’s JoinSet

| Feature                    | Tokio `JoinSet` | ScopedJoinSet |
| -------------------------- | --------------- | ------------- |
| Spawn non-`'static` tasks  | ❌ No           | ✅ Yes        |
| Borrows from stack allowed | ❌ No           | ✅ Yes        |
| Scoped lifetime enforced   | ❌ No           | ✅ Yes        |
| Implicit drop safe         | ✅ Yes          | ❌ Aborts     |

---

## Installation

```toml
[dependencies]
scoped-join-set = "0.7"
tokio = { version = "1", features = ["full"] }
```

---

## Soundness Notes

This crate uses `unsafe` internally but:

- all wrapper futures are pinned and immovable,
- no task can escape the scope (API prevents it),
- all allocations are freed once tasks finish or abort,
- extensive testing + Miri runs have found no UB.

However:

> **The crate has not undergone formal third-party audit.**

Use in critical contexts at your discretion.

---

## License

MIT OR Apache-2.0, at your option.
