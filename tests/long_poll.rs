use std::{future::poll_fn, thread::sleep, time::Duration};

use scoped_join_set::ScopedJoinSet;

#[tokio::test(flavor = "multi_thread")]
async fn long_poll() {
    let local_variable = "local variable".to_string();
    let mut scoped_join_set = ScopedJoinSet::new();
    scoped_join_set.spawn(async {
        poll_fn(|_| {
            let reference = &local_variable;
            sleep(Duration::from_millis(100));
            std::task::Poll::Ready(reference.clone())
        })
        .await;
    });
    drop(scoped_join_set);
    drop(local_variable);
}
