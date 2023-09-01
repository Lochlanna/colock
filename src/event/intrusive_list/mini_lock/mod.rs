mod atomic_const_ptr;

use crate::event::maybe_ref::MaybeRef;
use crate::event::parker::Parker;
use crate::spinwait::SpinWait;
use atomic_const_ptr::AtomicConstPtr;
use std::cell::{Cell, UnsafeCell};
use std::ops::{Deref, DerefMut};
use std::pin::pin;
use std::ptr::null;
use std::sync::atomic::{AtomicBool, Ordering};

struct IntrusiveLifo<T>
where
    T: 'static,
{
    head: AtomicConstPtr<Node<T>>,
}

impl<T> IntrusiveLifo<T> {
    const fn new() -> Self {
        Self {
            head: AtomicConstPtr::new(null()),
        }
    }
    fn peak(&self) -> *const Node<T> {
        self.head.load(Ordering::SeqCst)
    }

    fn is_empty(&self) -> bool {
        self.peak().is_null()
    }

    unsafe fn pop(&self) -> Option<&'static Node<T>> {
        let mut head = self.head.load(Ordering::SeqCst);
        loop {
            if head.is_null() {
                return None;
            }
            let current = &*head;
            let next = current.next.get();
            if let Err(new_head) =
                self.head
                    .compare_exchange(current, next, Ordering::SeqCst, Ordering::SeqCst)
            {
                head = new_head;
                continue;
            }
            return Some(current);
        }
    }
}

struct Node<T>
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
    fn push(&self, queue: &IntrusiveLifo<T>) {
        let mut head = queue.head.load(Ordering::SeqCst);
        loop {
            self.next.set(head);
            if let Err(new_head) =
                queue
                    .head
                    .compare_exchange_weak(head, self, Ordering::SeqCst, Ordering::SeqCst)
            {
                head = new_head;
                continue;
            }
            return;
        }
    }

    //TODO document how to use this
    unsafe fn remove_from_queue(&self, queue: &IntrusiveLifo<T>) {
        let next = self.next.get();
        let Err(mut current) =
            queue
                .head
                .compare_exchange(self, next, Ordering::SeqCst, Ordering::SeqCst)
        else {
            return;
        };

        // this node is not the head!
        debug_assert!(!current.is_null());

        let mut prev = null();
        while current != self {
            prev = current;
            current = (*current).next.get();
            assert!(!current.is_null());
        }

        debug_assert_eq!(current, self as *const _);
        debug_assert!(!prev.is_null());

        (*prev).next.set(next);
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

        let node = PARKER.with(|parker| {
            let parker: &'static Parker = unsafe { std::mem::transmute(parker) };
            Node::new(MaybeRef::Ref(parker))
        });

        if let Some(guard) = self.try_lock() {
            return guard;
        }
        //push onto the queue
        node.data.prepare_park();
        node.push(&self.queue);

        if let Some(guard) = self.try_lock() {
            unsafe { node.remove_from_queue(&self.queue) }
            println!("a");
            return guard;
        }

        node.data.park();
        debug_assert!(self.is_locked());
        println!("b");
        MiniLockGuard { inner: self }
    }

    pub fn try_lock(&self) -> Option<MiniLockGuard<T>> {
        if self.try_lock_internal() {
            return Some(MiniLockGuard { inner: self });
        }
        None
    }

    fn try_lock_weak(&self) -> Option<MiniLockGuard<T>> {
        if self
            .is_locked
            .compare_exchange_weak(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            return Some(MiniLockGuard { inner: self });
        }
        None
    }

    fn try_lock_internal(&self) -> bool {
        self.is_locked
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    unsafe fn unlock(&self) -> bool {
        loop {
            debug_assert!(self.is_locked());
            if let Some(node) = self.queue.pop() {
                let handle = node.data.unpark_handle();
                let did_unpark = handle.un_park();
                debug_assert!(did_unpark);
                return true;
            }

            self.is_locked.store(false, Ordering::SeqCst);

            if self.queue.is_empty() {
                // There is no one to wake up
                return false;
            }

            if !self.try_lock_internal() {
                // another thread has already taken the lock
                return false;
            }
        }
    }

    pub fn is_locked(&self) -> bool {
        self.is_locked.load(Ordering::SeqCst)
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
                    while mutex.queue.head.load(Ordering::SeqCst).is_null() {
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
        const J: u64 = 100_000;
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
