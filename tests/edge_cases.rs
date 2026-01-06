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

#[tokio::test]
async fn drop_uninit_memory_crash() {
    // A String requires valid internal pointers.
    // If we drop a String residing in uninitialized memory, it frees garbage.
    scope::<String, _, _>(async |s| {
        s.spawn(async {
            // Sleep to ensure we are aborted before writing the result
            tokio::time::sleep(Duration::from_millis(100)).await;
            "Valid String".to_string()
        });

        // Abort immediately, triggering WriteOutput::drop on the uninit slot
        s.abort_all();
    })
    .await;
}

/// Verifies that the library correctly handles Zero Sized Types (ZSTs).
///
/// ZSTs (like `()`) often have dangling pointers or optimization quirks.
/// We ensure that `Box::from_raw` logic handles unit types without segfaulting.
#[tokio::test]
async fn handle_zero_sized_types() {
    let output = scope::<(), _, _>(async |scope_handle| {
        scope_handle.spawn(async {
            // Do some "work"
            tokio::time::sleep(Duration::from_millis(5)).await;
            // Return unit
        });

        // Ensure we can join the ZST result
        let result = scope_handle.join_next().await;
        assert!(result.is_some());
        assert!(result.unwrap().is_ok());

        "Scope Completed"
    })
    .await;

    assert_eq!(output, "Scope Completed");
}
