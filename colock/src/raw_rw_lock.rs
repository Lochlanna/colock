use event::Event;
use lock_api::RawRwLock as RawRWLockApi;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

const SHARED_LOCK: usize = 0b1;
const EXCLUSIVE_LOCK: usize = 0b10;
const SHARED_WAITING: usize = 0b100;
const EXCLUSIVE_WAITING: usize = 0b1000;
const ONE_READER: usize = 0b10000;
const READERS_MASK: usize = !0b1111;

const fn num_readers(state: usize) -> usize {
    state >> 4
}

#[derive(Debug, Default)]
pub struct RawRwLock {
    state: AtomicUsize,
    read_queue: Event,
    write_queue: Event,
}

impl RawRwLock {
    pub const fn new() -> Self {
        Self {
            state: AtomicUsize::new(0),
            read_queue: Event::new(),
            write_queue: Event::new(),
        }
    }

    fn lock_shared_once(&self, state: &mut usize) -> bool {
        while *state & (EXCLUSIVE_LOCK | EXCLUSIVE_WAITING) == 0 {
            let target = (*state + ONE_READER) | SHARED_LOCK;
            if let Err(new_state) = self.state.compare_exchange_weak(
                *state,
                target,
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

    fn lock_exclusive_once(&self, state: &mut usize) -> bool {
        while *state & (EXCLUSIVE_LOCK | SHARED_LOCK) == 0 {
            let target = *state | EXCLUSIVE_LOCK;
            if let Err(new_state) = self.state.compare_exchange_weak(
                *state,
                target,
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

    const fn conditional_register_shared(&self) -> impl Fn(usize) -> bool + '_ {
        |_| {
            let mut state = self.state.load(Ordering::Relaxed);
            loop {
                let (target, ordering) = if state == 0 {
                    ((SHARED_LOCK + ONE_READER), Ordering::Acquire)
                } else if state & (EXCLUSIVE_LOCK | EXCLUSIVE_WAITING) == 0 {
                    debug_assert!(state & SHARED_LOCK != 0);
                    // it is already shared locked so we just add to it
                    (state + ONE_READER, Ordering::Relaxed)
                } else {
                    // it is exclusively locked or waiting to be exclusively locked
                    ((state + ONE_READER) | SHARED_WAITING, Ordering::Relaxed)
                };
                match self
                    .state
                    .compare_exchange_weak(state, target, ordering, Ordering::Relaxed)
                {
                    Ok(_) => {
                        if state & (EXCLUSIVE_LOCK | EXCLUSIVE_WAITING) == 0 {
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

    const fn conditional_register_exclusive(&self) -> impl Fn(usize) -> bool + '_ {
        |_| {
            let mut state = self.state.load(Ordering::Relaxed);
            loop {
                let (target, ordering) = if state & (SHARED_LOCK | EXCLUSIVE_LOCK) == 0 {
                    (EXCLUSIVE_LOCK, Ordering::Acquire)
                } else if state & EXCLUSIVE_WAITING != 0 {
                    return true;
                } else {
                    (state | EXCLUSIVE_WAITING, Ordering::Relaxed)
                };
                match self
                    .state
                    .compare_exchange_weak(state, target, ordering, Ordering::Relaxed)
                {
                    Ok(_) => {
                        if state & (SHARED_LOCK | EXCLUSIVE_LOCK) == 0 {
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

    const fn should_wake_shared(&self) -> impl Fn() -> bool + '_ {
        || {
            let mut state = self.state.load(Ordering::Relaxed);
            while state & (EXCLUSIVE_LOCK | EXCLUSIVE_WAITING) == 0 {
                if let Err(new_state) = self.state.compare_exchange_weak(
                    state,
                    state | SHARED_LOCK,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    state = new_state;
                } else {
                    return true;
                }
            }
            false
        }
    }

    const fn should_wake_exclusive(&self) -> impl Fn() -> bool + '_ {
        || {
            let mut state = self.state.load(Ordering::Relaxed);
            self.lock_exclusive_once(&mut state)
        }
    }

    const fn conditional_notify_exclusive(&self) -> impl Fn(usize) -> bool + '_ {
        |num_waiters_left| {
            let state = self.state.load(Ordering::Relaxed);
            if state & (EXCLUSIVE_LOCK | SHARED_LOCK) != 0 {
                //the lock has already been locked by someone else don't bother waking a thread
                return false;
            }
            if num_waiters_left == 1 {
                self.state.fetch_and(!EXCLUSIVE_WAITING, Ordering::Relaxed);
            }
            true
        }
    }

    const fn conditional_notify_shared(&self) -> impl Fn(usize) -> bool + '_ {
        |num_waiters_left| {
            let state = self.state.load(Ordering::Relaxed);
            if state & EXCLUSIVE_LOCK != 0 {
                //the lock has already been locked by someone else don't bother waking a thread
                return false;
            }
            if num_waiters_left == 1 {
                self.state.fetch_and(!SHARED_WAITING, Ordering::Relaxed);
            }
            true
        }
    }

    fn handle_wake(&self, state: usize) {
        if state & EXCLUSIVE_WAITING != 0 {
            // there is a writer waiting so we will wake them
            self.write_queue
                .notify_if(self.conditional_notify_exclusive(), || {
                    self.state.fetch_and(!EXCLUSIVE_WAITING, Ordering::Relaxed);
                });
        } else if state & SHARED_WAITING != 0 {
            // there are readers waiting so we will wake them
            self.read_queue
                .notify_all_while(self.conditional_notify_shared(), || {
                    self.state.fetch_and(!SHARED_WAITING, Ordering::Relaxed);
                });
        }
    }

    pub async fn lock_shared_async(&self) {
        let Err(mut state) = self.state.compare_exchange_weak(
            0,
            SHARED_LOCK | ONE_READER,
            Ordering::Acquire,
            Ordering::Relaxed,
        ) else {
            return;
        };
        // if it's not write locked and there are no writers waiting we are allowed to grab the shared lock
        if self.lock_shared_once(&mut state) {
            return;
        }
        self.read_queue
            .wait_while_async(
                self.conditional_register_shared(),
                self.should_wake_shared(),
            )
            .await;
    }

    pub async fn lock_exclusive_async(&self) {
        let Err(mut state) = self.state.compare_exchange_weak(
            0,
            EXCLUSIVE_LOCK,
            Ordering::Acquire,
            Ordering::Relaxed,
        ) else {
            return;
        };
        // if it's not write locked and there are no writers waiting we are allowed to grab the shared lock
        if self.lock_exclusive_once(&mut state) {
            return;
        }
        // we failed to grab the lock, so we need to wait
        self.write_queue
            .wait_while_async(
                self.conditional_register_exclusive(),
                self.should_wake_exclusive(),
            )
            .await;
    }
}

unsafe impl lock_api::RawRwLock for RawRwLock {
    #[allow(clippy::declare_interior_mutable_const)]
    const INIT: Self = Self::new();
    type GuardMarker = ();

    fn lock_shared(&self) {
        let Err(mut state) = self.state.compare_exchange_weak(
            0,
            SHARED_LOCK | ONE_READER,
            Ordering::Acquire,
            Ordering::Relaxed,
        ) else {
            return;
        };
        // if it's not write locked and there are no writers waiting we are allowed to grab the shared lock
        if self.lock_shared_once(&mut state) {
            return;
        }
        // we failed to grab the lock, so we need to wait
        self.read_queue.wait_while(
            self.conditional_register_shared(),
            self.should_wake_shared(),
        );
    }

    fn try_lock_shared(&self) -> bool {
        let mut state = self.state.load(Ordering::Relaxed);
        // we can lock as long as it's not exclusively locked or waiting to be exclusively locked
        self.lock_shared_once(&mut state)
    }

    unsafe fn unlock_shared(&self) {
        //optimistically attempt to unlock the lock
        let Err(mut state) = self.state.compare_exchange_weak(
            SHARED_LOCK | ONE_READER,
            0,
            Ordering::Release,
            Ordering::Relaxed,
        ) else {
            return;
        };
        debug_assert!(state & SHARED_LOCK != 0);
        debug_assert!(state & EXCLUSIVE_LOCK == 0);
        loop {
            debug_assert!(num_readers(state) > 0);
            let (target, ordering) = if num_readers(state) == 1 {
                (state & !READERS_MASK & !SHARED_LOCK, Ordering::Release)
            } else {
                (state - ONE_READER, Ordering::Relaxed)
            };
            if let Err(new_state) =
                self.state
                    .compare_exchange_weak(state, target, ordering, Ordering::Relaxed)
            {
                state = new_state;
                continue;
            }
            if num_readers(state) == 1 {
                // we were the last one (and unlocked the lock) so we should try to wake up waiting threads
                self.handle_wake(target);
            }
            return;
        }
    }

    fn lock_exclusive(&self) {
        let Err(mut state) = self.state.compare_exchange_weak(
            0,
            EXCLUSIVE_LOCK,
            Ordering::Acquire,
            Ordering::Relaxed,
        ) else {
            return;
        };
        // if it's not write locked and there are no writers waiting we are allowed to grab the shared lock
        if self.lock_exclusive_once(&mut state) {
            return;
        }
        // we failed to grab the lock, so we need to wait
        self.write_queue.wait_while(
            self.conditional_register_exclusive(),
            self.should_wake_exclusive(),
        );
    }

    fn try_lock_exclusive(&self) -> bool {
        let mut state = self.state.load(Ordering::Relaxed);
        // we can lock as long as it's not at all locked
        while state & (SHARED_LOCK | EXCLUSIVE_LOCK) == 0 {
            if let Err(new_state) = self.state.compare_exchange_weak(
                state,
                state | EXCLUSIVE_LOCK,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                state = new_state;
            } else {
                return true;
            }
        }
        false
    }

    unsafe fn unlock_exclusive(&self) {
        let old_state = self.state.fetch_and(!EXCLUSIVE_LOCK, Ordering::Release);
        self.handle_wake(old_state);
    }
}

unsafe impl lock_api::RawRwLockTimed for RawRwLock {
    type Duration = std::time::Duration;
    type Instant = std::time::Instant;

    fn try_lock_shared_for(&self, timeout: Self::Duration) -> bool {
        if let Some(timeout) = Instant::now().checked_add(timeout) {
            return self.try_lock_shared_until(timeout);
        }
        self.lock_shared();
        true
    }

    fn try_lock_shared_until(&self, timeout: Self::Instant) -> bool {
        let Err(mut state) = self.state.compare_exchange_weak(
            0,
            SHARED_LOCK | ONE_READER,
            Ordering::Acquire,
            Ordering::Relaxed,
        ) else {
            return true;
        };
        // if it's not write locked and there are no writers waiting we are allowed to grab the shared lock
        if self.lock_shared_once(&mut state) {
            return true;
        }
        // we failed to grab the lock, so we need to wait
        self.read_queue.wait_while_until(
            self.conditional_register_shared(),
            self.should_wake_shared(),
            timeout,
        )
    }

    fn try_lock_exclusive_for(&self, timeout: Self::Duration) -> bool {
        if let Some(timeout) = Instant::now().checked_add(timeout) {
            return self.try_lock_exclusive_until(timeout);
        }
        self.lock_exclusive();
        true
    }

    fn try_lock_exclusive_until(&self, timeout: Self::Instant) -> bool {
        let Err(mut state) = self.state.compare_exchange_weak(
            0,
            EXCLUSIVE_LOCK,
            Ordering::Acquire,
            Ordering::Relaxed,
        ) else {
            return true;
        };
        // if it's not write locked and there are no writers waiting we are allowed to grab the shared lock
        if self.lock_exclusive_once(&mut state) {
            return true;
        }
        // we failed to grab the lock, so we need to wait
        self.write_queue.wait_while_until(
            self.conditional_register_exclusive(),
            self.should_wake_exclusive(),
            timeout,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use itertools::Itertools;
    use lock_api::RawRwLock as RWLockAPI;
    use std::sync::atomic::AtomicU8;
    use std::sync::Arc;

    #[test]
    fn shared() {
        let lock = RawRwLock::new();
        lock.lock_shared();
        lock.lock_shared();
        lock.lock_shared();
        let state = lock.state.load(Ordering::Relaxed);
        assert_eq!(state, (ONE_READER * 3) | SHARED_LOCK);

        unsafe {
            lock.unlock_shared();
        }
        let state = lock.state.load(Ordering::Relaxed);
        assert_eq!(state, (ONE_READER * 2) | SHARED_LOCK);

        unsafe {
            lock.unlock_shared();
        }
        let state = lock.state.load(Ordering::Relaxed);
        assert_eq!(state, (ONE_READER * 1) | SHARED_LOCK);

        unsafe {
            lock.unlock_shared();
        }
        let state = lock.state.load(Ordering::Relaxed);
        assert_eq!(state, 0);
    }

    #[test]
    fn try_exclusive_fails() {
        let lock = RawRwLock::new();
        assert!(lock.try_lock_shared());
        assert!(!lock.try_lock_exclusive());
        let state = lock.state.load(Ordering::Relaxed);
        assert_eq!(state, ONE_READER | SHARED_LOCK);
    }

    #[test]
    fn readers_wait() {
        let value = Arc::new(AtomicU8::new(0));
        let lock = Arc::new(RawRwLock::new());
        let barrier = Arc::new(std::sync::Barrier::new(4));

        let handles = (0..3)
            .map(|_| {
                let lock = lock.clone();
                let value = value.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    lock.lock_shared();
                    assert!(num_readers(lock.state.load(Ordering::Relaxed)) <= 3);
                    assert_eq!(value.load(Ordering::Relaxed), 42);
                    unsafe {
                        lock.unlock_shared();
                    }
                })
            })
            .collect_vec();

        lock.lock_exclusive();
        assert_eq!(lock.state.load(Ordering::Relaxed), EXCLUSIVE_LOCK);
        barrier.wait();
        while lock.read_queue.num_waiting() != 3 {
            std::thread::yield_now();
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert_eq!(lock.read_queue.num_waiting(), 3);
        value.store(42, Ordering::Relaxed);
        unsafe {
            lock.unlock_exclusive();
        }

        for thread in handles {
            thread.join().unwrap();
        }
        assert_eq!(lock.state.load(Ordering::Relaxed), 0);
    }
}
