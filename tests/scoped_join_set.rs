use scoped_join_set::{JoinError, ScopedJoinSet};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::time::{sleep, Duration};

#[tokio::test]
async fn basic_completion() {
    let mut set = ScopedJoinSet::new();

    set.spawn(async { 1 });
    set.spawn(async { 2 });

    let mut results = vec![];
    while !set.is_empty() {
        if let Some(Ok(val)) = set.join_next().await {
            results.push(val);
        }
    }

    results.sort();
    assert_eq!(results, vec![1, 2]);
}

#[tokio::test]
async fn task_drops_properly() {
    let dropped = Arc::new(AtomicUsize::new(0));

    struct DropCounter(Arc<AtomicUsize>);

    impl Drop for DropCounter {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    let mut set = ScopedJoinSet::new();

    for _ in 0..5 {
        let drop_counter = DropCounter(dropped.clone());
        set.spawn(async move {
            drop(drop_counter); // Will drop at end of task
            42
        });
    }

    while !set.is_empty() {
        set.join_next().await;
    }

    assert_eq!(dropped.load(Ordering::SeqCst), 5);
}

#[tokio::test]
async fn none_on_dropped_future() {
    let mut set = ScopedJoinSet::new();

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();

    set.spawn(async move {
        let _ = rx.await;
        10
    });

    // Drop the set before joining
    drop(set);
    drop(tx); // complete the task so it doesn't hang
}

#[tokio::test]
async fn join_next_returns_none_when_empty() {
    let mut set: ScopedJoinSet<i32> = ScopedJoinSet::new();

    assert!(set.join_next().await.is_none());
}

#[tokio::test]
async fn scope_lifetime_enforced() {
    fn scoped_spawn<'a>(val: &'a i32) -> ScopedJoinSet<'a, i32> {
        let mut set = ScopedJoinSet::new();
        set.spawn(async move { *val });
        set
    }

    let value = 99;
    let mut set = scoped_spawn(&value);

    assert!(matches!(set.join_next().await, Some(Ok(99))));
}

#[tokio::test]
async fn tasks_run_concurrently() {
    let started = AtomicUsize::new(0);
    let finished = AtomicUsize::new(0);

    let mut set = ScopedJoinSet::new();

    for _ in 0..3 {
        set.spawn(async {
            started.fetch_add(1, Ordering::SeqCst);
            sleep(Duration::from_millis(50)).await;
            finished.fetch_add(1, Ordering::SeqCst);
            1
        });
    }

    sleep(Duration::from_millis(10)).await;
    assert_eq!(started.load(Ordering::SeqCst), 3);

    while !set.is_empty() {
        set.join_next().await;
    }

    assert_eq!(finished.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn panicking_task() {
    let mut set = ScopedJoinSet::new();

    set.spawn(async {
        panic!("test panic");
    });

    let res = set.join_next().await;
    assert!(
        matches!(res, Some(Err(JoinError::Panicked(_)))),
        "Panicking task should return JoinError::Panicked"
    );
}
