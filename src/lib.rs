#![allow(dead_code)]

use event_zero::{Event, EventApi, Listener};
use std::sync::atomic::{AtomicBool, Ordering};
pub struct RawMutexImpl<E>
where
    E: EventApi,
{
    event: E,
    is_locked: AtomicBool,
}

unsafe impl<E> lock_api::RawMutex for RawMutexImpl<E>
where
    E: EventApi,
{
    const INIT: Self = RawMutexImpl {
        event: E::NEW,
        is_locked: AtomicBool::new(false),
    };
    type GuardMarker = lock_api::GuardSend;

    fn lock(&self) {
        if self.try_lock() {
            return;
        }
        let listener = self.event.new_listener();
        loop {
            if self.try_lock() {
                return;
            }
            listener.register();
            if self.try_lock() {
                return;
            }
            listener.wait();
        }
    }

    fn try_lock(&self) -> bool {
        self.is_locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    unsafe fn unlock(&self) {
        self.is_locked.store(false, Ordering::Release);
        self.event.notify_one();
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

    /*
    THE BUG

    t1 - register on queue
    t1 - check for lock and get lock
    t2 - pop, revoke and clone
    t1 - drop and check for revoke (already revoked)
    t1 - push onto queue
    t1 - sleep
    t2 - trigger wake from old reference

    */

    #[test]
    #[cfg_attr(miri, ignore)]
    fn lots_and_lots() {
        // const J: u64 = 10000;
        // const J: u64 = 5000000;
        const J: u64 = 50000000;
        const K: u64 = 6;
        do_lots_and_lots(J, K);
    }

    // #[test]
    // fn lots_and_lots_miri() {
    //     const J: u64 = 400;
    //     const K: u64 = 5;
    //
    //     do_lots_and_lots(J, K);
    // }
}
