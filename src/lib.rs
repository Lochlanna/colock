#![allow(dead_code)]

mod event;
mod mutex;
mod raw_mutex;
mod raw_rw_lock;
mod spinwait;

pub use mutex::{Mutex, MutexGuard};
