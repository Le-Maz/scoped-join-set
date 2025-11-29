#![feature(test)]

extern crate test;

use scoped_join_set::ScopedJoinSet;
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
            let mut set = ScopedJoinSet::<usize>::new();

            for i in 0..TASKS {
                black_box(set.spawn(async move { i * 2 }));
            }

            let mut results = Vec::with_capacity(TASKS);
            while let Some(res) = set.join_next().await {
                results.push(res.unwrap());
            }

            set.shutdown().await;

            results
        });
    });
}

#[bench]
fn use_shared_state(b: &mut Bencher) {
    let rt = Runtime::new().unwrap();
    b.iter(|| {
        rt.block_on(async {
            let counter = AtomicUsize::new(0);
            let mut set = ScopedJoinSet::new();

            for _ in 0..TASKS {
                let c_ref = &counter;
                set.spawn(async move {
                    black_box(c_ref.fetch_add(1, Ordering::Relaxed));
                });
            }

            while let Some(res) = set.join_next().await {
                res.unwrap();
            }

            set.shutdown().await;

            counter.load(Ordering::Relaxed)
        });
    });
}
