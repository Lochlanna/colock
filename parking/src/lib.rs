//! A library for parking threads/tasks and waking them up.

#![allow(dead_code)]
#![warn(missing_docs)]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::module_name_repetitions)]
#![warn(clippy::undocumented_unsafe_blocks)]

mod thread_parker;

#[cfg(not(loom))]
mod loom;

pub use thread_parker::thread_yield;
pub use thread_parker::{ThreadParker, ThreadParkerT};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ThreadParker, ThreadParkerT};
    use std::time::{Duration, Instant};

    #[test]
    fn park_timeout() {
        loom::model(|| {
            let sleep_time = Duration::from_millis(5);
            let parker = ThreadParker::const_new();
            unsafe {
                parker.prepare_park();
            }
            let start = Instant::now();
            let was_woken = unsafe { parker.park_until(Instant::now() + sleep_time) };
            assert!(!was_woken);
            let elapsed = start.elapsed();
            assert!(elapsed >= sleep_time);
        });
    }

    #[test]
    fn park_wake() {
        loom::model(|| {
            const SLEEP_TIME: Duration = Duration::from_millis(50);
            let parker = ThreadParker::const_new();
            unsafe {
                parker.prepare_park();
            }
            let start = Instant::now();
            let was_woken = unsafe { parker.park_until(Instant::now() + SLEEP_TIME) };
            assert!(!was_woken);
            let elapsed = start.elapsed();
            assert!(elapsed >= SLEEP_TIME);
        });
    }
}
