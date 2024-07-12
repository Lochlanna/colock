#![allow(dead_code)]

use crate::Run;
use async_trait::async_trait;
use criterion::black_box;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[async_trait]
pub trait Mutex<T>: Sync {
    fn new(v: T) -> Self;
    async fn lock<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R + Send;
    async fn lock_timed<F, R>(&self, f: F) -> (std::time::Duration, R)
    where
        F: FnOnce(&mut T) -> R + Send;
}

#[async_trait]
impl<T> Mutex<T> for tokio::sync::Mutex<T>
where
    T: Send,
{
    fn new(v: T) -> Self {
        Self::new(v)
    }
    async fn lock<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R + Send,
    {
        f(&mut *self.lock().await)
    }

    async fn lock_timed<F, R>(&self, f: F) -> (std::time::Duration, R)
    where
        F: FnOnce(&mut T) -> R + Send,
    {
        let start = std::time::Instant::now();
        let mut guard = self.lock().await;
        let elapsed = start.elapsed();
        let res = f(&mut *guard);
        (elapsed, res)
    }
}

#[async_trait]
impl<T> Mutex<T> for maitake_sync::Mutex<T>
where
    T: Send,
{
    fn new(v: T) -> Self {
        Self::new(v)
    }
    async fn lock<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R + Send,
    {
        f(&mut *self.lock().await)
    }

    async fn lock_timed<F, R>(&self, f: F) -> (std::time::Duration, R)
    where
        F: FnOnce(&mut T) -> R + Send,
    {
        let start = std::time::Instant::now();
        let mut guard = self.lock().await;
        let elapsed = start.elapsed();
        let res = f(&mut *guard);
        (elapsed, res)
    }
}

#[async_trait]
impl<T> Mutex<T> for colock::mutex::Mutex<T>
where
    T: Send + Sync,
{
    fn new(v: T) -> Self {
        Self::new(v)
    }
    async fn lock<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R + Send,
    {
        f(&mut *self.lock_async().await)
    }

    async fn lock_timed<F, R>(&self, f: F) -> (std::time::Duration, R)
    where
        F: FnOnce(&mut T) -> R + Send,
    {
        let start = std::time::Instant::now();
        let mut guard = self.lock_async().await;
        let elapsed = start.elapsed();
        let res = f(&mut *guard);
        (elapsed, res)
    }
}

pub async fn run_throughput_benchmark<M: Mutex<f64> + Send + Sync + 'static>(
    run: Run,
    num_iters: u64,
) -> Duration {
    //pad the lock with 300 bytes on either side to avoid false sharing
    let lock = Arc::new(([0u8; 300], M::new(0.0), [0u8; 300]));
    let barrier = Arc::new(tokio::sync::Barrier::new(run.num_threads + 1));

    let mut max_end = Instant::now();

    let handles = (0..run.num_threads)
        .map(|_| {
            let lock = lock.clone();
            let barrier = barrier.clone();
            tokio::spawn(async move {
                barrier.wait().await;
                let start = Instant::now();
                let mut local_value = 0.0;
                let mut value = 0.0;
                for _ in 0..num_iters {
                    lock.1
                        .lock(|shared_value| {
                            for _ in 0..run.num_inside {
                                *shared_value += value;
                                *shared_value *= 1.01;
                                value = *shared_value;
                            }
                        })
                        .await;
                    for _ in 0..run.num_outside {
                        local_value += value;
                        local_value *= 1.01;
                        value = local_value;
                    }
                }
                black_box(value);
                (start, Instant::now())
            })
        })
        .collect::<Vec<_>>();

    barrier.wait().await;

    let mut min_start = Instant::now();
    for handle in handles {
        let (start, end) = handle.await.unwrap();
        min_start = min_start.min(start);
        max_end = max_end.max(end);
    }
    max_end - min_start
}
