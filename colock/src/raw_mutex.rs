use crate::spinwait;
use core::sync::atomic::{AtomicU8, Ordering};
use event::Event;
use lock_api::RawMutex as RawMutexAPI;
use std::time::Instant;

const LOCKED_BIT: u8 = 0b1;
const WAIT_BIT: u8 = 0b10;
const FAIR_BIT: u8 = 0b100;

#[derive(Debug)]
pub struct RawMutex {
    queue: Event,
    state: AtomicU8,
}

impl RawMutex {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            queue: Event::new(),
            state: AtomicU8::new(0),
        }
    }

    /// Returns a reference to the queue used by this mutex.
    /// This is for testing hence the pub(crate) visibility.
    pub(crate) const fn queue(&self) -> &Event {
        &self.queue
    }

    #[inline]
    fn try_lock_once(&self, state: &mut u8) -> bool {
        while *state & LOCKED_BIT == 0 {
            if let Err(new_state) = self.state.compare_exchange_weak(
                *state,
                *state | LOCKED_BIT,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                *state = new_state;
            } else {
                return true;
            }
        }
        false
    }

    #[inline]
    fn try_lock_spin(&self, state: &mut u8) -> bool {
        let mut spin_wait = spinwait::SpinWait::<4, 7>::new();
        while spin_wait.spin() {
            if *state & LOCKED_BIT == 0 {
                if let Err(new_state) = self.state.compare_exchange_weak(
                    *state,
                    *state | LOCKED_BIT,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    *state = new_state;
                } else {
                    return true;
                }
            } else {
                *state = self.state.load(Ordering::Relaxed);
            }
        }
        false
    }

    const fn should_sleep(&self) -> impl Fn() -> bool + '_ {
        || {
            let mut state = self.state.load(Ordering::Relaxed);
            loop {
                let (target, ordering) = match state {
                    0 => (LOCKED_BIT, Ordering::Acquire),
                    LOCKED_BIT => (LOCKED_BIT | WAIT_BIT, Ordering::Relaxed),
                    WAIT_BIT => (LOCKED_BIT | WAIT_BIT, Ordering::Acquire),
                    _ => {
                        // locked and waiting / fair unlock
                        return true;
                    }
                };
                match self
                    .state
                    .compare_exchange_weak(state, target, ordering, Ordering::Relaxed)
                {
                    Ok(_) => {
                        if state & LOCKED_BIT == 0 {
                            // we got the lock!
                            return false;
                        }
                        // we registered to wait!
                        return true;
                    }
                    Err(new_state) => state = new_state,
                }
            }
        }
    }

    const fn spin_on_wake(&self) -> impl Fn() -> bool + '_ {
        || {
            let mut state = self.state.load(Ordering::Relaxed);
            if state & FAIR_BIT == FAIR_BIT {
                // we are fair unlocking
                let old = self.state.fetch_and(!FAIR_BIT, Ordering::Relaxed);
                debug_assert_eq!(old, LOCKED_BIT | FAIR_BIT);
                return false;
            }
            !self.try_lock_spin(&mut state)
        }
    }

    const fn try_on_wake(&self) -> impl Fn() -> bool + '_ {
        || {
            let mut state = self.state.load(Ordering::Relaxed);
            if state & FAIR_BIT == FAIR_BIT {
                // we are fair unlocking
                let old = self.state.fetch_and(!FAIR_BIT, Ordering::Relaxed);
                debug_assert_eq!(old, LOCKED_BIT | FAIR_BIT);
                return false;
            }
            !self.try_lock_once(&mut state)
        }
    }

    const fn conditional_notify(&self) -> impl Fn(usize) -> bool + '_ {
        |num_waiters_left| {
            let state = self.state.load(Ordering::Relaxed);
            if state & LOCKED_BIT != 0 {
                //the lock has already been locked by someone else don't bother waking a thread
                return false;
            }
            if num_waiters_left == 1 {
                self.state.fetch_and(!WAIT_BIT, Ordering::Relaxed);
            }
            true
        }
    }

    const fn fair_conditional_notify(&self) -> impl Fn(usize) -> bool + '_ {
        |num_waiters_left| {
            if num_waiters_left == 1 {
                self.state.store(LOCKED_BIT | FAIR_BIT, Ordering::Relaxed);
            } else {
                self.state
                    .store(WAIT_BIT | LOCKED_BIT | FAIR_BIT, Ordering::Relaxed);
            }
            true
        }
    }

    pub async fn lock_async(&self) {
        let Err(mut state) =
            self.state
                .compare_exchange_weak(0, LOCKED_BIT, Ordering::Acquire, Ordering::Relaxed)
        else {
            return;
        };
        // try lock once is slightly smarter in that it will set current correctly and spin only while the lock is unlocked!
        if self.try_lock_once(&mut state) {
            return;
        }
        // use try on wake rather than spin try on wake
        self.queue
            .wait_while_async(self.should_sleep(), self.try_on_wake())
            .await;
    }
}

unsafe impl lock_api::RawMutex for RawMutex {
    #[allow(clippy::declare_interior_mutable_const)]
    const INIT: Self = Self::new();
    type GuardMarker = lock_api::GuardSend;

    #[inline]
    fn lock(&self) {
        let Err(mut state) =
            self.state
                .compare_exchange_weak(0, LOCKED_BIT, Ordering::Acquire, Ordering::Relaxed)
        else {
            return;
        };
        if self.try_lock_spin(&mut state) {
            return;
        }
        self.queue
            .wait_while(self.should_sleep(), self.spin_on_wake());
    }

    #[inline]
    fn try_lock(&self) -> bool {
        let Err(mut state) =
            self.state
                .compare_exchange_weak(0, LOCKED_BIT, Ordering::Acquire, Ordering::Relaxed)
        else {
            return true;
        };
        self.try_lock_once(&mut state)
    }

    #[inline]
    unsafe fn unlock(&self) {
        let state = self.state.fetch_and(!LOCKED_BIT, Ordering::Release);
        if state & WAIT_BIT == 0 {
            return;
        }

        self.queue.notify_if(self.conditional_notify(), || {});
    }

    fn is_locked(&self) -> bool {
        self.state.load(Ordering::Relaxed) & LOCKED_BIT != 0
    }
}

unsafe impl lock_api::RawMutexFair for RawMutex {
    unsafe fn unlock_fair(&self) {
        self.queue.notify_if(self.fair_conditional_notify(), || {
            //unlock from within the queue lock
            self.state.store(0, Ordering::Release);
        });
    }

    unsafe fn bump(&self) {
        if self.state.load(Ordering::Acquire) & WAIT_BIT == 0 {
            return;
        }
        if self.queue.notify_if(self.fair_conditional_notify(), || {}) {
            self.lock();
        }
    }
}

unsafe impl lock_api::RawMutexTimed for RawMutex {
    type Duration = std::time::Duration;
    type Instant = Instant;

    fn try_lock_for(&self, timeout: Self::Duration) -> bool {
        let timeout = Instant::now()
            .checked_add(timeout)
            .expect("overflow determining timeout");
        self.try_lock_until(timeout)
    }

    fn try_lock_until(&self, timeout: Self::Instant) -> bool {
        let Err(mut state) =
            self.state
                .compare_exchange_weak(0, LOCKED_BIT, Ordering::Acquire, Ordering::Relaxed)
        else {
            return true;
        };
        if self.try_lock_spin(&mut state) {
            return true;
        }
        self.queue
            .wait_while_until(self.should_sleep(), self.spin_on_wake(), timeout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lock_api::{RawMutex as RawMutexApi, RawMutexTimed};
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn it_works_threaded() {
        let mutex = RawMutex::new();
        let barrier = std::sync::Barrier::new(2);
        let num_iterations = 10;
        thread::scope(|s| {
            s.spawn(|| {
                for _ in 0..num_iterations {
                    mutex.lock();
                    barrier.wait();
                    while mutex.queue.num_waiting() == 0 {
                        thread::yield_now();
                    }
                    thread::sleep(Duration::from_millis(5));
                    unsafe {
                        mutex.unlock();
                    }
                    barrier.wait();
                }
            });
            for _ in 0..num_iterations {
                barrier.wait();
                assert!(mutex.is_locked());
                mutex.lock();
                unsafe {
                    mutex.unlock();
                }
                barrier.wait();
            }
        });
        assert!(!mutex.is_locked());
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn async_lock() {
        let mutex = RawMutex::new();
        mutex.lock_async().await;
        assert!(mutex.is_locked());
        unsafe {
            mutex.unlock();
        }
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn it_works_threaded_async() {
        let mutex = RawMutex::new();
        let barrier = tokio::sync::Barrier::new(2);
        let num_iterations = 10;
        tokio_scoped::scope(|s| {
            s.spawn(async {
                for _ in 0..num_iterations {
                    mutex.lock_async().await;
                    barrier.wait().await;
                    while mutex.queue.num_waiting() == 0 {
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    unsafe {
                        mutex.unlock();
                    }
                    barrier.wait().await;
                }
            });
            s.spawn(async {
                for _ in 0..num_iterations {
                    barrier.wait().await;
                    assert!(mutex.is_locked());
                    mutex.lock_async().await;
                    unsafe {
                        mutex.unlock();
                    }
                    barrier.wait().await;
                }
            });
        });
        assert!(!mutex.is_locked());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn lock_timeout() {
        let mutex = RawMutex::new();
        mutex.lock();
        let start = Instant::now();
        let did_lock = mutex.try_lock_for(Duration::from_millis(150));
        assert!(!did_lock);
        assert!(start.elapsed().as_millis() >= 150);
        unsafe {
            mutex.unlock();
        }

        let start = Instant::now();
        let did_lock = mutex.try_lock_for(Duration::from_millis(150));
        assert!(did_lock);
        assert!(start.elapsed().as_millis() < 10);
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn async_lock_timeout() {
        let mutex = RawMutex::new();
        mutex.lock_async().await;
        let start = Instant::now();
        let did_lock;
        tokio::select! {
            _ = mutex.lock_async() => {
                did_lock = true;
            }
            _ = tokio::time::sleep(Duration::from_millis(150)) => {
                did_lock = false;
            }
        }
        assert!(!did_lock);
        assert!(start.elapsed().as_millis() >= 150);
        unsafe {
            mutex.unlock();
        }
    }
}