# ScopedJoinSet

A **lifetime-aware** wrapper around [`tokio::task::JoinSet`] for spawning **scoped async tasks** in Rust.

`ScopedJoinSet` focuses on **correct task semantics** for non-`'static` futures. It ensures that each spawned task:

* Completes exactly once, and its output can be collected in completion order.
* Cannot outlive the lifetime of references it captures (`'scope`).
* Can be cancelled, aborted, or collected deterministically.

Unlike standard Tokio tasks, which require `'static` futures, this crate enables advanced async orchestration where tasks depend on temporary stack data, while guaranteeing predictable task lifetimes and output delivery.

---

> **Recommendation:** Prefer using `shutdown().await` instead of relying on `Drop` to terminate tasks.
> This allows the runtime to complete tasks asynchronously. Dropping the set will block the current thread with a spin loop (`block_in_place`) on multithreaded runtimes, which may interfere with other tasks.

---

## Features

* Spawn futures with **non-`'static` lifetimes**.
* **Cancellation-safe** task joining with `join_next` or `try_join_next`.
* Automatic cleanup on drop: **aborts all running tasks**.
* Supports both **awaited** and **non-blocking** task collection.
* **Graceful shutdown** of all tasks via `shutdown()`.
* **Memory-safe**, even under Miri.

---

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
scoped-join-set = "0.4"
tokio = { version = "1", features = ["full"] }
```

---

## License

MIT OR Apache-2.0

---

## ⚠️ Warning

This crate has **not undergone formal peer review or extensive production testing**.
While it has been tested for basic correctness and memory safety (including Miri), use in **critical systems or untrusted environments** carries risk.

Use at your own discretion and thoroughly test in your own context before deploying.
