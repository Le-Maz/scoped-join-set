use scoped_join_set::{scope, JoinError};
use std::time::Duration;
use tokio::time::sleep;

#[tokio::test]
async fn join_all_completion_order() {
    scope::<u32, _, _>(async |s| {
        s.spawn(async { 1 });
        s.spawn(async {
            sleep(Duration::from_millis(50)).await;
            2
        });

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
            });
        }

        // Abort all tasks immediately (synchronous)
        s.abort_all();

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
    scope::<u32, _, _>(async |s| {
        s.spawn(async {
            sleep(Duration::from_millis(50)).await;
            10
        });
        s.spawn(async {
            sleep(Duration::from_millis(100)).await;
            20
        });

        s.abort_all();
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
