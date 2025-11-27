# ScopedJoinSet

A **lifetime-aware** wrapper around [`tokio::task::JoinSet`] for spawning scoped async tasks in Rust.

Unlike standard Tokio tasks, which require `'static` futures, `ScopedJoinSet` allows dynamically spawning tasks whose futures are tied to a specific lifetime `'scope`. This provides a safe alternative to manual `unsafe` approaches while supporting advanced async orchestration.

---

## Features

- Spawn futures with **non-`'static` lifetimes**.
- **Cancellation-safe** task joining.
- Automatic cleanup on drop: aborts all running tasks.
- Supports both **awaited** and **non-blocking** task collection.
- Graceful shutdown of all tasks.
- **Miri-tested** to ensure memory safety.

---

## Installation

Add this to your `Cargo.toml`:

```toml
[dependencies]
scoped-join-set = "0.4"
tokio = { version = "1", features = ["full"] }
```
