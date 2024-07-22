use crate::lock_utils::{MaybeAsyncWaker, Timeout};
use core::sync::atomic::AtomicUsize;
use intrusive_list::{ConcurrentIntrusiveList, Error, Node, NodeAction, ScanAction};
use parking::{Parker, ThreadParker, ThreadParkerT};
use std::cell::{Cell, UnsafeCell};
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
    fn num_shared(self) -> usize;
    fn is_fair(self) -> bool;
    fn exclusive_waiting(self) -> bool;
}

impl State for usize {
    fn is_shared_locked(self) -> bool {
        self.num_shared() > 0 && self & EXCLUSIVE_LOCK == 0
    }

    fn is_exclusive_locked(self) -> bool {
        self & EXCLUSIVE_LOCK == EXCLUSIVE_LOCK
    }

    fn is_unlocked(self) -> bool {
        self == UNLOCKED
    }

    fn num_shared(self) -> usize {
        self >> 3
    }

    fn is_fair(self) -> bool {
        self & FAIR == FAIR
    }

    fn exclusive_waiting(self) -> bool {
        self & EXCLUSIVE_WAITING == EXCLUSIVE_WAITING
    }
}

#[derive(Debug)]
enum WaitToken {
    Reader(MaybeAsyncWaker),
    Writer(MaybeAsyncWaker),
}

impl WaitToken {
    fn wake(&self) {
        match self {
            Self::Reader(w) | Self::Writer(w) => w.wake_by_ref(),
        }
    }

    fn is_writer(&self) -> bool {
        match self {
            WaitToken::Reader(_) => false,
            WaitToken::Writer(_) => true,
        }
    }

    fn is_reader(&self) -> bool {
        !self.is_writer()
    }
}

#[derive(Debug)]
pub struct RawRwLock {
    state: AtomicUsize,
    queue: ConcurrentIntrusiveList<WaitToken>,
    //todo can we make this safer so it cant be accidentally used wrong...?
    exclusive_waiting_count: Cell<usize>, // this value can only be accessed from within the LL lock...
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
            queue: ConcurrentIntrusiveList::new(),
            exclusive_waiting_count: Cell::new(0),
        }
    }

    pub fn lock_shared_async(&self) -> SharedRawRwMutexPoller {
        SharedRawRwMutexPoller {
            raw_rw_lock: self,
            node: None,
        }
    }

    pub fn lock_exclusive_async(&self) -> ExclusiveRawRwMutexPoller {
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

    fn should_sleep_shared(&self) -> (bool, ()) {
        if self.state.load(Ordering::Acquire).is_shared_locked() {
            // it's already read locked so don't bother
            return (false, ());
        }
        (true, ())
    }

    fn lock_shared_inner<T: Timeout>(&self, timeout: Option<T>) -> bool {
        let mut state = self.state.fetch_add(ONE_READER, Ordering::Acquire) + ONE_READER;
        loop {
            if state.is_shared_locked() {
                return true;
            }

            let parker = Self::get_parker();
            parker.prepare_park();
            let waker = MaybeAsyncWaker::Parker(parker.waker());
            let token = WaitToken::Reader(waker);

            let mut node = ConcurrentIntrusiveList::make_node(token);
            let pinned_node = pin!(node);

            let (park_result, _) = self
                .queue
                .push_head(pinned_node, |_, _| self.should_sleep_shared());
            if let Err(park_error) = park_result {
                if park_error == Error::AbortedPush {
                    //we got the lock
                    return true;
                }
                panic!("The node was dirty");
            }

            state = self.state.load(Ordering::Acquire);
            if state.is_shared_locked() {
                return true;
            }
            if let Some(timeout) = &timeout {
                if !parker.park_until(timeout.to_instant()) {
                    loop {
                        if let Err(new_state) = self.state.compare_exchange(
                            state,
                            state - ONE_READER,
                            Ordering::Release,
                            Ordering::Acquire,
                        ) {
                            state = new_state;
                            if state.is_shared_locked() {
                                return true;
                            }
                        } else {
                            return false;
                        }
                    }
                }
            } else {
                parker.park();
            }
            state = self.state.load(Ordering::Acquire);
        }
    }

    pub fn lock_shared(&self) {
        self.lock_shared_inner::<Instant>(None);
    }

    pub fn try_lock_shared(&self) -> bool {
        let Err(mut state) =
            self.state
                .compare_exchange(UNLOCKED, ONE_READER, Ordering::Acquire, Ordering::Acquire)
        else {
            return true;
        };
        while !state.is_exclusive_locked() {
            let Err(new_state) = self.state.compare_exchange(
                state,
                state + ONE_READER,
                Ordering::Acquire,
                Ordering::Acquire,
            ) else {
                return true;
            };
            state = new_state;
        }
        false
    }

    pub unsafe fn unlock_shared(&self) {
        let state = self.state.fetch_sub(ONE_READER, Ordering::Release);
        if state.num_shared() == 0 && state.exclusive_waiting() {
            // we unlocked the shared lock and there is a waiting writer
            self.queue.pop_tail(
                |waker, num_waiting| {
                    debug_assert!(waker.is_writer());
                    if waker.is_writer() {
                        let waiting_writer_count = self.exclusive_waiting_count.get();
                        debug_assert!(waiting_writer_count > 0);
                        self.exclusive_waiting_count.set(waiting_writer_count - 1);
                    }
                    waker.wake();
                    if num_waiting == 0 {
                        self.state.fetch_and(!EXCLUSIVE_WAITING, Ordering::Release);
                    }
                },
                || {},
            );
        }
    }

    fn should_sleep_exclusive(&self) -> (bool, ()) {
        let mut state = self.state.load(Ordering::Acquire);
        while state.is_unlocked() || state & EXCLUSIVE_WAITING == 0 {
            if state.is_unlocked() {
                if let Err(new_state) = self.state.compare_exchange(
                    state,
                    state | EXCLUSIVE_LOCK,
                    Ordering::Acquire,
                    Ordering::Acquire,
                ) {
                    state = new_state;
                } else {
                    // we got the lock abort the push
                    return (false, ());
                }
            } else {
                state = self
                    .state
                    .compare_exchange(
                        state,
                        state | EXCLUSIVE_WAITING,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .unwrap_or_else(|u| u);
            }
        }
        self.exclusive_waiting_count
            .set(self.exclusive_waiting_count.get() + 1);
        (true, ())
    }

    fn lock_exclusive_inner<T: Timeout>(&self, timeout: Option<T>) -> bool {
        loop {
            if self.try_lock_exclusive() {
                return true;
            }

            let parker = Self::get_parker();
            parker.prepare_park();
            let waker = MaybeAsyncWaker::Parker(parker.waker());
            let token = WaitToken::Writer(waker);

            let mut node = ConcurrentIntrusiveList::make_node(token);
            let pinned_node = pin!(node);

            let (park_result, _) = self
                .queue
                .push_head(pinned_node, |_, _| self.should_sleep_exclusive());

            if let Err(park_error) = park_result {
                if park_error == Error::AbortedPush {
                    //we got the lock
                    return true;
                }
                panic!("The node was dirty");
            }

            if let Some(timeout) = &timeout {
                if !parker.park_until(timeout.to_instant()) {
                    return false;
                }
            } else {
                parker.park();
            }

            let state = self.state.load(Ordering::Acquire);
            if state.is_exclusive_locked() && state.is_fair() {
                // we have been woken to fairly take this lock. We can assume it's ours!
                self.state.fetch_and(!FAIR, Ordering::Acquire);
                return true;
            }
        }
    }

    pub fn lock_exclusive(&self) {
        self.lock_exclusive_inner::<Instant>(None);
    }

    pub fn try_lock_exclusive(&self) -> bool {
        let Err(mut state) = self.state.compare_exchange(
            UNLOCKED,
            EXCLUSIVE_LOCK,
            Ordering::Acquire,
            Ordering::Relaxed,
        ) else {
            return true;
        };
        while state == EXCLUSIVE_WAITING {
            // the exclusive wait bit is set. Attempt to barge the lock!
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

    pub unsafe fn unlock_exclusive(&self) {
        let state = self.state.fetch_and(!EXCLUSIVE_LOCK, Ordering::Release);
        if state.num_shared() > 0 || state.exclusive_waiting() {
            let mut unlock_shared: bool = false;
            self.queue.scan(
                |waker, _| {
                    if !unlock_shared && waker.is_reader() {
                        // we will unpark readers from now on!
                        unlock_shared = true
                    }
                    if unlock_shared {
                        if waker.is_reader() {
                            waker.wake();
                            return (NodeAction::Remove, ScanAction::Continue);
                        }
                        // it's a writer node so ignore it!
                        return (NodeAction::Retain, ScanAction::Continue);
                    }

                    // we are waking just one writer!
                    waker.wake();
                    let writer_wait_count = self.exclusive_waiting_count.get();
                    debug_assert!(writer_wait_count > 0);
                    self.exclusive_waiting_count.set(writer_wait_count - 1);
                    if writer_wait_count == 1 {
                        self.state.fetch_and(!EXCLUSIVE_WAITING, Ordering::Release);
                    }
                    (NodeAction::Remove, ScanAction::Stop)
                },
                || {},
            );
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
        self.queue.pop_tail(
            |waker, num_waiting| {
                debug_assert!(waker.is_writer());
                let mut current_state = self.state.load(Ordering::Acquire);
                loop {
                    let target_state = if current_state.num_shared() == 1 {
                        if num_waiting > 1 {
                            FAIR | EXCLUSIVE_LOCK | EXCLUSIVE_WAITING
                        } else {
                            FAIR | EXCLUSIVE_LOCK
                        }
                    } else {
                        current_state - ONE_READER
                    };
                    match self.state.compare_exchange(
                        current_state,
                        target_state,
                        Ordering::Release,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => {
                            if target_state.is_exclusive_locked() {
                                let writer_waiting_count = self.exclusive_waiting_count.get();
                                debug_assert!(writer_waiting_count > 0);
                                self.exclusive_waiting_count.set(writer_waiting_count - 1);
                                waker.wake();
                            }
                        }
                        Err(new_state) => {
                            current_state = new_state;
                        }
                    }
                }
            },
            || {
                self.state.fetch_sub(ONE_READER, Ordering::Release);
            },
        );
    }

    pub unsafe fn unlock_exclusive_fair(&self) {
        let mut unlock_shared: bool = false;
        let mut unlock = Some(|| self.state.fetch_and(!EXCLUSIVE_LOCK, Ordering::Release));
        self.queue.scan(
            |waker, _| {
                // this ensures we only unlock once...
                if !unlock_shared && waker.is_reader() {
                    if let Some(unlock) = unlock.take() {
                        unlock();
                    }
                    // we will unpark readers from now on!
                    unlock_shared = true
                }
                if unlock_shared {
                    if waker.is_reader() {
                        waker.wake();
                        return (NodeAction::Remove, ScanAction::Continue);
                    }
                    // it's a writer node so ignore it!
                    return (NodeAction::Retain, ScanAction::Continue);
                }
                self.state.fetch_or(FAIR, Ordering::Release);
                // we are waking just one writer!
                waker.wake();
                let writer_wait_count = self.exclusive_waiting_count.get();
                debug_assert!(writer_wait_count > 0);
                self.exclusive_waiting_count.set(writer_wait_count - 1);
                if writer_wait_count == 1 {
                    self.state.fetch_and(!EXCLUSIVE_WAITING, Ordering::Release);
                }
                (NodeAction::Remove, ScanAction::Stop)
            },
            || {
                self.state.fetch_and(!EXCLUSIVE_LOCK, Ordering::Release);
            },
        );
    }

    pub unsafe fn bump_shared(&self) {
        let state = self.state.load(Ordering::Acquire);
        debug_assert!(state.is_shared_locked());
        if !state.exclusive_waiting() {
            return;
        }
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
}

pub struct ExclusiveRawRwMutexPoller<'a> {
    raw_rw_lock: &'a RawRwLock,
    node: Option<Node<WaitToken>>,
}

unsafe impl<'a> Send for ExclusiveRawRwMutexPoller<'a> {}

impl<'a> Future for ExclusiveRawRwMutexPoller<'a> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.raw_rw_lock.try_lock_exclusive() {
            return Poll::Ready(());
        }
        let self_mut = unsafe { self.get_unchecked_mut() };
        let waker = MaybeAsyncWaker::Waker(cx.waker().clone());
        let token = WaitToken::Writer(waker);
        let node = self_mut
            .node
            .insert(ConcurrentIntrusiveList::make_node(token));
        let node = unsafe { Pin::new_unchecked(node) };

        let (park_result, _) = self_mut
            .raw_rw_lock
            .queue
            .push_head(node, |_, _| self_mut.raw_rw_lock.should_sleep_exclusie());

        if let Err(park_error) = park_result {
            if park_error == Error::AbortedPush {
                //we got the lock
                return Poll::Ready(());
            }
            panic!("The node was dirty");
        }

        Poll::Pending
    }
}

pub struct SharedRawRwMutexPoller<'a> {
    raw_rw_lock: &'a RawRwLock,
    node: Option<Node<WaitToken>>,
}

unsafe impl<'a> Send for SharedRawRwMutexPoller<'a> {}

impl<'a> Future for SharedRawRwMutexPoller<'a> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.raw_rw_lock.try_lock_shared() {
            return Poll::Ready(());
        }
        let self_mut = unsafe { self.get_unchecked_mut() };
        let waker = MaybeAsyncWaker::Waker(cx.waker().clone());
        let token = WaitToken::Writer(waker);
        let node = self_mut
            .node
            .insert(ConcurrentIntrusiveList::make_node(token));
        let node = unsafe { Pin::new_unchecked(node) };

        let (park_result, _) = self_mut
            .raw_rw_lock
            .queue
            .push_head(node, |_, _| self_mut.raw_rw_lock.should_sleep_shared());

        if let Err(park_error) = park_result {
            if park_error == Error::AbortedPush {
                //we got the lock
                return Poll::Ready(());
            }
            panic!("The node was dirty");
        }

        Poll::Pending
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
        while lock.queue.count() != 3 {
            std::thread::yield_now();
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert_eq!(lock.queue.count(), 3);
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
