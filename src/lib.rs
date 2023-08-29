#![allow(dead_code)]
#![warn(missing_docs)]
// #![warn(missing_docs_in_private_items)]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]

mod event;
pub mod mutex;
mod raw_mutex;
mod raw_rw_lock;
mod spinwait;
