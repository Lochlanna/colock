//! A library for parking threads/tasks and waking them up.

#![allow(dead_code)]
#![warn(missing_docs)]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::module_name_repetitions)]
#![warn(clippy::undocumented_unsafe_blocks)]

mod thread_parker;

use std::cell::Cell;
use std::fmt::{Debug, Formatter};
use std::sync::atomic::{AtomicU8, Ordering};
use thread_parker::ThreadParkerT;

enum ParkInner {
    ThreadParker(thread_parker::ThreadParker),
    Waker(Cell<Option<core::task::Waker>>),
}

impl Debug for ParkInner {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ThreadParker(_) => f.write_str("thread"),
            Self::Waker(_) => f.write_str("waker"),
        }
    }
}

/// The state of the parker
#[repr(u8)]
#[derive(Debug, Eq, PartialEq, Copy, Clone)]
pub enum State {
    Waiting = 1,
    Notified = 2,
}

/// A parker is used to park a thread or task and wake it up later.
#[derive(Debug)]
pub struct Parker {
    inner: ParkInner,
    should_park: AtomicU8,
}

impl Parker {
    /// Creates a new parker.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            inner: ParkInner::ThreadParker(thread_parker::ThreadParker::const_new()),
            should_park: AtomicU8::new(State::Waiting as u8),
        }
    }

    /// Returns true if it's trivial to construct a parker.
    #[must_use]
    pub const fn is_cheap_to_construct() -> bool {
        thread_parker::ThreadParker::IS_CHEAP_TO_CONSTRUCT
    }

    /// Creates a new parker that is ready to accept a waker to wake an async task
    #[must_use]
    pub const fn new_async() -> Self {
        Self {
            inner: ParkInner::Waker(Cell::new(None)),
            should_park: AtomicU8::new(State::Waiting as u8),
        }
    }

    /// Resets the parker ready for parking. This must be called before every park. Including the first
    /// time park is called.
    pub fn prepare_park(&self) {
        self.should_park
            .store(State::Waiting as u8, Ordering::Relaxed);
        match &self.inner {
            ParkInner::ThreadParker(parker) => unsafe {
                parker.prepare_park();
            },
            ParkInner::Waker(_) => {}
        }
    }

    /// Park the thread until it's unparked. Should not be called for async tasks.
    ///
    /// # Panics
    /// This function will panic if it's called on a task Parker intended for async.
    pub fn park(&self) {
        match &self.inner {
            ParkInner::ThreadParker(thread) => unsafe { thread.park() },
            ParkInner::Waker(_) => panic!("park not supported for waker"),
        }
    }

    /// Park the thread until it's unparked or the timeout is reached. Should not be called for async tasks.
    ///
    /// # Panics
    /// This function will panic if it's called on a task Parker intended for async.
    pub fn park_until(&self, timeout: std::time::Instant) -> bool {
        match &self.inner {
            ParkInner::ThreadParker(thread) => unsafe { thread.park_until(timeout) },
            ParkInner::Waker(_) => panic!("park until not supported for waker"),
        }
    }

    /// Get a handle that can be used to unpark the thread or task.
    pub const fn unpark_handle(&self) -> UnparkHandle {
        UnparkHandle { inner: self }
    }

    /// Replace/Set the waker on a task parker.
    ///
    ///# Panics
    /// This function will panic if it's called on a thread parker.
    pub fn replace_waker(&self, waker: core::task::Waker) {
        match &self.inner {
            ParkInner::ThreadParker(_) => panic!("can't replace waker on a thread parker"),
            ParkInner::Waker(inner) => inner.replace(Some(waker)),
        };
    }

    /// Get the state of the parker
    ///
    /// # Panics
    /// Under normal circumstances this function should never panic. However, if the parker is in an
    /// invalid state this function will panic. Getting into an invalid state is a bug and should be
    /// reported.
    pub fn get_state(&self) -> State {
        let should_park = self.should_park.load(Ordering::Acquire);
        match should_park {
            1 => State::Waiting,
            2 => State::Notified,
            _ => panic!("unknown state for parker"),
        }
    }
}

/// A handle that can be used to unpark a thread or task.
pub struct UnparkHandle {
    inner: *const Parker,
}

impl UnparkHandle {
    /// Unpark the thread or task.
    ///
    /// # Safety
    /// This function is unsafe because it accesses the parker through a raw pointer. It's the
    /// callers responsibility to ensure that the parker is still valid and has not been freed or moved.
    ///
    /// # Panics
    /// This function will panic if it was expecting a waker to be set but there was none
    ///
    /// TODO should this panic if there is no waker? We could return false instead and let the caller handle it.
    pub unsafe fn un_park(&self) {
        let parker = &*self.inner;
        parker
            .should_park
            .store(State::Notified as u8, Ordering::Release);
        match &parker.inner {
            ParkInner::ThreadParker(thread) => thread.unpark(),
            ParkInner::Waker(waker) => waker.take().expect("there was no waker").wake(),
        }
    }
}
