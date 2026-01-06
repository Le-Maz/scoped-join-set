#![cfg(not(miri))]

use scoped_join_set::scope;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::time::{sleep, Duration};

#[tokio::test(flavor = "multi_thread")]
async fn many_tasks_complete_successfully() {
    let num_tasks = 10_000;

    scope(async |s| {
        for i in 0..num_tasks {
            s.spawn(async move { i });
        }

        let mut sum = 0usize;
        let mut count = 0;

        while !s.is_empty() {
            if let Some(Ok(val)) = s.join_next().await {
                sum += val;
                count += 1;
            }
        }

        // Sum of 0..num_tasks = n(n-1)/2
        assert_eq!(count, num_tasks);
        assert_eq!(sum, num_tasks * (num_tasks - 1) / 2);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn dropping_scope_while_tasks_pending() {
    let drop_count = Arc::new(AtomicUsize::new(0));

    struct DropMe(Arc<AtomicUsize>);
    impl Drop for DropMe {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    let count = 5000;

    scope::<i32, _, _>(async |s| {
        for _ in 0..count {
            let dropper = DropMe(drop_count.clone());
            s.spawn(async move {
                sleep(Duration::from_secs(10)).await; // won't complete
                drop(dropper);
                0
            });
        }

        // We simulate "shutdown" by aborting everything before leaving the scope.
        // Implicitly, `scope` also cleans up, but for the test we want to assert
        // drop counts after everything returns.
        s.abort_all();
    })
    .await;

    // All tasks should be dropped
    assert_eq!(drop_count.load(Ordering::SeqCst), count);
}

#[tokio::test(flavor = "multi_thread")]
async fn interleaved_spawn_and_join() {
    scope::<usize, _, _>(async |s| {
        for i in 0..1000 {
            s.spawn(async move { i });
            if i % 50 == 0 {
                if let Some(Ok(_)) = s.join_next().await {
                    // Intentionally interleave
                }
            }
        }

        let mut count = 0;
        while !s.is_empty() {
            if let Some(Ok(_)) = s.join_next().await {
                count += 1;
            }
        }

        assert_eq!(count, 980);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn many_scoped_references() {
    let values: Vec<usize> = (0..500).collect();

    // We rewrite the helper function to be a direct scope call
    scope(async |s| {
        for val in &values {
            s.spawn(async move { *val });
        }

        let mut results = vec![];
        while let Some(res) = s.join_next().await {
            results.push(res.unwrap());
        }

        results.sort();
        assert_eq!(results, values);
    })
    .await;
}
