mod atomic_const_ptr;

use crate::event::parker::Parker;
use crate::spinwait::SpinWait;
use std::cell::UnsafeCell;
use std::ops::{Deref, DerefMut};
use std::ptr::null_mut;
use std::sync::atomic::{AtomicUsize, Ordering};

const LOCKED_BIT: usize = 0b1;
const PTR_MASK: usize = !LOCKED_BIT;

#[repr(align(2))]
struct Node {
    data: &'static Parker,
    next: *mut Self,
}

impl Node {
    const fn new(data: &'static Parker) -> Self {
        Self {
            data,
            next: null_mut(),
        }
    }
}

pub struct MiniLock<T> {
    data: UnsafeCell<T>,
    head: AtomicUsize,
}

unsafe impl<T> Sync for MiniLock<T> {}
unsafe impl<T> Send for MiniLock<T> where T: Send {}

impl<T> MiniLock<T> {
    pub const fn new(data: T) -> Self {
        Self {
            data: UnsafeCell::new(data),
            head: AtomicUsize::new(0),
        }
    }

    fn push(&self, node: &mut Node) {
        let mut head = self.head.load(Ordering::Acquire);
        loop {
            node.next = (head & PTR_MASK) as *mut Node;
            let target = node as *const _ as usize | (head & LOCKED_BIT);
            if let Err(new_head) =
                self.head
                    .compare_exchange_weak(head, target, Ordering::Release, Ordering::Relaxed)
            {
                head = new_head;
            } else {
                return;
            }
        }
    }

    fn remove(&self, node: &mut Node) {
        let node_ptr = node as *mut Node;
        debug_assert_eq!(self.head.load(Ordering::Acquire) & LOCKED_BIT, LOCKED_BIT);
        let Err(head) = self.head.compare_exchange(
            node_ptr as usize | LOCKED_BIT,
            node.next as usize | LOCKED_BIT,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) else {
            return;
        };
        let mut current_ptr = (head & PTR_MASK) as *mut Node;
        debug_assert_ne!(current_ptr as usize, node_ptr as usize);
        debug_assert!(!current_ptr.is_null());
        debug_assert_eq!(head & LOCKED_BIT, LOCKED_BIT);

        let mut prev = null_mut();
        while current_ptr != node_ptr {
            let current = unsafe { &*current_ptr };
            prev = current_ptr;
            current_ptr = current.next;
        }

        debug_assert!(!prev.is_null());
        unsafe {
            (*prev).next = node.next;
        }
    }

    fn pop(&self) -> Option<&Node> {
        let mut head = self.head.load(Ordering::Acquire);
        debug_assert_eq!(head & LOCKED_BIT, LOCKED_BIT);
        while head != LOCKED_BIT {
            debug_assert_eq!(head & LOCKED_BIT, LOCKED_BIT);
            let head_ptr = (head & PTR_MASK) as *mut Node;
            let head_ref = unsafe { &*head_ptr };
            let next = head_ref.next as usize | LOCKED_BIT;
            if let Err(new_head) =
                self.head
                    .compare_exchange_weak(head, next, Ordering::Relaxed, Ordering::Relaxed)
            {
                head = new_head;
            } else {
                return Some(head_ref);
            }
        }
        // it's null
        None
    }

    pub fn lock(&self) -> MiniLockGuard<T> {
        let mut spin = SpinWait::<3, 7>::new();
        while spin.spin() {
            if let Some(guard) = self.try_lock() {
                return guard;
            }
        }

        thread_local! {
            static PARKER: Parker = const {Parker::new()};
        }

        let mut node = PARKER.with(|parker| {
            let parker: &'static Parker = unsafe { std::mem::transmute(parker) };
            Node::new(parker)
        });

        if let Some(guard) = self.try_lock() {
            return guard;
        }
        //push onto the queue
        node.data.prepare_park();
        self.push(&mut node);

        if let Some(guard) = self.try_lock() {
            self.remove(&mut node);
            return guard;
        }

        node.data.park();
        debug_assert!(self.is_locked());

        MiniLockGuard { inner: self }
    }

    pub fn try_lock(&self) -> Option<MiniLockGuard<T>> {
        if self.try_lock_internal() {
            return Some(MiniLockGuard { inner: self });
        }
        None
    }

    fn try_lock_internal(&self) -> bool {
        let mut head = self.head.load(Ordering::Acquire);
        while head & LOCKED_BIT == 0 {
            if let Err(new_head) = self.head.compare_exchange_weak(
                head,
                head | LOCKED_BIT,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                head = new_head;
            } else {
                return true;
            }
        }
        false
    }

    unsafe fn unlock(&self) -> bool {
        loop {
            let head = self.head.fetch_and(!LOCKED_BIT, Ordering::Release);

            if head == LOCKED_BIT {
                // there were no waiters
                return false;
            }

            if !self.try_lock_internal() {
                // the lock was already locked by another thread
                return false;
            }

            if let Some(node) = self.pop() {
                // we got a waiter!
                // the lock is still locked
                node.data.unpark_handle().un_park();
                return true;
            }
        }
    }

    pub fn is_locked(&self) -> bool {
        self.head.load(Ordering::Relaxed) & LOCKED_BIT == LOCKED_BIT
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
                    thread::sleep(Duration::from_millis(50));
                    while mutex.head.load(Ordering::Relaxed) & PTR_MASK == 0 {
                        thread::yield_now();
                    }
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
        const K: u64 = 3;

        do_lots_and_lots(J, K);
    }
}
