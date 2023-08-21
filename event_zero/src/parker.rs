use std::fmt::{Debug, Formatter};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

enum ParkInner {
    ThreadParker(std::thread::Thread),
}

impl Default for ParkInner {
    fn default() -> Self {
        Self::ThreadParker(std::thread::current())
    }
}

impl Debug for ParkInner {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            ParkInner::ThreadParker(_) => f.write_str("thread"),
        }
    }
}

const NULL_STATE: u8 = 0;
const WAITING_STATE: u8 = 1;
const NOTIFYING_STATE: u8 = 2;

const NOTIFIED_STATE: u8 = 3;

#[derive(Debug, Default)]
pub struct Parker {
    inner: ParkInner,
    should_park: AtomicU8,
}

impl Parker {
    pub fn new() -> Self {
        let current_thread = std::thread::current();
        Self {
            inner: ParkInner::ThreadParker(current_thread),
            should_park: AtomicU8::new(NULL_STATE),
        }
    }

    pub fn prepare_park(&self) {
        debug_assert_ne!(self.should_park.load(Ordering::Relaxed), WAITING_STATE);
        self.should_park.store(WAITING_STATE, Ordering::Relaxed);
    }
    pub fn park(&self) {
        while self.should_park.load(Ordering::Acquire) == WAITING_STATE {
            std::thread::park();
        }
        while self.should_park.load(Ordering::Acquire) == NOTIFYING_STATE {
            core::hint::spin_loop()
        }
    }

    pub fn unpark_handle(&self) -> UnparkHandle {
        UnparkHandle { inner: self }
    }

    pub fn will_park(&self) -> bool {
        let should_park = self.should_park.load(Ordering::Acquire);
        should_park != NOTIFIED_STATE && should_park != NULL_STATE
    }
}

pub struct UnparkHandle {
    inner: *const Parker,
}

impl UnparkHandle {
    pub fn un_park(&self) -> bool {
        let parker = unsafe { &*self.inner };
        let old_state = parker.should_park.swap(NOTIFYING_STATE, Ordering::Release);
        let did_unpark = old_state == WAITING_STATE;
        if did_unpark {
            match &parker.inner {
                ParkInner::ThreadParker(thread) => thread.unpark(),
            }
        }
        parker.should_park.store(NOTIFIED_STATE, Ordering::Release);
        did_unpark
    }
}
