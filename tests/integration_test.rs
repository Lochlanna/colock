use core::fmt;
use criterion::black_box;
use std::fmt::Display;
use std::thread;
use std::time::{Duration, Instant};

trait Mutex<T> {
    fn new(v: T) -> Self;
    fn lock<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R;
}

impl<T> Mutex<T> for colock::mutex::Mutex<T> {
    fn new(v: T) -> Self {
        Self::new(v)
    }
    fn lock<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R,
    {
        f(&mut *self.lock())
    }
}

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

#[test]
#[ignore]
fn run_bench() {
    for i in 0..15 {
        run_benchmark::<colock::mutex::Mutex<f64>>(&Run::from((2, 1, 1)), 618222);
        println!("done {}", i);
        std::io::Write::flush(&mut std::io::stdout()).unwrap();
    }
}
