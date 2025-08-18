use scoped_join_set::{JoinError, ScopedJoinSet};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::time::{sleep, Duration};

#[tokio::test]
async fn test_basic_completion() {
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
async fn test_task_drops_properly() {
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
async fn test_none_on_dropped_future() {
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
async fn test_join_next_returns_none_when_empty() {
    let mut set: ScopedJoinSet<i32> = ScopedJoinSet::new();

    assert!(set.join_next().await.is_none());
}

#[tokio::test]
async fn test_scope_lifetime_enforced() {
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
async fn test_tasks_run_concurrently() {
    let started = AtomicUsize::new(0);
    let finished = AtomicUsize::new(0);

    let mut set = ScopedJoinSet::new();

    for _ in 0..3 {
        set.spawn(async {
            (&started).fetch_add(1, Ordering::SeqCst);
            sleep(Duration::from_millis(50)).await;
            (&finished).fetch_add(1, Ordering::SeqCst);
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
async fn test_panicking_task() {
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

#[tokio::test]
async fn test_many_tasks_complete_successfully() {
    let mut set = ScopedJoinSet::new();
    let num_tasks = 10_000;

    for i in 0..num_tasks {
        set.spawn(async move { i });
    }

    let mut sum = 0usize;
    let mut count = 0;

    while !set.is_empty() {
        if let Some(Ok(val)) = set.join_next().await {
            sum += val;
            count += 1;
        }
    }

    // Sum of 0..num_tasks = n(n-1)/2
    assert_eq!(count, num_tasks);
    assert_eq!(sum, num_tasks * (num_tasks - 1) / 2);
}

#[tokio::test]
async fn test_many_tasks_with_panics() {
    let mut set = ScopedJoinSet::new();
    let num_tasks = 1000;

    for i in 0..num_tasks {
        set.spawn(async move {
            if i % 10 == 0 {
                panic!("panic at {}", i);
            }
            i
        });
    }

    let mut ok = 0;
    let mut panicked = 0;

    while !set.is_empty() {
        match set.join_next().await {
            Some(Ok(_)) => ok += 1,
            Some(Err(JoinError::Panicked(_))) => panicked += 1,
            _ => panic!("unexpected join result"),
        }
    }

    assert_eq!(ok + panicked, num_tasks);
    assert_eq!(panicked, num_tasks / 10);
}

#[tokio::test]
async fn test_dropping_set_while_tasks_pending() {
    let drop_count = Arc::new(AtomicUsize::new(0));

    struct DropMe(Arc<AtomicUsize>);
    impl Drop for DropMe {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    let mut set = ScopedJoinSet::new();
    let count = 5000;

    for _ in 0..count {
        let dropper = DropMe(drop_count.clone());
        set.spawn(async move {
            sleep(Duration::from_secs(1)).await; // won't complete
            drop(dropper);
            0
        });
    }

    // Drop before they complete
    drop(set);

    // All tasks should be dropped
    assert_eq!(drop_count.load(Ordering::SeqCst), count);
}

#[tokio::test]
async fn test_interleaved_spawn_and_join() {
    let mut set = ScopedJoinSet::new();

    for i in 0..1000 {
        set.spawn(async move { i });
        if i % 50 == 0 {
            if let Some(Ok(_)) = set.join_next().await {
                // Intentionally interleave
            }
        }
    }

    let mut count = 0;
    while !set.is_empty() {
        if let Some(Ok(_)) = set.join_next().await {
            count += 1;
        }
    }

    assert_eq!(count, 980);
}

#[tokio::test]
async fn test_many_scoped_references() {
    fn spawn_scoped_refs<'a>(data: &'a [usize]) -> ScopedJoinSet<'a, usize> {
        let mut set = ScopedJoinSet::new();

        for val in data {
            set.spawn(async move { *val });
        }

        set
    }

    let values: Vec<usize> = (0..500).collect();
    let mut set = spawn_scoped_refs(&values);

    let mut results = vec![];
    while let Some(res) = set.join_next().await {
        results.push(res.unwrap());
    }

    results.sort();
    assert_eq!(results, values);
}
