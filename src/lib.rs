#![allow(dead_code)]

mod event;
mod mutex;
mod raw_mutex;
mod spinwait;
pub use mutex::{Mutex, MutexGuard};
