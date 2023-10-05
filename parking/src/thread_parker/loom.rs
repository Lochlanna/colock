use loom::hint::spin_loop;
use loom::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Instant;
// Helper type for putting a thread to sleep until some other thread wakes it up
pub struct ThreadParker {
    parked: AtomicBool,
}

impl ThreadParker {
    dev_utils::loom_const_fn! {
        pub fn const_new() -> Self {
            ThreadParker {
                parked: AtomicBool::new(false),
            }
        }
    }
}

impl super::ThreadParkerT for ThreadParker {
    const IS_CHEAP_TO_CONSTRUCT: bool = true;

    #[inline]
    fn new() -> ThreadParker {
        ThreadParker::const_new()
    }

    #[inline]
    unsafe fn prepare_park(&self) {
        self.parked.store(true, Ordering::Relaxed);
    }

    #[inline]
    unsafe fn timed_out(&self) -> bool {
        self.parked.load(Ordering::Relaxed) != false
    }

    #[inline]
    unsafe fn park(&self) {
        while self.parked.load(Ordering::Acquire) != false {
            spin_loop();
            thread_yield();
        }
    }

    #[inline]
    unsafe fn park_until(&self, timeout: Instant) -> bool {
        while self.parked.load(Ordering::Acquire) != false {
            if Instant::now() >= timeout {
                return false;
            }
            spin_loop();
            thread_yield();
        }
        true
    }

    #[inline]
    unsafe fn unpark(&self) {
        // We don't need to lock anything, just clear the state
        self.parked.store(false, Ordering::Release);
    }
}

#[inline]
pub fn thread_yield() {
    thread::yield_now();
}
