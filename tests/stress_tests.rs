#![cfg(not(miri))]

use scoped_join_set::ScopedJoinSet;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::time::{sleep, Duration};

#[tokio::test(flavor = "multi_thread")]
async fn many_tasks_complete_successfully() {
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

#[tokio::test(flavor = "multi_thread")]
async fn dropping_set_while_tasks_pending() {
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

#[tokio::test(flavor = "multi_thread")]
async fn interleaved_spawn_and_join() {
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

#[tokio::test(flavor = "multi_thread")]
async fn many_scoped_references() {
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
