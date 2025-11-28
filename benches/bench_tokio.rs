#![feature(test)]

extern crate test;

use std::{
    hint::black_box,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
use test::Bencher;
use tokio::runtime::Runtime;
use tokio::task::JoinSet;

const TASKS: usize = 100;

#[bench]
fn spawn_regular_tasks(b: &mut Bencher) {
    let rt = Runtime::new().unwrap();
    b.iter(|| {
        rt.block_on(async {
            let mut set = JoinSet::new();

            for i in 0..TASKS {
                black_box(set.spawn(Box::pin(async move { i * 2 })));
            }

            let mut results = Vec::with_capacity(TASKS);
            while let Some(res) = set.join_next().await {
                results.push(res.unwrap());
            }

            results
        });
    });
}

#[bench]
fn use_shared_state(b: &mut Bencher) {
    let rt = Runtime::new().unwrap();
    b.iter(|| {
        rt.block_on(async {
            let mut set = JoinSet::new();
            let counter = Arc::new(AtomicUsize::new(0));

            for _ in 0..TASKS {
                let c = Arc::clone(&counter);
                set.spawn(async move {
                    black_box(c.fetch_add(1, Ordering::Relaxed));
                });
            }

            while let Some(res) = set.join_next().await {
                res.unwrap();
            }

            counter.load(Ordering::Relaxed)
        });
    });
}
