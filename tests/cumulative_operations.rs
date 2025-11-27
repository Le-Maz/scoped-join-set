use std::time::Duration;

use scoped_join_set::{JoinError, ScopedJoinSet};
use tokio::time::sleep;

#[tokio::test]
async fn join_all_completion_order() {
    let mut set = ScopedJoinSet::<u32>::new();

    set.spawn(async { 1 });
    set.spawn(async {
        sleep(Duration::from_millis(50)).await;
        2
    });

    let results = set.join_all().await;

    // Should contain both results, completion order is not guaranteed for the first one
    let mut values: Vec<_> = results.into_iter().map(|r| r.unwrap()).collect();
    values.sort();
    assert_eq!(values, vec![1, 2]);
}

#[tokio::test]
async fn abort_all() {
    let mut set = ScopedJoinSet::<u32>::new();

    for _ in 0..3 {
        set.spawn(async {
            sleep(Duration::from_millis(100)).await;
            5
        });
    }

    // Abort all tasks immediately
    set.abort_all();

    // Await remaining tasks
    let results = set.join_all().await;
    assert!(results
        .iter()
        .all(|r| matches!(r, Err(JoinError::Cancelled))));
}

#[tokio::test]
async fn shutdown_graceful_abort() {
    let mut set = ScopedJoinSet::<u32>::new();

    set.spawn(async {
        sleep(Duration::from_millis(50)).await;
        10
    });
    set.spawn(async {
        sleep(Duration::from_millis(100)).await;
        20
    });

    // Shutdown should abort and await all tasks
    let results = set.shutdown().await;

    // All tasks should be either cancelled or completed
    assert_eq!(results.len(), 2);
    for r in results {
        match r {
            Ok(v) => assert!(v == 10 || v == 20),
            Err(JoinError::Cancelled) | Err(JoinError::Panicked(_)) => {}
        }
    }
}
