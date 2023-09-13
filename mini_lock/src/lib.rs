#![allow(dead_code)]
// #![warn(missing_docs)]
// #![warn(missing_docs_in_private_items)]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::module_name_repetitions)]
#![warn(clippy::undocumented_unsafe_blocks)]

mod atomic_const_ptr;
mod spinwait;

use parking::Parker;
use spinwait::SpinWait;
use std::cell::{Cell, UnsafeCell};
use std::ops::{Deref, DerefMut};
use std::ptr::null_mut;
use std::sync::atomic::{AtomicUsize, Ordering};

const LOCKED_BIT: usize = 0b1;
const PTR_MASK: usize = !LOCKED_BIT;

#[repr(align(2))]
struct Node {
    parker: Parker,
    next: Cell<*mut Self>,
}

impl Node {
    const fn new(parker: Parker) -> Self {
        Self {
            parker,
            next: Cell::new(null_mut()),
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
    /// Creates a new `MiniLock` in an unlocked state ready for use.
    pub const fn new(data: T) -> Self {
        Self {
            data: UnsafeCell::new(data),
            head: AtomicUsize::new(0),
        }
    }

    /// Pushes a node onto the queue.
    ///
    /// # Safety
    /// It is always safe to push a node onto the queue as long as the node is guaranteed to live
    /// long enough for it to be popped off and for the thread which pops it to be finished with it.
    ///
    /// As this is implemented in a mutex and the queue is used for parking it's guaranteed that when
    /// a thread is woken the node has already been removed from the queue and the waking thread
    /// is finished.
    ///
    /// If the thread wants to early out it must hold the lock to do so.
    unsafe fn push(&self, node: &Node) {
        let mut head = self.head.load(Ordering::Acquire);
        loop {
            node.next.set((head & PTR_MASK) as *mut Node);
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

    /// Removes a node from the queue no matter it's position.
    ///
    ///
    /// # Safety
    ///
    /// The mutex must be have been locked by the thread that calls this function.
    /// It is only safe for the holder of the lock to modify the queue
    unsafe fn remove(&self, node: &Node) {
        let node_ptr = node as *const Node;
        debug_assert_eq!(self.head.load(Ordering::Acquire) & LOCKED_BIT, LOCKED_BIT);
        let Err(head) = self.head.compare_exchange(
            node_ptr as usize | LOCKED_BIT,
            node.next.get() as usize | LOCKED_BIT,
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
        while current_ptr.cast_const() != node_ptr {
            let current = unsafe { &*current_ptr };
            prev = current_ptr;
            current_ptr = current.next.get();
        }

        debug_assert!(!prev.is_null());
        unsafe {
            (*prev).next.set(node.next.get());
        }
    }

    /// Pops the head off the queue.
    /// Returns None if the queue is empty.
    ///
    ///
    /// # Safety
    ///
    /// The mutex must be have been locked by the thread that calls this function.
    /// It is only safe for the holder of the lock to modify the queue
    unsafe fn pop(&self) -> Option<&Node> {
        let mut head = self.head.load(Ordering::Relaxed);
        debug_assert_eq!(head & LOCKED_BIT, LOCKED_BIT);
        while head != LOCKED_BIT {
            debug_assert_eq!(head & LOCKED_BIT, LOCKED_BIT);
            let head_ptr = (head & PTR_MASK) as *mut Node;
            let head_ref = unsafe { &*head_ptr };
            let next = head_ref.next.get() as usize | LOCKED_BIT;
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

    /// Locks the mutex and returns a guard that will unlock it when dropped.
    ///
    /// This function will always lock the lock and is not cancellable.
    pub fn lock(&self) -> MiniLockGuard<T> {
        let mut spin = SpinWait::<3, 7>::new();
        while spin.spin() {
            if let Some(guard) = self.try_lock() {
                return guard;
            }
        }

        thread_local! {
            static NODE: Node = const {Node::new(Parker::new())};
        }

        NODE.with(|node| {
            if let Some(guard) = self.try_lock() {
                return guard;
            }
            //push onto the queue
            node.parker.prepare_park();
            unsafe { self.push(node) };

            if let Some(guard) = self.try_lock() {
                //we have the lock so it's safe to remove ourselves from the queue
                unsafe {
                    self.remove(node);
                }
                return guard;
            }

            node.parker.park();
            debug_assert_eq!(self.head.load(Ordering::Relaxed) & LOCKED_BIT, LOCKED_BIT);

            MiniLockGuard { inner: self }
        })
    }

    /// Attempts to acquire this lock without parking.
    pub fn try_lock(&self) -> Option<MiniLockGuard<T>> {
        if self.try_lock_internal() {
            return Some(MiniLockGuard { inner: self });
        }
        None
    }

    /// Attempts to acquire this lock without parking.
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

    /// Unlocks the mutex.
    ///
    /// If there are waiting threads and it is able to re-acquire the lock it will
    /// wake a thread and pass the lock on
    ///
    /// # Safety
    /// It is only safe to call this function if the mutex is locked by the calling thread.
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
                // the lock is locked, we are passing it to the waking thread!
                node.parker.unpark_handle().un_park();
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
