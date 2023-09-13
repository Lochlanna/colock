#![allow(dead_code)]

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use itertools::Itertools;

use benchmarks::sync_shared;
use benchmarks::sync_shared::PthreadMutex;
use benchmarks::Run;

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
            b.iter_custom(|iters| {
                sync_shared::run_throughput_benchmark::<colock::mutex::Mutex<f64>>(run, iters)
            })
        });
        group.bench_with_input(BenchmarkId::new("parking_lot", run), &run, |b, run| {
            b.iter_custom(|iters| {
                sync_shared::run_throughput_benchmark::<parking_lot::Mutex<f64>>(run, iters)
            })
        });
        group.bench_with_input(BenchmarkId::new("usync", run), &run, |b, run| {
            b.iter_custom(|iters| {
                sync_shared::run_throughput_benchmark::<usync::Mutex<f64>>(run, iters)
            })
        });
        group.bench_with_input(BenchmarkId::new("std", run), &run, |b, run| {
            b.iter_custom(|iters| {
                sync_shared::run_throughput_benchmark::<std::sync::Mutex<f64>>(run, iters)
            })
        });
        group.bench_with_input(BenchmarkId::new("mini", run), &run, |b, run| {
            b.iter_custom(|iters| {
                sync_shared::run_throughput_benchmark::<mini_lock::MiniLock<f64>>(run, iters)
            })
        });
        if cfg!(unix) {
            group.bench_with_input(BenchmarkId::new("pthread", run), &run, |b, run| {
                b.iter_custom(|iters| {
                    sync_shared::run_throughput_benchmark::<PthreadMutex<f64>>(run, iters)
                })
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
