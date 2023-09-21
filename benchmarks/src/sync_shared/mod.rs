#![allow(dead_code)]

use crate::Run;
use criterion::black_box;
use std::cell::UnsafeCell;
use std::ops::Deref;
use std::thread;
use std::time::{Duration, Instant};

pub trait Mutex<T> {
    fn new(v: T) -> Self;
    fn lock<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R;
    fn lock_timed<F, R>(&self, f: F) -> (std::time::Duration, R)
    where
        F: FnOnce(&mut T) -> R;
}

impl<T> Mutex<T> for mini_lock::MiniLock<T> {
    fn new(v: T) -> Self {
        Self::new(v)
    }

    fn lock<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R,
    {
        f(&mut *self.lock())
    }

    fn lock_timed<F, R>(&self, f: F) -> (Duration, R)
    where
        F: FnOnce(&mut T) -> R,
    {
        let start = std::time::Instant::now();
        let mut guard = self.lock();
        let elapsed = start.elapsed();
        let res = f(&mut *guard);
        (elapsed, res)
    }
}

impl<T> Mutex<T> for std::sync::Mutex<T> {
    fn new(v: T) -> Self {
        Self::new(v)
    }
    fn lock<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R,
    {
        f(&mut *self.lock().unwrap())
    }

    fn lock_timed<F, R>(&self, f: F) -> (std::time::Duration, R)
    where
        F: FnOnce(&mut T) -> R,
    {
        let start = std::time::Instant::now();
        let mut guard = self.lock().unwrap();
        let elapsed = start.elapsed();
        let res = f(&mut *guard);
        (elapsed, res)
    }
}

impl<T> Mutex<T> for parking_lot::Mutex<T> {
    fn new(v: T) -> Self {
        Self::new(v)
    }
    fn lock<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R,
    {
        f(&mut *self.lock())
    }

    fn lock_timed<F, R>(&self, f: F) -> (std::time::Duration, R)
    where
        F: FnOnce(&mut T) -> R,
    {
        let start = std::time::Instant::now();
        let mut guard = self.lock();
        let elapsed = start.elapsed();
        let res = f(&mut *guard);
        (elapsed, res)
    }
}

impl<T> Mutex<T> for colock::mutex::Mutex<T> {
    fn new(v: T) -> Self {
        Self::new(v)
    }
    fn lock<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R,
    {
        f(&mut *(self.deref().lock()))
    }

    fn lock_timed<F, R>(&self, f: F) -> (std::time::Duration, R)
    where
        F: FnOnce(&mut T) -> R,
    {
        let start = std::time::Instant::now();
        let mut guard = self.deref().lock();
        let elapsed = start.elapsed();
        let res = f(&mut *guard);
        (elapsed, res)
    }
}

impl<T> Mutex<T> for usync::Mutex<T> {
    fn new(v: T) -> Self {
        Self::new(v)
    }
    fn lock<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R,
    {
        f(&mut *self.lock())
    }

    fn lock_timed<F, R>(&self, f: F) -> (std::time::Duration, R)
    where
        F: FnOnce(&mut T) -> R,
    {
        let start = std::time::Instant::now();
        let mut guard = self.lock();
        let elapsed = start.elapsed();
        let res = f(&mut *guard);
        (elapsed, res)
    }
}

#[cfg(unix)]
pub struct PthreadMutex<T>(UnsafeCell<T>, UnsafeCell<libc::pthread_mutex_t>);
#[cfg(unix)]
unsafe impl<T> Sync for PthreadMutex<T> {}
#[cfg(unix)]
impl<T> Mutex<T> for PthreadMutex<T> {
    fn new(v: T) -> Self {
        PthreadMutex(
            UnsafeCell::new(v),
            UnsafeCell::new(libc::PTHREAD_MUTEX_INITIALIZER),
        )
    }
    fn lock<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R,
    {
        unsafe {
            libc::pthread_mutex_lock(self.1.get());
            let res = f(&mut *self.0.get());
            libc::pthread_mutex_unlock(self.1.get());
            res
        }
    }

    fn lock_timed<F, R>(&self, f: F) -> (std::time::Duration, R)
    where
        F: FnOnce(&mut T) -> R,
    {
        unsafe {
            let start = std::time::Instant::now();
            libc::pthread_mutex_lock(self.1.get());
            let elapsed = start.elapsed();
            let res = f(&mut *self.0.get());
            libc::pthread_mutex_unlock(self.1.get());
            (elapsed, res)
        }
    }
}
#[cfg(unix)]
impl<T> Drop for PthreadMutex<T> {
    fn drop(&mut self) {
        unsafe {
            libc::pthread_mutex_destroy(self.1.get());
        }
    }
}

pub fn run_latency_benchmark<M: Mutex<f64> + Send + Sync>(run: &Run, num_iters: u64) -> Duration {
    //pad the lock with 300 bytes on either side to avoid false sharing
    let lock = ([0u8; 300], M::new(0.0), [0u8; 300]);

    thread::scope(|s| {
        let mut handles = Vec::with_capacity(run.num_threads);
        for _ in 0..run.num_threads {
            let handle = s.spawn(|| {
                let mut local_value = 0.0;
                let mut value = 0.0;
                let mut worst_case_latency = Duration::default();
                for _ in 0..num_iters {
                    let (latency, _) = lock.1.lock_timed(|shared_value| {
                        for _ in 0..run.num_inside {
                            *shared_value += value;
                            *shared_value *= 1.01;
                            value = *shared_value;
                        }
                    });
                    if latency > worst_case_latency {
                        worst_case_latency = latency;
                    }
                    for _ in 0..run.num_outside {
                        local_value += value;
                        local_value *= 1.01;
                        value = local_value;
                    }
                }
                black_box(value);
                worst_case_latency
            });
            handles.push(handle);
        }
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .max()
            .unwrap()
    })
}

pub fn run_throughput_benchmark<M: Mutex<f64> + Send + Sync>(
    run: &Run,
    num_iters: u64,
) -> Duration {
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
