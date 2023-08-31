use crate::event::Event;
use crate::spinwait;
use core::sync::atomic::{AtomicU8, Ordering};
use lock_api::RawMutex as RawMutexApi;
use std::time::Instant;

const LOCKED_BIT: u8 = 1;
const WAIT_BIT: u8 = 2;
const LOCKED_AND_WAITING: u8 = 3;

#[derive(Debug)]
pub struct RawMutex {
    queue: Event,
    state: AtomicU8,
}

impl RawMutex {
    pub const fn new() -> Self {
        Self {
            queue: Event::new(),
            state: AtomicU8::new(0),
        }
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
        let mut spin_wait = spinwait::SpinWait::new();
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

    const fn should_register(&self) -> impl Fn() -> bool + '_ {
        || !self.try_lock()
    }

    // Set the wait bit
    const fn should_sleep(&self) -> impl Fn() -> bool + '_ {
        || {
            let mut state = LOCKED_BIT;
            let mut target = LOCKED_BIT | WAIT_BIT;
            while let Err(new_state) = self.state.compare_exchange_weak(
                state,
                target,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                state = new_state;
                match state {
                    0 => target = LOCKED_BIT,
                    LOCKED_AND_WAITING => return true,
                    _ => target = LOCKED_AND_WAITING,
                }
            }
            state & LOCKED_BIT != 0
        }
    }

    const fn spin_on_wake(&self) -> impl Fn() -> bool + '_ {
        || {
            let mut state = self.state.load(Ordering::Relaxed);
            !self.try_lock_spin(&mut state)
        }
    }

    const fn try_on_wake(&self) -> impl Fn() -> bool + '_ {
        || {
            let mut state = self.state.load(Ordering::Relaxed);
            !self.try_lock_once(&mut state)
        }
    }

    const fn conditional_notify(&self) -> impl Fn(usize) -> bool + '_ {
        |num_waiters_left| {
            let state = self.state.load(Ordering::Relaxed);
            if num_waiters_left == 1 && state & LOCKED_BIT == 0 {
                return true;
            }
            let state = self.state.fetch_or(WAIT_BIT, Ordering::SeqCst);
            state & LOCKED_BIT == 0
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
            .wait_while_async(
                self.should_register(),
                self.should_sleep(),
                self.try_on_wake(),
            )
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
        self.queue.wait_while(
            self.should_register(),
            self.should_sleep(),
            self.spin_on_wake(),
        );
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
        let state = self.state.swap(0, Ordering::Release);
        if state & WAIT_BIT == 0 {
            return;
        }

        self.queue.notify_if(self.conditional_notify(), || {});
    }

    fn is_locked(&self) -> bool {
        self.state.load(Ordering::Relaxed) & LOCKED_BIT != 0
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
        self.queue.wait_while_until(
            self.should_register(),
            self.should_sleep(),
            self.spin_on_wake(),
            timeout,
        )
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
                    thread::sleep(Duration::from_millis(50));
                    unsafe {
                        mutex.unlock();
                    }
                    barrier.wait();
                }
            });
            for _ in 0..num_iterations {
                barrier.wait();
                assert!(mutex.is_locked());
                let start = Instant::now();
                mutex.lock();
                let elapsed = start.elapsed().as_millis();
                assert!(elapsed >= 40);
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
                    tokio::time::sleep(Duration::from_millis(50)).await;
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
                    let start = Instant::now();
                    mutex.lock_async().await;
                    let elapsed = start.elapsed().as_millis();
                    assert!(elapsed >= 40);
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
