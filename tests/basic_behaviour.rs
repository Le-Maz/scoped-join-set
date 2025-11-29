use scoped_join_set::ScopedJoinSet;
use tokio::{
    sync::Notify,
    time::{sleep, Duration},
};

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

    set.shutdown().await;
}

#[tokio::test]
async fn join_next_returns_none_when_empty() {
    let mut set: ScopedJoinSet<i32> = ScopedJoinSet::new();

    assert!(set.join_next().await.is_none());

    set.shutdown().await;
}

#[tokio::test]
async fn len_and_spawn() {
    let mut set = ScopedJoinSet::<u32>::new();
    assert_eq!(set.len(), 0);

    set.spawn(async { 1 });
    set.spawn(async { 2 });
    assert_eq!(set.len(), 2);

    set.shutdown().await;
}

#[tokio::test]
async fn try_join_next_non_blocking() {
    let notify = Notify::new();
    let mut set = ScopedJoinSet::<u32>::new();

    // Spawn a long task
    set.spawn(async {
        sleep(Duration::from_millis(50)).await;
        notify.notify_waiters();
        42
    });

    // Immediately try join: should return None
    assert!(set.try_join_next().is_none());

    // Wait for it to complete
    notify.notified().await;
    sleep(Duration::from_millis(10)).await;

    // Now try_join_next should return the result
    let res = set.try_join_next();
    assert!(matches!(res, Some(Ok(42))));

    set.shutdown().await;
}
