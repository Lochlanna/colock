mod atomic_const_ptr;

use crate::event::maybe_ref::MaybeRef;
use crate::event::parker::Parker;
use crate::spinwait::SpinWait;
use atomic_const_ptr::AtomicConstPtr;
use std::cell::{Cell, UnsafeCell};
use std::ops::{Deref, DerefMut};
use std::pin::{pin, Pin};
use std::ptr::null;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

pub struct IntrusiveLifo<T>
where
    T: 'static,
{
    head: AtomicConstPtr<Node<T>>,
    crit_counter: AtomicUsize,
}

impl<T> IntrusiveLifo<T> {
    const fn new() -> Self {
        Self {
            head: AtomicConstPtr::new(null()),
            crit_counter: AtomicUsize::new(0),
        }
    }
    fn peak(&self) -> *const Node<T> {
        self.head.load(Ordering::Acquire)
    }
}

pub struct Node<T>
where
    T: 'static,
{
    data: MaybeRef<'static, T>,
    next: Cell<*const Node<T>>,
}

impl<T> Node<T>
where
    T: 'static,
{
    const fn new(data: MaybeRef<'static, T>) -> Self {
        Self {
            data,
            next: Cell::new(null()),
        }
    }
    fn push(self: Pin<&Self>, queue: &IntrusiveLifo<T>) {
        let mut head = queue.head.load(Ordering::Relaxed);
        loop {
            self.next.set(head);
            if let Err(new_head) = queue.head.compare_exchange_weak(
                head,
                self.get_ref(),
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                head = new_head;
                continue;
            }
            return;
        }
    }

    //TODO document how to use this
    unsafe fn remove_from_queue(self: Pin<&Self>, queue: &IntrusiveLifo<T>) {
        debug_assert_eq!(queue.crit_counter.fetch_add(1, Ordering::Acquire), 0);

        let mut current = queue.head.load(Ordering::Acquire);
        let self_ptr = self.get_ref() as *const _;
        let next = self.next.get();
        if current == self_ptr {
            if let Err(new_head) =
                queue
                    .head
                    .compare_exchange(current, next, Ordering::AcqRel, Ordering::Acquire)
            {
                current = new_head;
            } else {
                debug_assert_eq!(queue.crit_counter.fetch_sub(1, Ordering::Acquire), 1);
                return;
            }
        }

        debug_assert!(!current.is_null());

        let mut prev = null();
        while current != self_ptr {
            prev = current;
            current = (*current).next.get();
            assert!(!current.is_null());
        }

        (*prev).next.set(next);

        debug_assert_eq!(queue.crit_counter.fetch_sub(1, Ordering::Acquire), 1);
    }
}

pub struct MiniLock<T> {
    is_locked: AtomicBool,
    queue: IntrusiveLifo<Parker>,
    data: UnsafeCell<T>,
}

unsafe impl<T> Sync for MiniLock<T> {}
unsafe impl<T> Send for MiniLock<T> where T: Send {}

impl<T> MiniLock<T> {
    pub const fn new(data: T) -> Self {
        Self {
            is_locked: AtomicBool::new(false),
            queue: IntrusiveLifo::new(),
            data: UnsafeCell::new(data),
        }
    }

    pub fn lock(&self) -> MiniLockGuard<T> {
        let mut spin = SpinWait::new();
        while spin.spin() {
            if let Some(guard) = self.try_lock_weak() {
                return guard;
            }
        }

        thread_local! {
            static PARKER: Parker = const {Parker::new()};
        }

        let node = pin!(PARKER.with(|parker| {
            let parker: &'static Parker = unsafe { std::mem::transmute(parker) };
            Node::new(MaybeRef::Ref(parker))
        }));

        if let Some(guard) = self.try_lock() {
            return guard;
        }
        //push onto the queue
        node.data.prepare_park();
        node.as_ref().push(&self.queue);

        if let Some(guard) = self.try_lock() {
            unsafe { node.as_ref().remove_from_queue(&self.queue) }
            return guard;
        }

        node.data.park();

        debug_assert!(self.is_locked());

        unsafe { node.as_ref().remove_from_queue(&self.queue) }

        MiniLockGuard { inner: self }
    }

    pub fn try_lock(&self) -> Option<MiniLockGuard<T>> {
        if self
            .is_locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            return Some(MiniLockGuard { inner: self });
        }
        None
    }

    fn try_lock_weak(&self) -> Option<MiniLockGuard<T>> {
        if self
            .is_locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            return Some(MiniLockGuard { inner: self });
        }
        None
    }

    unsafe fn unlock(&self) -> bool {
        loop {
            self.is_locked.store(false, Ordering::Release);
            if !self.queue.peak().is_null() && self.try_lock_internal() {
                debug_assert!(self.is_locked());
                let node = self.queue.peak();
                if node.is_null() {
                    continue;
                }
                let did_unpark = (*node).data.unpark_handle().un_park();
                debug_assert!(did_unpark);
                return true;
            }
            return false;
        }
    }

    fn try_lock_internal(&self) -> bool {
        self.is_locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    pub fn is_locked(&self) -> bool {
        self.is_locked.load(Ordering::Acquire)
    }
}

pub struct MiniLockGuard<'lock, T> {
    inner: &'lock MiniLock<T>,
}

impl<T> Drop for MiniLockGuard<'_, T> {
    fn drop(&mut self) {
        unsafe { self.inner.unlock() };
    }
}

impl<T> Deref for MiniLockGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.inner.data.get() }
    }
}

impl<T> DerefMut for MiniLockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.inner.data.get() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn it_works_threaded() {
        let mutex = MiniLock::new(());
        let barrier = std::sync::Barrier::new(2);
        let num_iterations = 10;
        thread::scope(|s| {
            s.spawn(|| {
                for _ in 0..num_iterations {
                    let guard = mutex.lock();
                    barrier.wait();
                    while mutex.queue.head.load(Ordering::Acquire).is_null() {
                        thread::yield_now();
                    }
                    thread::sleep(Duration::from_millis(10));
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
                assert!(elapsed >= 10);
                drop(guard);
                barrier.wait();
            }
        });
        assert!(!mutex.is_locked());
    }

    fn do_lots_and_lots(j: u64, k: u64) {
        let m = MiniLock::new(0_u64);

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
        const J: u64 = 1_000_000;
        // const J: u64 = 10000000;
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
