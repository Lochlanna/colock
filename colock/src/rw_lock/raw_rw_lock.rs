use crate::lock_utils::{MaybeAsyncWaker, Timeout};
use core::sync::atomic::AtomicUsize;
use intrusive_list::{ConcurrentIntrusiveList, Error, Node};
use parking::{Parker, ThreadParker, ThreadParkerT};
use std::future::Future;
use std::pin::{pin, Pin};
use std::sync::atomic::Ordering;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
const UNLOCKED: usize = 0;
const EXCLUSIVE_LOCK: usize = 0b1;
const EXCLUSIVE_WAITING: usize = 0b10;
const FAIR: usize = 0b100;
const ONE_READER: usize = 0b1000;

trait State: Copy {
    fn is_shared_locked(self) -> bool;
    fn is_exclusive_locked(self) -> bool;
    fn is_unlocked(self) -> bool;
    fn exclusive_waiting(self) -> bool;
    fn is_fair(self) -> bool;
    fn num_shared(self) -> usize;
}

impl State for usize {
    fn is_shared_locked(self) -> bool {
        self.num_shared() > 0 && !self.is_exclusive_locked()
    }

    fn is_exclusive_locked(self) -> bool {
        self & EXCLUSIVE_LOCK == EXCLUSIVE_LOCK
    }

    fn is_unlocked(self) -> bool {
        !self.is_exclusive_locked() && self.num_shared() == 0
    }

    fn exclusive_waiting(self) -> bool {
        self & EXCLUSIVE_WAITING == EXCLUSIVE_WAITING
    }

    fn is_fair(self) -> bool {
        self & FAIR == FAIR
    }

    fn num_shared(self) -> usize {
        self >> 3
    }
}

pub struct TaggedWaker {
    waker: MaybeAsyncWaker,
    tag: usize
}

#[derive(Debug)]
pub struct RawRwLock {
    state: AtomicUsize,
    read_queue: ConcurrentIntrusiveList<MaybeAsyncWaker>,
    write_queue: ConcurrentIntrusiveList<MaybeAsyncWaker>,
}

unsafe impl Send for RawRwLock {}
unsafe impl Sync for RawRwLock {}

impl Default for RawRwLock {
    fn default() -> Self {
        Self::new()
    }
}

impl RawRwLock {
    pub const fn new() -> Self {
        Self {
            state: AtomicUsize::new(0),
            read_queue: ConcurrentIntrusiveList::new(),
            write_queue: ConcurrentIntrusiveList::new(),
        }
    }

    pub const fn lock_shared_async(&self) -> SharedRawRwMutexPoller {
        SharedRawRwMutexPoller {
            raw_rw_lock: self,
            node: None,
        }
    }

    pub const fn lock_exclusive_async(&self) -> ExclusiveRawRwMutexPoller {
        ExclusiveRawRwMutexPoller {
            raw_rw_lock: self,
            node: None,
        }
    }

    fn get_parker() -> Parker {
        if ThreadParker::IS_CHEAP_TO_CONSTRUCT {
            return Parker::Owned(ThreadParker::const_new());
        }

        thread_local! {
            static HANDLE: ThreadParker = const {ThreadParker::const_new()}
        }
        HANDLE.with(|handle| unsafe { Parker::Ref(core::mem::transmute(handle)) })
    }
    fn lock_shared_inner<T: Timeout>(&self, timeout: Option<T>) -> bool {
        //TODO handle timeout!
        let mut state = self.state.fetch_add(ONE_READER, Ordering::AcqRel) + ONE_READER;
        while !state.is_shared_locked() {
            debug_assert!(state.is_exclusive_locked());

            // prepare to wait
            let parker = Self::get_parker();
            parker.prepare_park();
            let mut node =
                ConcurrentIntrusiveList::make_node(MaybeAsyncWaker::Parker(parker.waker()));
            let pinned_node = pin!(node);

            let (push_result, _) = self.read_queue.push_head(pinned_node, |_, _| {
                state = self.state.load(Ordering::Acquire);
                if state.is_shared_locked() {
                    return (false, ());
                }
                (true, ())
            });

            if let Err(e) = push_result {
                match e {
                    Error::DirtyNode => panic!("dirty shared node"),
                    Error::AbortedPush => {
                        // we got the lock!
                        return true;
                    }
                }
            }

            // it has now been pushed onto the list. We should check that we didn't get the lock
            // one last time!
            state = self.state.load(Ordering::Acquire);
            if state.is_shared_locked() {
                return true;
            }

            parker.park();

            state = self.state.load(Ordering::Acquire);
        }
        true
    }

    pub fn lock_shared(&self) {
        self.lock_shared_inner::<Instant>(None);
    }

    pub fn try_lock_shared(&self) -> bool {
        let mut current_state = self.state.load(Ordering::Acquire);
        while !current_state.is_exclusive_locked() {
            if let Err(new_state) = self.state.compare_exchange(
                current_state,
                current_state + ONE_READER,
                Ordering::Acquire,
                Ordering::Acquire,
            ) {
                current_state = new_state;
            } else {
                return true;
            }
        }
        false
    }

    pub unsafe fn unlock_shared(&self) {
        let mut state = self.state.fetch_sub(ONE_READER, Ordering::AcqRel);
        debug_assert!(state.num_shared() > 0);
        state -= ONE_READER;
        if state.is_shared_locked() {
            // we were not the last reader so we don't have to do anything here
            return;
        }
        if state.exclusive_waiting() {
            self.wake_writer();
        }
    }
    fn lock_exclusive_inner<T: Timeout>(&self, timeout: Option<T>) -> bool {
        // TODO handle timeout
        while !self.try_lock_exclusive() {
            let parker = Self::get_parker();
            parker.prepare_park();
            let mut node =
                ConcurrentIntrusiveList::make_node(MaybeAsyncWaker::Parker(parker.waker()));
            let pinned_node = pin!(node);

            let (push_result, _) = self.write_queue.push_head(pinned_node, |_, _| {
                let mut state = self.state.load(Ordering::Acquire);
                // lock or set exclusive waiting bit
                loop {
                    if state.is_unlocked() {
                        if let Err(new_state) = self.state.compare_exchange(
                            state,
                            state | EXCLUSIVE_LOCK,
                            Ordering::Acquire,
                            Ordering::Acquire,
                        ) {
                            state = new_state;
                        } else {
                            return (false, ());
                        }
                    } else if !state.exclusive_waiting() {
                        state = self
                            .state
                            .compare_exchange(
                                state,
                                state | EXCLUSIVE_WAITING,
                                Ordering::Release,
                                Ordering::Acquire,
                            )
                            .unwrap_or_else(|u| u);
                    } else {
                        // go to sleep!
                        return (true, ());
                    }
                }
            });

            if let Err(e) = push_result {
                match e {
                    Error::DirtyNode => panic!("dirty shared node"),
                    Error::AbortedPush => {
                        // we got the lock!
                        return true;
                    }
                }
            }

            // it has now been pushed onto the list. We should check that we didn't get the lock
            // one last time!
            let state = self.state.load(Ordering::Acquire);
            if self.try_lock_exclusive() {
                return true;
            }

            parker.park();
        }

        true
    }

    pub fn lock_exclusive(&self) {
        self.lock_exclusive_inner::<Instant>(None);
    }

    pub fn try_lock_exclusive(&self) -> bool {
        let mut state = self.state.load(Ordering::Acquire);
        while state.is_unlocked() {
            if let Err(new_state) = self.state.compare_exchange(
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

    fn wake_writer(&self) {
        self.write_queue.pop_tail(|waker, num_left|{
            waker.wake();
            if num_left == 0 {
                self.state.fetch_and(!EXCLUSIVE_WAITING, Ordering::Release);
            }
        }, ||{
            self.state.fetch_and(!EXCLUSIVE_WAITING, Ordering::Release);
        });
    }

    pub unsafe fn unlock_exclusive(&self) {
        let state = self.state.fetch_and(!EXCLUSIVE_LOCK, Ordering::Release) & !EXCLUSIVE_LOCK;
        if state.is_shared_locked() {
            // wake all the waiting readers!
            self.read_queue.pop_all(
                |waker, _| {
                    waker.wake();
                },
                || {},
            );
        } else if state.exclusive_waiting() {
            self.wake_writer();
        }
    }

    pub fn is_locked(&self) -> bool {
        !self.state.load(Ordering::Acquire).is_unlocked()
    }

    pub fn is_locked_exclusive(&self) -> bool {
        self.state.load(Ordering::Acquire).is_exclusive_locked()
    }

    pub fn try_lock_shared_for(&self, timeout: Duration) -> bool {
        self.lock_shared_inner(Some(timeout))
    }

    pub fn try_lock_shared_until(&self, timeout: Instant) -> bool {
        self.lock_shared_inner(Some(timeout))
    }

    pub fn try_lock_exclusive_for(&self, timeout: Duration) -> bool {
        self.lock_exclusive_inner(Some(timeout))
    }

    pub fn try_lock_exclusive_until(&self, timeout: Instant) -> bool {
        self.lock_exclusive_inner(Some(timeout))
    }

    pub unsafe fn unlock_shared_fair(&self) {
        todo!()
    }

    pub unsafe fn unlock_exclusive_fair(&self) {
        todo!()
    }

    pub unsafe fn bump_shared(&self) {
        let state = self.state.load(Ordering::Acquire);
        debug_assert!(state.is_shared_locked());
        self.unlock_shared_fair();
        self.lock_shared();
    }

    pub unsafe fn bump_exclusive(&self) {
        let state = self.state.load(Ordering::Acquire);
        debug_assert!(state.is_exclusive_locked());
        if state.num_shared() == 0 {
            return;
        }
        self.unlock_exclusive_fair();
        self.lock_exclusive();
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

    pub(crate) fn exclusive_waiting(&self) -> bool {
        self.write_queue.count() > 0
    }
}

pub struct ExclusiveRawRwMutexPoller<'a> {
    raw_rw_lock: &'a RawRwLock,
    node: Option<Node<MaybeAsyncWaker>>,
}

unsafe impl<'a> Send for ExclusiveRawRwMutexPoller<'a> {}

impl<'a> Future for ExclusiveRawRwMutexPoller<'a> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        todo!()
    }
}

pub struct SharedRawRwMutexPoller<'a> {
    raw_rw_lock: &'a RawRwLock,
    node: Option<Node<MaybeAsyncWaker>>,
}

unsafe impl<'a> Send for SharedRawRwMutexPoller<'a> {}

impl<'a> Future for SharedRawRwMutexPoller<'a> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        todo!()
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
        assert_eq!(state, ONE_READER * 3);

        unsafe {
            lock.unlock_shared();
        }
        let state = lock.state.load(Ordering::Relaxed);
        assert_eq!(state, ONE_READER * 2);

        unsafe {
            lock.unlock_shared();
        }
        let state = lock.state.load(Ordering::Relaxed);
        assert_eq!(state, ONE_READER);

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
        assert_eq!(state, ONE_READER);
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
                    assert!(lock.state.load(Ordering::Relaxed).num_shared() <= 3);
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
        while lock.read_queue.count() != 3 {
            std::thread::yield_now();
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert_eq!(lock.read_queue.count(), 3);
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
