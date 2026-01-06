use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use scoped_join_set::scope;
use tokio::time::sleep;

#[tokio::test]
async fn tasks_run_concurrently() {
    let started = AtomicUsize::new(0);
    let finished = AtomicUsize::new(0);

    scope::<u32, _, _>(async |s| {
        for _ in 0..3 {
            s.spawn(async {
                started.fetch_add(1, Ordering::SeqCst);
                sleep(Duration::from_millis(50)).await;
                finished.fetch_add(1, Ordering::SeqCst);
                1
            });
        }

        sleep(Duration::from_millis(10)).await;
        assert_eq!(started.load(Ordering::SeqCst), 3);

        while !s.is_empty() {
            s.join_next().await;
        }

        assert_eq!(finished.load(Ordering::SeqCst), 3);
    })
    .await;
}
