#![allow(dead_code)]

mod spinwait;

use event_zero::{Event, EventApi, Listener};
use std::sync::atomic::{AtomicU8, Ordering};

const LOCKED_BIT: u8 = 1;
const WAIT_BIT: u8 = 2;

#[derive(Debug)]
pub struct RawMutexImpl<E>
where
    E: EventApi,
{
    event: E,
    state: AtomicU8,
}

impl<E> RawMutexImpl<E>
where
    E: EventApi,
{
    #[inline(always)]
    fn try_lock_internal(&self, state: &mut u8) -> bool {
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

    fn try_lock_spin(&self, state: &mut u8) -> bool {
        let mut spin_wait = spinwait::SpinWait::new();
        while spin_wait.spin() {
            if let Err(new_state) = self.state.compare_exchange_weak(
                *state & !LOCKED_BIT,
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
}

unsafe impl<E> lock_api::RawMutex for RawMutexImpl<E>
where
    E: EventApi,
{
    const INIT: Self = RawMutexImpl {
        event: E::NEW,
        state: AtomicU8::new(0),
    };
    type GuardMarker = lock_api::GuardSend;

    fn lock(&self) {
        let mut state = self.state.load(Ordering::Acquire);
        if self.try_lock_spin(&mut state) {
            return;
        }

        let mut listener = self.event.new_listener();

        state = self.state.load(Ordering::Relaxed);
        if self.try_lock_internal(&mut state) {
            return;
        }

        loop {
            let did_register = listener.register_if(|| {
                let mut state = self.state.load(Ordering::Acquire);
                loop {
                    let target = if state & LOCKED_BIT == 0 {
                        state | LOCKED_BIT
                    } else if state & WAIT_BIT == 0 {
                        state | WAIT_BIT
                    } else {
                        // wait bit is already set!
                        return true;
                    };
                    match self.state.compare_exchange_weak(
                        state,
                        target,
                        Ordering::Acquire,
                        Ordering::Relaxed,
                    ) {
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
            });
            if !did_register {
                debug_assert!(self.is_locked());
                // we got the lock and didn't register!
                return;
            }
            listener.wait();
            state = self.state.load(Ordering::Acquire);
            if self.try_lock_spin(&mut state) {
                return;
            }
        }
    }

    fn try_lock(&self) -> bool {
        let mut state = self.state.load(Ordering::Acquire);
        self.try_lock_internal(&mut state)
    }

    unsafe fn unlock(&self) {
        let mut state = self.state.load(Ordering::Relaxed);
        while let Err(new_state) = self.state.compare_exchange_weak(
            state,
            state & !LOCKED_BIT,
            Ordering::Release,
            Ordering::Relaxed,
        ) {
            state = new_state;
        }
        if state & WAIT_BIT == 0 {
            return;
        }

        self.event.notify_if(
            |num_waiters_left| {
                if num_waiters_left == 1 {
                    self.state.fetch_and(!WAIT_BIT, Ordering::Relaxed);
                }
                true
            },
            |_| {
                // panic!("didn't expect to fail...");
            },
        );
    }

    fn is_locked(&self) -> bool {
        self.state.load(Ordering::Relaxed) & LOCKED_BIT != 0
    }
}

pub type Mutex<T> = lock_api::Mutex<RawMutexImpl<Event>, T>;
pub type MutexGuard<'a, T> = lock_api::MutexGuard<'a, RawMutexImpl<Event>, T>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn it_works_threaded() {
        let mutex = Mutex::new(());
        let barrier = std::sync::Barrier::new(2);
        let num_iterations = 10;
        thread::scope(|s| {
            s.spawn(|| {
                for _ in 0..num_iterations {
                    let guard = mutex.lock();
                    barrier.wait();
                    thread::sleep(Duration::from_millis(50));
                    drop(guard);
                    barrier.wait();
                }
            });
            for _ in 0..num_iterations {
                barrier.wait();
                assert!(mutex.is_locked());
                let start = Instant::now();
                let guard = mutex.lock();
                let elapsed = start.elapsed().as_millis();
                assert!(elapsed >= 40);
                drop(guard);
                barrier.wait();
            }
        });
        assert!(!mutex.is_locked());
    }

    fn do_lots_and_lots(j: u64, k: u64) {
        let m = Mutex::new(0_u64);

        thread::scope(|s| {
            for _ in 0..k {
                s.spawn(|| {
                    for _ in 0..j {
                        *m.lock() += 1;
                    }
                });
            }
        });

        assert_eq!(*m.lock(), j * k);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn lots_and_lots() {
        const J: u64 = 10000;
        // const J: u64 = 5000000;
        // const J: u64 = 50000000;
        const K: u64 = 6;
        do_lots_and_lots(J, K);
    }

    #[test]
    fn lots_and_lots_miri() {
        const J: u64 = 400;
        const K: u64 = 5;

        do_lots_and_lots(J, K);
    }
}
