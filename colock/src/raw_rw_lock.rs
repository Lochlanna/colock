use core::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use intrusive_list::ConcurrentIntrusiveList;
use crate::maybe_async::MaybeAsync;
const UNLOCKED: usize = 0;
const EXCLUSIVE_LOCK: usize = 0b1;
const EXCLUSIVE_WAITING: usize = 0b10;
const ONE_READER: usize = 0b100;

trait State: Copy {
    fn is_shared_locked(self)->bool;
    fn is_exclusive_locked(self)->bool;
    fn is_unlocked(self)->bool;
    fn num_shared(self) ->usize;
    fn exclusive_waiting(self)->bool;
}

impl State for usize {
    fn is_shared_locked(self) -> bool {
        self & ONE_READER == ONE_READER && self & EXCLUSIVE_LOCK == 0
    }

    fn is_exclusive_locked(self) -> bool {
        self & EXCLUSIVE_LOCK == EXCLUSIVE_LOCK
    }

    fn is_unlocked(self) -> bool {
        self == UNLOCKED
    }

    fn num_shared(self) -> usize {
        self >> 2
    }

    fn exclusive_waiting(self) -> bool {
        self & EXCLUSIVE_WAITING == EXCLUSIVE_WAITING
    }
}

#[derive(Debug)]
pub struct RawRwLock {
    state: AtomicUsize,
    reader_queue: ConcurrentIntrusiveList<MaybeAsync>,
    writer_queue: ConcurrentIntrusiveList<MaybeAsync>,
}

impl Default for RawRwLock {
    fn default() -> Self {
        Self::new()
    }
}

impl RawRwLock {
    pub const fn new() -> Self {
        Self {
            state: AtomicUsize::new(0),
            reader_queue: ConcurrentIntrusiveList::new(),
            writer_queue: ConcurrentIntrusiveList::new(),
        }
    }

    pub async fn lock_shared_async(&self) {
        todo!()
    }

    pub async fn lock_exclusive_async(&self) {
        todo!()
    }

    pub fn lock_shared(&self) {
        todo!()
    }

    pub fn try_lock_shared(&self) -> bool {
        let Err(mut state) = self.state.compare_exchange(UNLOCKED, ONE_READER, Ordering::Acquire, Ordering::Acquire) else {
            return true;
        };
        while !state.is_exclusive_locked() {
            let Err(new_state) = self.state.compare_exchange(state, state + ONE_READER, Ordering::Acquire, Ordering::Acquire) else {
                return true;
            };
            state = new_state;
        }
        false
    }

    pub unsafe fn unlock_shared(&self) {
        let state = self.state.fetch_sub(ONE_READER, Ordering::Release);
        if state.num_shared() == 0 && state.exclusive_waiting() {
            self.writer_queue.pop_tail(|waker, num_waiting|{
                waker.wake();
                if num_waiting == 0 {
                    self.state.fetch_and(!EXCLUSIVE_WAITING, Ordering::Release);
                }
            }, ||{});
        }
    }

    pub fn lock_exclusive(&self) {
        todo!()
    }

    pub fn try_lock_exclusive(&self) -> bool {
        let Err(mut state) = self.state.compare_exchange(UNLOCKED, EXCLUSIVE_LOCK, Ordering::Acquire, Ordering::Relaxed) else {
            return true;
        };
        while state.num_shared() == 0 && !state.is_exclusive_locked() {
            if let Err(new_state) = self.state.compare_exchange(state, state | EXCLUSIVE_LOCK, Ordering::Acquire, Ordering::Relaxed) {
                state = new_state;
            } else {
                return true;
            }
        }
        false
    }

    pub unsafe fn unlock_exclusive(&self) {
        let state = self.state.fetch_and(!EXCLUSIVE_LOCK, Ordering::Release);
        if state.num_shared() == 0 && state.exclusive_waiting() {
            self.writer_queue.pop_tail(|waker, num_waiting|{
                waker.wake();
                if num_waiting == 0 {
                    self.state.fetch_and(!EXCLUSIVE_WAITING, Ordering::Release);
                }
            }, ||{});
        } else if state.num_shared() > 0 {
            // wake all readers
            self.reader_queue.pop_all(|waker, _|{
                waker.wake();
            }, ||{});
        }
    }

    pub fn is_locked(&self) -> bool {
        !self.state.load(Ordering::Acquire).is_unlocked()
    }

    pub fn is_locked_exclusive(&self) -> bool {
        self.state.load(Ordering::Acquire).is_exclusive_locked()
    }

    pub fn try_lock_shared_for(&self, timeout: Duration) -> bool {
        todo!()
    }

    pub fn try_lock_shared_until(&self, timeout: Instant) -> bool {
        todo!()
    }

    pub fn try_lock_exclusive_for(&self, timeout: Duration) -> bool {
        todo!()
    }

    pub fn try_lock_exclusive_until(&self, timeout: Instant) -> bool {
        todo!()
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
        todo!()
    }
}


//
// #[cfg(test)]
// mod tests {
//     use super::*;
//     use itertools::Itertools;
//     use std::sync::atomic::AtomicU8;
//     use std::sync::Arc;
//
//     #[test]
//     fn shared() {
//         let lock = RawRwLock::new();
//         lock.lock_shared();
//         lock.lock_shared();
//         lock.lock_shared();
//         let state = lock.state.load(Ordering::Relaxed);
//         assert_eq!(state, (ONE_SHARED * 3) | SHARED_LOCK);
//
//         unsafe {
//             lock.unlock_shared();
//         }
//         let state = lock.state.load(Ordering::Relaxed);
//         assert_eq!(state, (ONE_SHARED * 2) | SHARED_LOCK);
//
//         unsafe {
//             lock.unlock_shared();
//         }
//         let state = lock.state.load(Ordering::Relaxed);
//         assert_eq!(state, ONE_SHARED | SHARED_LOCK);
//
//         unsafe {
//             lock.unlock_shared();
//         }
//         let state = lock.state.load(Ordering::Relaxed);
//         assert_eq!(state, 0);
//     }
//
//     #[test]
//     fn try_exclusive_fails() {
//         let lock = RawRwLock::new();
//         assert!(lock.try_lock_shared());
//         assert!(!lock.try_lock_exclusive());
//         let state = lock.state.load(Ordering::Relaxed);
//         assert_eq!(state, ONE_SHARED | SHARED_LOCK);
//     }
//
//     #[test]
//     fn readers_wait() {
//         let value = Arc::new(AtomicU8::new(0));
//         let lock = Arc::new(RawRwLock::new());
//         let barrier = Arc::new(std::sync::Barrier::new(4));
//
//         let handles = (0..3)
//             .map(|_| {
//                 let lock = lock.clone();
//                 let value = value.clone();
//                 let barrier = barrier.clone();
//                 std::thread::spawn(move || {
//                     barrier.wait();
//                     lock.lock_shared();
//                     assert!(num_readers(lock.state.load(Ordering::Relaxed)) <= 3);
//                     assert_eq!(value.load(Ordering::Relaxed), 42);
//                     unsafe {
//                         lock.unlock_shared();
//                     }
//                 })
//             })
//             .collect_vec();
//
//         lock.lock_exclusive();
//         assert_eq!(lock.state.load(Ordering::Relaxed), EXCLUSIVE_LOCK);
//         barrier.wait();
//         while lock.reader_queue.num_waiting() != 3 {
//             std::thread::yield_now();
//         }
//         std::thread::sleep(std::time::Duration::from_millis(10));
//         assert_eq!(lock.reader_queue.num_waiting(), 3);
//         value.store(42, Ordering::Relaxed);
//         unsafe {
//             lock.unlock_exclusive();
//         }
//
//         for thread in handles {
//             thread.join().unwrap();
//         }
//         assert_eq!(lock.state.load(Ordering::Relaxed), 0);
//     }
// }
