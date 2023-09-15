//! A library for parking threads/tasks and waking them up.

#![allow(dead_code)]
#![warn(missing_docs)]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::module_name_repetitions)]
#![warn(clippy::undocumented_unsafe_blocks)]

mod thread_parker;

pub use thread_parker::thread_yield;
pub use thread_parker::{ThreadParker, ThreadParkerT};
