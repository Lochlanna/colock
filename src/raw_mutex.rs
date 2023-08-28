use crate::event::{Event, EventListener};
use crate::spinwait;
use core::cell::Cell;
use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::{AtomicU8, Ordering};
use core::task::{Context, Poll};
use std::time::Instant;

const LOCKED_BIT: u8 = 1;
const WAIT_BIT: u8 = 2;
const LOCKED_AND_WAITING: u8 = 3;
const FAIR_BIT: u8 = 4;

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

    const fn conditional_register(&self) -> impl FnOnce() -> bool + '_ {
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

    pub const fn lock_async<F, G>(&self, guard_builder: F) -> RawMutexPoller<'_, F>
    where
        F: Fn() -> G,
    {
        RawMutexPoller::new(self, guard_builder)
    }

    fn lock_slow(&self, timeout: Option<Instant>) -> bool {
        let mut listener = self.queue.new_listener();

        let mut state = self.state.load(Ordering::Relaxed);
        if state & LOCKED_BIT == 0
            && self
                .state
                .compare_exchange(
                    state,
                    state | LOCKED_BIT,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                )
                .is_ok()
        {
            return true;
        }
        if let Some(timeout) = timeout {
            if timeout <= Instant::now() {
                return false;
            }
        }
        while listener.register_if(self.conditional_register()) {
            if let Some(timeout) = timeout {
                if !listener.wait_until(timeout) {
                    // we timed out!
                    return false;
                }
            } else {
                listener.wait();
            }
            state = self.state.load(Ordering::Relaxed);
            if state & FAIR_BIT != 0 {
                self.state.fetch_and(!FAIR_BIT, Ordering::Relaxed);
                return true;
            }
            if self.try_lock_spin(&mut state) {
                return true;
            }
        }
        true
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

        self.lock_slow(None);
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
    #[inline]
    unsafe fn unlock_fair(&self) {
        if self
            .state
            .compare_exchange(LOCKED_BIT, 0, Ordering::Release, Ordering::Relaxed)
            .is_ok()
        {
            return;
        }
        self.queue.notify_if(
            |num_waiters_left| {
                if num_waiters_left == 1 {
                    self.state.store(FAIR_BIT | LOCKED_BIT, Ordering::Relaxed);
                } else {
                    self.state
                        .store(FAIR_BIT | WAIT_BIT | LOCKED_BIT, Ordering::Relaxed);
                }
                true
            },
            || {
                //unlock it as they've canceled the wait
                self.state.store(0, Ordering::Release);
            },
        );
    }
}

unsafe impl lock_api::RawMutexTimed for RawMutex {
    type Duration = core::time::Duration;
    type Instant = std::time::Instant;

    fn try_lock_for(&self, timeout: Self::Duration) -> bool {
        let timeout = std::time::Instant::now() + timeout;
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

        self.lock_slow(Some(timeout))
    }
}

pub struct RawMutexPoller<'a, F> {
    mutex: &'a RawMutex,
    listener: Option<EventListener<'a>>,
    did_take_lock: Cell<bool>,
    guard_builder: F,
}

impl<'a, F> RawMutexPoller<'a, F> {
    const fn new(mutex: &'a RawMutex, guard_builder: F) -> Self {
        Self {
            mutex,
            listener: None,
            did_take_lock: Cell::new(false),
            guard_builder,
        }
    }
}

impl<F> Drop for RawMutexPoller<'_, F> {
    fn drop(&mut self) {
        if let Some(listener) = self.listener.as_mut() {
            if !listener.cancel() && !self.did_take_lock.get() {
                // The waker was triggered but we never woke up to take the lock
                if self.mutex.state.load(Ordering::Relaxed) & LOCKED_BIT == 0 {
                    self.mutex
                        .queue
                        .notify_if(self.mutex.conditional_notify(), || {});
                }
            }
        }
    }
}

unsafe impl<F> Send for RawMutexPoller<'_, F> {}
impl<'a, F, G> Future for RawMutexPoller<'a, F>
where
    F: Fn() -> G,
{
    type Output = G;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let taken_lock = |this: &mut Self| {
            this.did_take_lock.set(true);
            if let Some(listener) = this.listener.as_mut() {
                listener.set_off_queue();
            }
            Poll::Ready((this.guard_builder)())
        };

        let this = unsafe { self.get_unchecked_mut() };
        let mut state = 0;
        while state & LOCKED_BIT == 0 {
            if let Err(new_state) = this.mutex.state.compare_exchange_weak(
                state,
                state | LOCKED_BIT,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                state = new_state;
            } else {
                return taken_lock(this);
            }
        }

        let listener = this
            .listener
            .get_or_insert_with(|| this.mutex.queue.new_async_listener(cx.waker().clone()));
        if !listener.register_if(this.mutex.conditional_register()) {
            //register will fail if it gets the lock!
            return taken_lock(this);
        }

        Poll::Pending
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

    #[test]
    fn timeout() {
        let mutex = RawMutex::new();
        mutex.lock();
        let start = Instant::now();
        mutex.try_lock_until(Instant::now() + Duration::from_millis(25));
        let elapsed = start.elapsed().as_millis();
        assert!(elapsed >= 25);
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn async_lock() {
        let mutex = RawMutex::new();
        mutex.lock_async(|| ()).await;
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
                    mutex.lock_async(|| ()).await;
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
                    mutex.lock_async(|| ()).await;
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
}
