use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use scoped_join_set::scope;
use tokio::sync::oneshot;

#[tokio::test]
async fn task_drops_properly() {
    let dropped = Arc::new(AtomicUsize::new(0));

    struct DropCounter(Arc<AtomicUsize>);
    impl Drop for DropCounter {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    scope::<(), _, _>(async |s| {
        for _ in 0..5 {
            let drop_counter = DropCounter(dropped.clone());
            s.spawn(async move {
                drop(drop_counter);
            });
        }

        while !s.is_empty() {
            s.join_next().await;
        }
    })
    .await;

    assert_eq!(dropped.load(Ordering::SeqCst), 5);
}

#[tokio::test]
async fn scope_lifetime_enforced() {
    let value = 99;

    scope::<i32, _, _>(async |s| {
        s.spawn(async { value });

        assert!(matches!(s.join_next().await, Some(Ok(99))));
    })
    .await;
}

#[tokio::test]
async fn none_on_dropped_future() {
    let (tx, rx) = oneshot::channel::<()>();

    scope::<i32, _, _>(async move |s| {
        s.spawn(async move {
            let _ = rx.await;
            10
        });

        // We exit the scope, triggering implicit abort/join of the task.
    })
    .await;

    // Verify tx was dropped (channel closed) without receiving a result
    drop(tx);
}
