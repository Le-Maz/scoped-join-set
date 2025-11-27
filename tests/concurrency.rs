use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use scoped_join_set::ScopedJoinSet;
use tokio::time::sleep;

#[tokio::test]
async fn tasks_run_concurrently() {
    let started = AtomicUsize::new(0);
    let finished = AtomicUsize::new(0);

    let mut set = ScopedJoinSet::new();
    for _ in 0..3 {
        set.spawn(async {
            started.fetch_add(1, Ordering::SeqCst);
            sleep(Duration::from_millis(50)).await;
            finished.fetch_add(1, Ordering::SeqCst);
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
