use scoped_join_set::{scope, JoinError};

#[tokio::test]
async fn panicking_task() {
    scope::<(), _, _>(async |s| {
        s.spawn(async {
            panic!("test panic");
        });

        let res = s.join_next().await;
        assert!(
            matches!(res, Some(Err(JoinError::Panicked(_)))),
            "Panicking task should return JoinError::Panicked"
        );
    })
    .await;
}
