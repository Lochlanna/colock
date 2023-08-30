use crate::event::Event;
use std::sync::atomic::{AtomicUsize, Ordering};

const SHARED_LOCK: usize = 0b1;
const EXCLUSIVE_LOCK: usize = 0b10;
const LOCKED: usize = 0b11;
const SHARED_WAITING: usize = 0b100;
const EXCLUSIVE_WAITING: usize = 0b1000;
const ONE_READER: usize = 0b10000;
const READERS_MASK: usize = !0b1111;

pub struct RawRWLock {
    state: AtomicUsize,
    read_queue: Event,
    write_queue: Event,
}

impl RawRWLock {
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

    const fn conditional_register_shared(&self) -> impl Fn() -> bool + '_ {
        || {
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

    const fn on_wake_shared(&self) -> impl Fn() -> bool + '_ {
        || {
            let mut state = self.state.load(Ordering::Relaxed);
            self.lock_shared_once(&mut state)
        }
    }
}

unsafe impl lock_api::RawRwLock for RawRWLock {
    #[allow(clippy::declare_interior_mutable_const)]
    const INIT: Self = Self::new();
    type GuardMarker = ();

    fn lock_shared(&self) {
        let mut state = self.state.load(Ordering::Relaxed);
        // if it's not write locked and there are no writers waiting we are allowed to grab the shared lock
        if self.lock_shared_once(&mut state) {
            return;
        }
        // we failed to grab the lock, so we need to wait
        self.read_queue
            .wait_while(self.conditional_register_shared(), self.on_wake_shared())
    }

    fn try_lock_shared(&self) -> bool {
        let mut state = self.state.load(Ordering::Relaxed);
        // we can lock as long as it's not exclusively locked or waiting to be exclusively locked
        self.lock_shared_once(&mut state)
    }

    unsafe fn unlock_shared(&self) {
        todo!()
    }

    fn lock_exclusive(&self) {
        todo!()
    }

    fn try_lock_exclusive(&self) -> bool {
        let mut state = self.state.load(Ordering::Relaxed);
        // we can lock as long as it's not at all locked
        while state & LOCKED == 0 {
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
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lock_api::RawRwLock as RWLockAPI;

    #[test]
    fn shared() {
        let lock = RawRWLock::new();
        lock.lock_shared();
        lock.lock_shared();
        lock.lock_shared();
        let state = lock.state.load(Ordering::Relaxed);
        assert_eq!(state, (ONE_READER * 3) | SHARED_LOCK);
    }

    #[test]
    fn try_exclusive_fails() {
        let lock = RawRWLock::new();
        assert!(lock.try_lock_shared());
        assert!(!lock.try_lock_exclusive());
        let state = lock.state.load(Ordering::Relaxed);
        assert_eq!(state, ONE_READER | SHARED_LOCK);
    }
}
