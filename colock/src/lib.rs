#![allow(dead_code)]

use event_zero::{Event, EventApi, Listener};
use std::sync::atomic::{AtomicU8, Ordering};

const LOCKED_BIT: u8 = 1;
const WAIT_BIT: u8 = 2;

pub struct RawMutexImpl<E>
where
    E: EventApi,
{
    event: E,
    state: AtomicU8,
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
        let listener = self.event.new_listener();
        while !self.try_lock() {
            let did_register = listener.register_with_callback(|| {
                let old_state = self.state.fetch_or(WAIT_BIT | LOCKED_BIT, Ordering::SeqCst);
                if old_state & LOCKED_BIT == 0 {
                    // we took the lock abort the register
                    if old_state & WAIT_BIT == 0 {
                        // we were the only waiter
                        self.state.store(LOCKED_BIT, Ordering::SeqCst);
                    }
                    false
                } else {
                    true
                }
            });
            if !did_register {
                // we got the lock and didn't register!
                return;
            }
            listener.wait();
        }
    }

    fn try_lock(&self) -> bool {
        self.state.fetch_or(LOCKED_BIT, Ordering::SeqCst) & LOCKED_BIT == 0
    }

    unsafe fn unlock(&self) {
        if self
            .state
            .compare_exchange(LOCKED_BIT, 0, Ordering::SeqCst, Ordering::Relaxed)
            .is_err()
        {
            self.event.notify_one_with_callback(
                |num_waiters_left| {
                    if num_waiters_left == 0 {
                        self.state.store(0, Ordering::SeqCst);
                    } else {
                        self.state.store(WAIT_BIT, Ordering::SeqCst);
                    }
                },
                |num_left| {
                    assert_eq!(num_left, 0);
                    //just unlock if the queue is empty!
                    self.state.store(0, Ordering::SeqCst);
                },
            );
        }
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
