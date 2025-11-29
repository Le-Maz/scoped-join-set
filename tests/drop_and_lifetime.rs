use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use scoped_join_set::ScopedJoinSet;
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

    let mut set = ScopedJoinSet::new();
    for _ in 0..5 {
        let drop_counter = DropCounter(dropped.clone());
        set.spawn(async move {
            drop(drop_counter);
            42
        });
    }

    while !set.is_empty() {
        set.join_next().await;
    }

    assert_eq!(dropped.load(Ordering::SeqCst), 5);

    set.shutdown().await;
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

    set.shutdown().await;
}

#[tokio::test]
async fn none_on_dropped_future() {
    let mut set = ScopedJoinSet::new();
    let (tx, rx) = oneshot::channel::<()>();

    set.spawn(async move {
        let _ = rx.await;
        10
    });

    set.shutdown().await;
    drop(tx);
}
