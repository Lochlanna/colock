#![allow(dead_code)]

use benchmarks::async_shared;
use benchmarks::Run;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use itertools::Itertools;

const MIN_THREADS: usize = 1;
const MAX_THREADS: usize = 3;

const MIN_INSIDE: usize = 1;
const MAX_INSIDE: usize = 2;
const INSIDE_STEP: usize = 2;

const MIN_OUTSIDE: usize = 1;
const MAX_OUTSIDE: usize = 2;
const OUTSIDE_STEP: usize = 2;

fn criterion_benchmark(c: &mut Criterion) {
    return;
    let tokio_runtime = tokio::runtime::Runtime::new().expect("couldn't spawn tokio runtime");

    let mut group = c.benchmark_group("async/throughput");
    let runs = (MIN_THREADS..=MAX_THREADS)
        .cartesian_product((MIN_INSIDE..=MAX_INSIDE).step_by(INSIDE_STEP))
        .cartesian_product((MIN_OUTSIDE..=MAX_OUTSIDE).step_by(OUTSIDE_STEP))
        .map(|((a, b), c)| Run::from((a, b, c)));
    for run in runs {
        group.bench_with_input(BenchmarkId::new("colock", run), &run, |b, run| {
            b.to_async(&tokio_runtime).iter_custom(|iters| {
                async_shared::run_throughput_benchmark::<colock::mutex::Mutex<f64>>(*run, iters)
            })
        });
        group.bench_with_input(BenchmarkId::new("tokio", run), &run, |b, run| {
            b.to_async(&tokio_runtime).iter_custom(|iters| {
                async_shared::run_throughput_benchmark::<tokio::sync::Mutex<f64>>(*run, iters)
            })
        });
        group.bench_with_input(BenchmarkId::new("maitake-sync", run), &run, |b, run| {
            b.to_async(&tokio_runtime).iter_custom(|iters| {
                async_shared::run_throughput_benchmark::<maitake_sync::Mutex<f64>>(*run, iters)
            })
        });
    }
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
