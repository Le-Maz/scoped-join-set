use scoped_join_set::scope;
use tokio::{
    sync::Notify,
    time::{sleep, Duration},
};

#[tokio::test]
async fn basic_completion() {
    scope(async |s| {
        s.spawn(async { 1 });
        s.spawn(async { 2 });

        let mut results = vec![];
        while !s.is_empty() {
            if let Some(Ok(val)) = s.join_next().await {
                results.push(val);
            }
        }

        results.sort();
        assert_eq!(results, vec![1, 2]);
    })
    .await;
}

#[tokio::test]
async fn join_next_returns_none_when_empty() {
    scope::<i32, _, _>(async |s| {
        assert!(s.join_next().await.is_none());
    })
    .await;
}

#[tokio::test]
async fn len_and_spawn() {
    scope::<u32, _, _>(async |s| {
        assert_eq!(s.len(), 0);

        s.spawn(async { 1 });
        s.spawn(async { 2 });
        assert_eq!(s.len(), 2);
    })
    .await;
}

#[tokio::test]
async fn try_join_next_non_blocking() {
    let notify = Notify::new();

    scope::<u32, _, _>(async |s| {
        // Spawn a long task
        s.spawn(async {
            sleep(Duration::from_millis(50)).await;
            notify.notify_waiters();
            42
        });

        // Immediately try join: should return None (task not ready)
        assert!(s.try_join_next().is_none());

        // Wait for it to complete
        notify.notified().await;
        sleep(Duration::from_millis(10)).await;

        // Now try_join_next should return the result
        let res = s.try_join_next();
        assert!(matches!(res, Some(Ok(42))));
    })
    .await;
}
