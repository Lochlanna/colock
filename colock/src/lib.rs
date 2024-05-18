#![allow(dead_code)]
// #![warn(missing_docs)]
// #![warn(missing_docs_in_private_items)]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::undocumented_unsafe_blocks)]

pub mod barrier;
pub mod condvar;
pub mod mutex;
pub mod raw_mutex;
mod raw_rw_lock;
pub mod rw_lock;
mod spinwait;
