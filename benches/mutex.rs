#![allow(dead_code)]

mod shared;

use shared::*;

use core::fmt;
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use itertools::Itertools;

use std::fmt::Display;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Run {
    num_threads: usize,
    num_inside: usize,
    num_outside: usize,
}

impl From<(usize, usize, usize)> for Run {
    fn from((num_threads, num_inside, num_outside): (usize, usize, usize)) -> Self {
        Self {
            num_threads,
            num_inside,
            num_outside,
        }
    }
}

impl Display for Run {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} threads, {} inside, {} outside",
            self.num_threads, self.num_inside, self.num_outside
        )
    }
}

fn run_benchmark<M: Mutex<f64> + Send + Sync>(run: &Run, num_iters: u64) -> Duration {
    //pad the lock with 300 bytes on either side to avoid false sharing
    let lock = ([0u8; 300], M::new(0.0), [0u8; 300]);
    let barrier = std::sync::Barrier::new(run.num_threads + 1);

    let mut elapsed = Duration::default();
    thread::scope(|s| {
        for _ in 0..run.num_threads {
            s.spawn(|| {
                barrier.wait();
                let mut local_value = 0.0;
                let mut value = 0.0;
                for _ in 0..num_iters {
                    lock.1.lock(|shared_value| {
                        for _ in 0..run.num_inside {
                            *shared_value += value;
                            *shared_value *= 1.01;
                            value = *shared_value;
                        }
                    });
                    for _ in 0..run.num_outside {
                        local_value += value;
                        local_value *= 1.01;
                        value = local_value;
                    }
                }
                black_box(value);
                barrier.wait();
            });
        }
        barrier.wait();
        let start = Instant::now();
        barrier.wait();
        elapsed = start.elapsed() / run.num_threads as u32;
    });
    elapsed
}

const MIN_THREADS: usize = 1;
const MAX_THREADS: usize = 3;

const MIN_INSIDE: usize = 1;
const MAX_INSIDE: usize = 2;
const INSIDE_STEP: usize = 2;

const MIN_OUTSIDE: usize = 1;
const MAX_OUTSIDE: usize = 2;
const OUTSIDE_STEP: usize = 2;

fn criterion_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("throughput");
    let runs = (MIN_THREADS..=MAX_THREADS)
        .cartesian_product((MIN_INSIDE..=MAX_INSIDE).step_by(INSIDE_STEP))
        .cartesian_product((MIN_OUTSIDE..=MAX_OUTSIDE).step_by(OUTSIDE_STEP))
        .map(|((a, b), c)| Run::from((a, b, c)));
    for run in runs {
        group.bench_with_input(BenchmarkId::new("colock4", run), &run, |b, run| {
            b.iter_custom(|iters| run_benchmark::<colock::mutex::Mutex<f64>>(run, iters))
        });
        group.bench_with_input(BenchmarkId::new("parking_lot", run), &run, |b, run| {
            b.iter_custom(|iters| run_benchmark::<parking_lot::Mutex<f64>>(run, iters))
        });
        group.bench_with_input(BenchmarkId::new("usync", run), &run, |b, run| {
            b.iter_custom(|iters| run_benchmark::<usync::Mutex<f64>>(run, iters))
        });
        group.bench_with_input(BenchmarkId::new("std", run), &run, |b, run| {
            b.iter_custom(|iters| run_benchmark::<std::sync::Mutex<f64>>(run, iters))
        });
        if cfg!(unix) {
            group.bench_with_input(BenchmarkId::new("pthread", run), &run, |b, run| {
                b.iter_custom(|iters| run_benchmark::<PthreadMutex<f64>>(run, iters))
            });
        }
        // if cfg!(windows) {
        //     group.bench_with_input(BenchmarkId::new("SrwLock", run), &run, |b, run| {
        //         b.iter_custom(|iters| run_benchmark::<SrwLock<f64>>(run, iters))
        //     });
        // }
    }
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
