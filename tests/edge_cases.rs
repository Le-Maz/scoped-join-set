use std::{
    future::{poll_fn, Future},
    task::Poll,
    thread::sleep as thread_sleep,
    time::Duration,
};

use scoped_join_set::scope;

#[tokio::test(flavor = "multi_thread")]
async fn long_poll() {
    let local_variable = "local variable".to_string();

    scope::<(), _, _>(async |s| {
        s.spawn(async {
            poll_fn(|_| {
                let reference = &local_variable;
                thread_sleep(Duration::from_millis(100));
                Poll::Ready(reference.clone())
            })
            .await;
        });
    })
    .await;

    drop(local_variable);
}

#[tokio::test(flavor = "current_thread")]
async fn drop_panic() {
    let local_variable = "local variable".to_string();

    struct SpecialDrop<'a> {
        reference: &'a str,
    }

    impl<'a> SpecialDrop<'a> {
        fn new(reference: &'a str) -> Self {
            Self { reference }
        }
    }

    impl<'a> Future for SpecialDrop<'a> {
        type Output = String;
        fn poll(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> Poll<Self::Output> {
            Poll::Pending
        }
    }

    impl<'a> Drop for SpecialDrop<'a> {
        fn drop(&mut self) {
            panic!("{}", self.reference)
        }
    }

    scope::<(), _, _>(async |s| {
        s.spawn(async {
            let f = SpecialDrop::new(&local_variable);
            tokio::time::sleep(Duration::from_millis(20)).await;
            f.await;
        });
    })
    .await;

    drop(local_variable);
}
