//! A library for parking threads/tasks and waking them up.

#![allow(dead_code)]
#![warn(missing_docs)]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::module_name_repetitions)]
#![warn(clippy::undocumented_unsafe_blocks)]

mod thread_parker;

use std::fmt::{Debug, Formatter};
use std::time::Instant;
pub use thread_parker::thread_yield;
pub use thread_parker::{ThreadParker, ThreadParkerT};

pub enum Parker {
    Owned(ThreadParker),
    Ref(&'static ThreadParker),
}

impl Debug for Parker {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("ThreadParker")
    }
}

impl Parker {
    pub fn new_owned(thread_parker: ThreadParker) -> Self {
        Self::Owned(thread_parker)
    }

    pub fn new_ref(thread_parker: &'static ThreadParker) -> Self {
        Self::Ref(thread_parker)
    }

    pub fn prepare_park(&self) {
        match self {
            Self::Owned(thread_parker) => {
                unsafe {thread_parker.prepare_park()}
            }
            Self::Ref(thread_parker) => {
                unsafe {thread_parker.prepare_park()}
            }
        }
    }

    pub fn park(&self) {
        match self {
            Self::Owned(thread_parker) => {
                unsafe {thread_parker.park()}
            }
            Self::Ref(thread_parker) => {
                unsafe {thread_parker.park()}
            }
        }
    }

    pub fn park_until(&self, instant: Instant) -> bool {
        match self {
            Self::Owned(thread_parker) => {
                unsafe {thread_parker.park_until(instant)}
            }
            Self::Ref(thread_parker) => {
                unsafe {thread_parker.park_until(instant)}
            }
        }
    }

    pub fn unpark(&self) {
        match self {
            Self::Owned(thread_parker) => {
                unsafe {thread_parker.unpark()}
            }
            Self::Ref(thread_parker) => {
                unsafe {thread_parker.unpark()}
            }
        }
    }
    
    pub const fn waker(&self)-> ThreadWaker {
        ThreadWaker {
            parker: self,
        }
    }
}

#[derive(Debug)]
pub struct ThreadWaker {
    parker: *const Parker
}

impl ThreadWaker {
    pub fn wake(self) {
        unsafe {
            (*self.parker).unpark();
        }
    }

    pub fn wake_by_ref(&self) {
        unsafe {
            (*self.parker).unpark();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thread_parker::{ThreadParker, ThreadParkerT};
    use std::time::{Duration, Instant};

    #[test]
    fn park_timeout() {
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
    }

    #[test]
    fn park_wake() {
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
    }
}
