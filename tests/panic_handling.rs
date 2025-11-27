use scoped_join_set::{JoinError, ScopedJoinSet};

#[tokio::test]
async fn panicking_task() {
    let mut set = ScopedJoinSet::new();

    set.spawn(async {
        panic!("test panic");
    });

    let res = set.join_next().await;
    assert!(
        matches!(res, Some(Err(JoinError::Panicked(_)))),
        "Panicking task should return JoinError::Panicked"
    );
}
