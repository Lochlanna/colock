use core::sync::atomic::AtomicUsize;
use event::Event;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

#[derive(Debug, Default)]
pub struct RawRwLock {
    state: AtomicUsize,
    reader_queue: Event,
    writer_queue: Event,
}

impl RawRwLock {
    pub const fn new() -> Self {
        Self {
            state: AtomicUsize::new(0),
            reader_queue: Event::new(),
            writer_queue: Event::new(),
        }
    }

    fn try_lock_shared_inner(&self, state: &mut usize) -> bool {
        while !state.is_exclusive_locked() {
            let target = (*state | SHARED_LOCK) + ONE_SHARED;
            if let Err(new_state) = self.state.compare_exchange_weak(
                *state,
                target,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                *state = new_state;
                continue;
            }
            *state = target;
            return true;
        }
        false
    }

    fn try_lock_exclusive_inner(&self, state: &mut usize) -> bool {
        while !state.is_locked() {
            let target = *state | EXCLUSIVE_LOCK;
            if let Err(new_state) = self.state.compare_exchange_weak(
                *state,
                target,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                *state = new_state;
                continue;
            }
            *state = target;
            return true;
        }
        false
    }

    fn lock_shared_wait_conditions(&self) -> (impl Fn(usize) -> bool + '_, impl Fn() -> bool + '_) {
        let should_sleep = |_| {
            let mut state = self.state.load(Ordering::Acquire);
            loop {
                if state.is_exclusive_locked() && state.shared_waiting() {
                    return true;
                }
                let target;
                let ordering;
                if state.is_exclusive_locked() {
                    target = state | SHARED_WAITING;
                    ordering = Ordering::Relaxed;
                } else if state.is_shared_locked() {
                    target = state + ONE_SHARED;
                    ordering = Ordering::Relaxed;
                } else {
                    target = (state + ONE_SHARED) | SHARED_LOCK;
                    ordering = Ordering::Acquire;
                }
                match self
                    .state
                    .compare_exchange_weak(state, target, ordering, Ordering::Relaxed)
                {
                    Ok(_) => {
                        if !state.is_locked() || state.is_shared_locked() {
                            return false;
                        }
                        return true;
                    }
                    Err(new_state) => state = new_state,
                }
            }
        };
        let should_wake = || self.try_lock_shared();
        (should_sleep, should_wake)
    }
    fn lock_shared_slow(&self) {
        let (should_sleep, should_wake) = self.lock_shared_wait_conditions();
        self.reader_queue.wait_while(should_sleep, should_wake);
    }

    fn lock_shared_slow_timed(&self, timeout: Instant) -> bool {
        let (should_sleep, should_wake) = self.lock_shared_wait_conditions();
        self.reader_queue
            .wait_while_until(should_sleep, should_wake, timeout)
    }

    fn lock_exclusive_wait_conditions(
        &self,
    ) -> (impl Fn(usize) -> bool + '_, impl Fn() -> bool + '_) {
        let should_sleep = |_| {
            let mut state = self.state.load(Ordering::Acquire);
            loop {
                if state.is_locked() && state.exclusive_waiting() {
                    return true;
                }
                let (target, ordering) = if state.is_locked() {
                    (state | EXCLUSIVE_WAITING, Ordering::Relaxed)
                } else {
                    (state | EXCLUSIVE_LOCK, Ordering::Acquire)
                };
                match self
                    .state
                    .compare_exchange_weak(state, target, ordering, Ordering::Relaxed)
                {
                    Ok(_) => {
                        if !state.is_locked() {
                            return false;
                        }
                        return true;
                    }
                    Err(new_state) => state = new_state,
                }
            }
        };
        let should_wake = || self.try_lock_exclusive();
        (should_sleep, should_wake)
    }

    fn lock_exclusive_slow(&self) {
        let (should_sleep, should_wake) = self.lock_exclusive_wait_conditions();
        self.writer_queue.wait_while(should_sleep, should_wake);
        debug_assert!(self.state.load(Ordering::Relaxed).is_exclusive_locked());
    }

    fn lock_exclusive_slow_timed(&self, timeout: Instant) -> bool {
        let (should_sleep, should_wake) = self.lock_exclusive_wait_conditions();
        self.writer_queue
            .wait_while_until(should_sleep, should_wake, timeout)
    }

    fn shared_notify(&self, state: usize) {
        // try writers if no readers were notified
        if state.exclusive_waiting() {
            let did_notify = self.writer_queue.notify_if(
                |num_left| {
                    if num_left == 1 {
                        self.state.fetch_and(!EXCLUSIVE_WAITING, Ordering::Relaxed);
                    }
                    true
                },
                || {
                    self.state.fetch_and(!EXCLUSIVE_WAITING, Ordering::Relaxed);
                },
            );
            if did_notify {
                return;
            }
        }

        if state.shared_waiting() {
            self.notify_all_readers();
        }
    }

    fn exclusive_notify(&self, state: usize) {
        // writers should hand over to readers first
        if state.shared_waiting() {
            let num_notified = self.notify_all_readers();
            if num_notified > 0 {
                return;
            }
        }

        // try writers if no readers were notified
        if state.exclusive_waiting() {
            self.writer_queue.notify_if(
                |num_left| {
                    if num_left == 1 {
                        self.state.fetch_and(!EXCLUSIVE_WAITING, Ordering::Relaxed);
                    }
                    true
                },
                || {
                    self.state.fetch_and(!EXCLUSIVE_WAITING, Ordering::Relaxed);
                },
            );
        }
    }

    pub async fn lock_shared_async(&self) {
        let Err(mut state) = self.state.compare_exchange_weak(
            0,
            SHARED_LOCK | ONE_SHARED,
            Ordering::Acquire,
            Ordering::Relaxed,
        ) else {
            return;
        };
        // if it's not write locked and there are no writers waiting we are allowed to grab the shared lock
        if self.try_lock_shared_inner(&mut state) {
            return;
        }
        let (should_sleep, should_wake) = self.lock_shared_wait_conditions();
        self.reader_queue
            .wait_while_async(should_sleep, should_wake)
            .await;
        debug_assert!(self.state.load(Ordering::Relaxed).is_shared_locked());
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
        if self.try_lock_exclusive_inner(&mut state) {
            return;
        }
        let (should_sleep, should_wake) = self.lock_exclusive_wait_conditions();
        self.writer_queue
            .wait_while_async(should_sleep, should_wake)
            .await;
        debug_assert!(self.state.load(Ordering::Relaxed).is_exclusive_locked());
    }

    fn notify_all_readers(&self) -> usize {
        self.reader_queue.notify_all_while(
            |num_left| {
                if num_left == 1 {
                    self.state.fetch_and(!SHARED_WAITING, Ordering::Relaxed);
                }
                true
            },
            || {
                self.state.fetch_and(!SHARED_WAITING, Ordering::Relaxed);
            },
        )
    }

    pub fn lock_shared(&self) {
        let Err(mut state) = self.state.compare_exchange_weak(
            0,
            SHARED_LOCK | ONE_SHARED,
            Ordering::Acquire,
            Ordering::Relaxed,
        ) else {
            return;
        };
        if self.try_lock_shared_inner(&mut state) {
            return;
        }
        self.lock_shared_slow();
        debug_assert!(self.state.load(Ordering::Relaxed).is_shared_locked());
    }

    pub fn try_lock_shared(&self) -> bool {
        let Err(mut state) = self.state.compare_exchange_weak(
            0,
            SHARED_LOCK | ONE_SHARED,
            Ordering::Acquire,
            Ordering::Relaxed,
        ) else {
            return true;
        };
        self.try_lock_shared_inner(&mut state)
    }

    pub unsafe fn unlock_shared(&self) {
        let mut state = self.state.fetch_sub(ONE_SHARED, Ordering::Relaxed);
        debug_assert!(state.num_shared() > 0);
        if state.num_shared() == 1 {
            state -= ONE_SHARED;
            while let Err(new_state) = self.state.compare_exchange_weak(
                state,
                state & !SHARED_LOCK,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                state = new_state;
                if state.num_shared() > 0 {
                    // another reader has barged the lock
                    return;
                }
            }
            debug_assert_eq!(state.num_shared(), 0);
            if state.shared_waiting() || state.exclusive_waiting() {
                self.shared_notify(state);
            }
        }
    }

    pub fn lock_exclusive(&self) {
        let Err(mut state) = self.state.compare_exchange_weak(
            0,
            EXCLUSIVE_LOCK,
            Ordering::Acquire,
            Ordering::Relaxed,
        ) else {
            return;
        };
        if self.try_lock_exclusive_inner(&mut state) {
            return;
        }
        self.lock_exclusive_slow();
        debug_assert!(self.state.load(Ordering::Relaxed).is_exclusive_locked());
    }

    pub fn try_lock_exclusive(&self) -> bool {
        let Err(mut state) = self.state.compare_exchange_weak(
            0,
            EXCLUSIVE_LOCK,
            Ordering::Acquire,
            Ordering::Relaxed,
        ) else {
            return true;
        };
        self.try_lock_exclusive_inner(&mut state)
    }

    pub unsafe fn unlock_exclusive(&self) {
        let mut state = EXCLUSIVE_LOCK;
        while let Err(new_state) = self.state.compare_exchange_weak(
            state,
            state & !EXCLUSIVE_LOCK,
            Ordering::Release,
            Ordering::Relaxed,
        ) {
            state = new_state;
        }
        if state.shared_waiting() || state.exclusive_waiting() {
            self.exclusive_notify(state);
        }
    }

    pub fn is_locked(&self) -> bool {
        self.state.load(Ordering::Acquire).is_locked()
    }

    pub fn is_locked_exclusive(&self) -> bool {
        self.state.load(Ordering::Acquire).is_exclusive_locked()
    }

    pub fn try_lock_shared_for(&self, timeout: Duration) -> bool {
        let timeout = Instant::now() + timeout;
        Self::try_lock_shared_until(self, timeout)
    }

    pub fn try_lock_shared_until(&self, timeout: Instant) -> bool {
        let Err(mut state) = self.state.compare_exchange_weak(
            0,
            SHARED_LOCK | ONE_SHARED,
            Ordering::Acquire,
            Ordering::Relaxed,
        ) else {
            return true;
        };
        if self.try_lock_shared_inner(&mut state) {
            return true;
        }
        self.lock_shared_slow_timed(timeout)
    }

    pub fn try_lock_exclusive_for(&self, timeout: Duration) -> bool {
        let timeout = Instant::now() + timeout;
        self.try_lock_exclusive_until(timeout)
    }

    pub fn try_lock_exclusive_until(&self, timeout: Instant) -> bool {
        let Err(mut state) = self.state.compare_exchange_weak(
            0,
            EXCLUSIVE_LOCK,
            Ordering::Acquire,
            Ordering::Relaxed,
        ) else {
            return true;
        };
        if self.try_lock_exclusive_inner(&mut state) {
            return true;
        }
        self.lock_exclusive_slow_timed(timeout)
    }

    pub unsafe fn unlock_shared_fair(&self) {
        todo!()
    }

    pub unsafe fn unlock_exclusive_fair(&self) {
        todo!()
    }

    //should this be unsafe??
    pub fn bump_shared(&self) {
        todo!()
    }

    pub unsafe fn bump_exclusive(&self) {
        todo!()
    }
    pub fn lock_upgradable(&self) {
        todo!()
    }

    pub fn try_lock_upgradable(&self) -> bool {
        todo!()
    }

    pub unsafe fn unlock_upgradable(&self) {
        todo!()
    }

    pub unsafe fn upgrade(&self) {
        todo!()
    }

    pub unsafe fn try_upgrade(&self) -> bool {
        todo!()
    }

    pub unsafe fn downgrade(&self) {
        debug_assert!(self.state.load(Ordering::Relaxed).is_exclusive_locked());
        let mut state = EXCLUSIVE_LOCK;
        while let Err(new_state) = self.state.compare_exchange_weak(
            state,
            ((state & !EXCLUSIVE_LOCK) | SHARED_LOCK) + ONE_SHARED,
            Ordering::AcqRel,
            Ordering::Relaxed,
        ) {
            state = new_state;
        }
        if state.shared_waiting() {
            self.notify_all_readers();
        }
    }
}

const SHARED_LOCK: usize = 0b1;
const EXCLUSIVE_LOCK: usize = 0b10;
const SHARED_WAITING: usize = 0b100;
const EXCLUSIVE_WAITING: usize = 0b1000;
const ONE_SHARED: usize = 0b10000;
const NUM_SHARED_MASK: usize = !0b1111;

const fn num_readers(state: usize) -> usize {
    state >> 4
}

trait RwLockState: Copy {
    fn is_shared_locked(self) -> bool;
    fn is_exclusive_locked(self) -> bool;
    fn is_locked(self) -> bool;
    fn shared_waiting(self) -> bool;
    fn exclusive_waiting(self) -> bool;
    fn num_shared(self) -> usize;
}

impl RwLockState for usize {
    fn is_shared_locked(self) -> bool {
        self & SHARED_LOCK != 0
    }

    fn is_exclusive_locked(self) -> bool {
        self & EXCLUSIVE_LOCK != 0
    }

    fn is_locked(self) -> bool {
        self & (SHARED_LOCK | EXCLUSIVE_LOCK) != 0
    }

    fn shared_waiting(self) -> bool {
        self & SHARED_WAITING != 0
    }

    fn exclusive_waiting(self) -> bool {
        self & EXCLUSIVE_WAITING != 0
    }

    fn num_shared(self) -> usize {
        self >> 4
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use itertools::Itertools;
    use std::sync::atomic::AtomicU8;
    use std::sync::Arc;

    #[test]
    fn shared() {
        let lock = RawRwLock::new();
        lock.lock_shared();
        lock.lock_shared();
        lock.lock_shared();
        let state = lock.state.load(Ordering::Relaxed);
        assert_eq!(state, (ONE_SHARED * 3) | SHARED_LOCK);

        unsafe {
            lock.unlock_shared();
        }
        let state = lock.state.load(Ordering::Relaxed);
        assert_eq!(state, (ONE_SHARED * 2) | SHARED_LOCK);

        unsafe {
            lock.unlock_shared();
        }
        let state = lock.state.load(Ordering::Relaxed);
        assert_eq!(state, ONE_SHARED | SHARED_LOCK);

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
        assert_eq!(state, ONE_SHARED | SHARED_LOCK);
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
        while lock.reader_queue.num_waiting() != 3 {
            std::thread::yield_now();
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert_eq!(lock.reader_queue.num_waiting(), 3);
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
