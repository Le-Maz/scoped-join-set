#![feature(test)]

extern crate test;

use scoped_join_set::scope;
use std::{
    hint::black_box,
    sync::atomic::{AtomicUsize, Ordering},
};
use test::Bencher;
use tokio::runtime::Runtime;

const TASKS: usize = 100;

#[bench]
fn spawn_regular_tasks(b: &mut Bencher) {
    let rt = Runtime::new().unwrap();
    b.iter(|| {
        rt.block_on(async {
            scope(async |s| {
                for i in 0..TASKS {
                    // s.spawn is now async
                    black_box(s.spawn(async move { i * 2 }).await);
                }

                let mut results = Vec::with_capacity(TASKS);
                while !s.is_empty().await {
                    if let Some(Ok(res)) = s.join_next().await {
                        results.push(res);
                    }
                }

                results
            })
            .await
        });
    });
}

#[bench]
fn use_shared_state(b: &mut Bencher) {
    let rt = Runtime::new().unwrap();
    b.iter(|| {
        rt.block_on(async {
            let counter = AtomicUsize::new(0);

            scope(async |s| {
                for _ in 0..TASKS {
                    // Capture reference to counter from stack
                    s.spawn(async {
                        black_box(counter.fetch_add(1, Ordering::Relaxed));
                    })
                    .await;
                }

                while !s.is_empty().await {
                    s.join_next().await;
                }
            })
            .await;

            counter.load(Ordering::Relaxed)
        });
    });
}
