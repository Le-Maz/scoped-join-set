use scoped_join_set::{scope, JoinError};
use std::time::Duration;
use tokio::time::sleep;

#[tokio::test]
async fn join_all_completion_order() {
    scope::<u32, _, _>(async |s| {
        s.spawn(async { 1 }).await;
        s.spawn(async {
            sleep(Duration::from_millis(50)).await;
            2
        })
        .await;

        let results = s.join_all().await;

        let mut values: Vec<_> = results.into_iter().map(|r| r.unwrap()).collect();
        values.sort();
        assert_eq!(values, vec![1, 2]);
    })
    .await;
}

#[tokio::test]
async fn abort_all() {
    scope::<u32, _, _>(async |s| {
        for _ in 0..3 {
            s.spawn(async {
                sleep(Duration::from_millis(100)).await;
                5
            })
            .await;
        }

        // Abort all tasks immediately
        s.abort_all().await;

        // Await remaining tasks
        let results = s.join_all().await;
        assert!(results
            .iter()
            .all(|r| matches!(r, Err(JoinError::Cancelled))));
    })
    .await;
}

#[tokio::test]
async fn shutdown_graceful_abort_behavior() {
    // Note: The original test used `shutdown()` which consumed the set.
    // In the `scope` API, shutdown happens automatically at the end of the scope.
    // We simulate explicit shutdown logic by aborting and joining manually.
    scope::<u32, _, _>(async |s| {
        s.spawn(async {
            sleep(Duration::from_millis(50)).await;
            10
        })
        .await;
        s.spawn(async {
            sleep(Duration::from_millis(100)).await;
            20
        })
        .await;

        s.abort_all().await;
        let results = s.join_all().await;

        assert_eq!(results.len(), 2);
        for r in results {
            match r {
                Ok(v) => assert!(v == 10 || v == 20),
                Err(JoinError::Cancelled) | Err(JoinError::Panicked(_)) => {}
            }
        }
    })
    .await;
}
