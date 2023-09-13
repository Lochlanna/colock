#![allow(dead_code)]
// #![warn(missing_docs)]
// #![warn(missing_docs_in_private_items)]
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

#[repr(u8)]
#[derive(Debug, Eq, PartialEq, Copy, Clone)]
pub enum State {
    Waiting = 1,
    Notified = 2,
}

#[derive(Debug)]
pub struct Parker {
    inner: ParkInner,
    should_park: AtomicU8,
}

impl Parker {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            inner: ParkInner::ThreadParker(thread_parker::ThreadParker::const_new()),
            should_park: AtomicU8::new(State::Waiting as u8),
        }
    }

    #[must_use]
    pub const fn is_cheap_to_construct() -> bool {
        thread_parker::ThreadParker::IS_CHEAP_TO_CONSTRUCT
    }

    #[must_use]
    pub const fn new_async() -> Self {
        Self {
            inner: ParkInner::Waker(Cell::new(None)),
            should_park: AtomicU8::new(State::Waiting as u8),
        }
    }

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
    pub fn park(&self) {
        match &self.inner {
            ParkInner::ThreadParker(thread) => unsafe { thread.park() },
            ParkInner::Waker(_) => panic!("park not supported for waker"),
        }
    }

    pub fn park_until(&self, timeout: std::time::Instant) -> bool {
        match &self.inner {
            ParkInner::ThreadParker(thread) => unsafe { thread.park_until(timeout) },
            ParkInner::Waker(_) => panic!("park until not supported for waker"),
        }
    }

    pub const fn unpark_handle(&self) -> UnparkHandle {
        UnparkHandle { inner: self }
    }

    pub fn replace_waker(&self, waker: core::task::Waker) {
        match &self.inner {
            ParkInner::ThreadParker(_) => panic!("can't replace waker on a thread parker"),
            ParkInner::Waker(inner) => inner.replace(Some(waker)),
        };
    }

    pub fn get_state(&self) -> State {
        let should_park = self.should_park.load(Ordering::Acquire);
        match should_park {
            1 => State::Waiting,
            2 => State::Notified,
            _ => panic!("unknown state for parker"),
        }
    }
}

pub struct UnparkHandle {
    inner: *const Parker,
}

impl UnparkHandle {
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
