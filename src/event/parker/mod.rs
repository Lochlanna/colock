mod thread_parker;

use crate::event::parker::thread_parker::ThreadParkerT;
use std::cell::Cell;
use std::fmt::{Debug, Formatter};
use std::sync::atomic::{AtomicU8, Ordering};

enum ParkInner {
    ThreadParker(thread_parker::ThreadParker),
    Waker(Cell<Option<core::task::Waker>>),
}

impl Debug for ParkInner {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            ParkInner::ThreadParker(_) => f.write_str("thread"),
            ParkInner::Waker(_) => f.write_str("waker"),
        }
    }
}

#[repr(u8)]
#[derive(Debug, Eq, PartialEq, Copy, Clone)]
pub enum State {
    Waiting = 1,
    Notifying = 2,
    Notified = 3,
}

#[derive(Debug)]
pub struct Parker {
    inner: ParkInner,
    should_park: AtomicU8,
}

impl Parker {
    pub const fn new() -> Self {
        Self {
            inner: ParkInner::ThreadParker(thread_parker::ThreadParker::const_new()),
            should_park: AtomicU8::new(State::Waiting as u8),
        }
    }

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
            ParkInner::ThreadParker(thread) => {
                while self.should_park.load(Ordering::Acquire) == State::Waiting as u8 {
                    unsafe { thread.park() }
                }
            }
            ParkInner::Waker(_) => {}
        }

        while self.should_park.load(Ordering::Acquire) == State::Notifying as u8 {
            core::hint::spin_loop()
        }
    }

    pub fn park_until(&self, timeout: std::time::Instant) -> bool {
        match &self.inner {
            ParkInner::ThreadParker(thread) => {
                while self.should_park.load(Ordering::Acquire) == State::Waiting as u8 {
                    if !unsafe { thread.park_until(timeout) } {
                        return false;
                    }
                }
            }
            ParkInner::Waker(_) => {}
        }

        while self.should_park.load(Ordering::Acquire) == State::Notifying as u8 {
            core::hint::spin_loop()
        }
        true
    }

    pub fn unpark_handle(&self) -> UnparkHandle {
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
            2 => State::Notifying,
            3 => State::Notified,
            _ => panic!("unknown state for parker"),
        }
    }
}

pub struct UnparkHandle {
    inner: *const Parker,
}

impl UnparkHandle {
    pub fn un_park(&self) -> bool {
        let parker = unsafe { &*self.inner };
        let old_state = parker
            .should_park
            .swap(State::Notifying as u8, Ordering::Release);
        let did_unpark = old_state == State::Waiting as u8;
        if did_unpark {
            match &parker.inner {
                ParkInner::ThreadParker(thread) => unsafe { thread.unpark() },
                ParkInner::Waker(waker) => waker.take().expect("there was no waker").wake(),
            }
        }
        parker
            .should_park
            .store(State::Notified as u8, Ordering::Release);
        did_unpark
    }
}
