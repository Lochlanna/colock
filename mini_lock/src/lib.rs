//! `MiniLock` is a small, light weight, unfair, FILO mutex that does not use any other locks including spin
//! locks. It makes use of thread parking and thread yielding along with a FILO queue to provide
//! a self contained priority inversion safe mutex.
//!
//! `MiniLock` provides only try lock and lock functions. It does not provide any cancellable locking
//! functionality. This restriction allows it to use itself as the lock to modify the queue. Only
//! threads which hold the lock are allowed to modify/remove themselves from the queue.

#![allow(dead_code)]
#![warn(missing_docs)]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::module_name_repetitions)]
#![warn(clippy::undocumented_unsafe_blocks)]

mod atomic_const_ptr;
mod spinwait;

use parking::{ThreadParker, ThreadParkerT};
use spinwait::SpinWait;
use std::cell::UnsafeCell;
use std::fmt::Debug;
use std::ops::{Deref, DerefMut};
use std::ptr;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

/// `LOCKED_BIT` is the bit that is set when the lock is locked
const LOCKED_BIT: usize = 0b1;

/// `PTR_MASK` is the mask that is used to get the pointer to the head of the queue
const PTR_MASK: usize = !LOCKED_BIT;

#[repr(align(2))]
struct Node {
    parker: ThreadParker,
    next: AtomicPtr<Self>,
}

impl Node {
    /// Creates a new node with the given parker
    const fn new(parker: ThreadParker) -> Self {
        Self {
            parker,
            next: AtomicPtr::new(null_mut()),
        }
    }
}

/// A small, light weight, unfair, FILO mutex that does not use any other locks including spin
/// locks. It makes use of thread parking and thread yielding along with a FILO queue to provide
/// a self contained priority inversion safe mutex.
#[derive(Default)]
pub struct MiniLock<T> {
    /// The data that is protected by the mutex
    data: UnsafeCell<T>,
    /// A tagged pointer representing the head of the queue and the lock state
    head: AtomicUsize,
}

impl<T> Debug for MiniLock<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MiniLock")
            .field("is_locked", &self.is_locked())
            .finish()
    }
}

// SAFETY: MiniLock provides the synchronization needed to access the data
unsafe impl<T> Sync for MiniLock<T> where T: Send {}

// SAFETY: MiniLock is safe to send as long as the data is safe to send
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
    fn push(&self, node: &Node) {
        let mut head = self.head.load(Ordering::Acquire);
        loop {
            node.next
                .store((head & PTR_MASK) as *mut Node, Ordering::Relaxed);
            let target = ptr::from_ref(node) as usize | (head & LOCKED_BIT);
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
            node.next.load(Ordering::Relaxed) as usize | LOCKED_BIT,
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
            current_ptr = current.next.load(Ordering::Relaxed);
        }

        debug_assert!(!prev.is_null());
        unsafe {
            (*prev)
                .next
                .store(node.next.load(Ordering::Relaxed), Ordering::Relaxed);
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
        // this checks to see if the queue is empty as the pop function required the lock to be held
        // meaning the locked bit will always be set. Therefore empty list == LOCKED_BIT
        while head != LOCKED_BIT {
            debug_assert_eq!(head & LOCKED_BIT, LOCKED_BIT);
            let head_ptr = (head & PTR_MASK) as *mut Node;
            debug_assert_ne!(head_ptr, null_mut());
            // SAFETY: we know that the head is not null as we checked for that above
            let head_ref = unsafe { &*head_ptr };
            let next = head_ref.next.load(Ordering::Relaxed) as usize | LOCKED_BIT;
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
    #[inline]
    pub fn lock(&self) -> MiniLockGuard<T> {
        let Err(head) =
            self.head
                .compare_exchange(0, LOCKED_BIT, Ordering::Acquire, Ordering::Relaxed)
        else {
            return MiniLockGuard { inner: self };
        };
        self.lock_slow(head)
    }

    /// Locks the mutex and returns a guard that will unlock it when dropped.
    fn lock_slow(&self, mut head: usize) -> MiniLockGuard<T> {
        let mut spin = SpinWait::<3, 7>::new();
        while head & LOCKED_BIT == 0 || spin.spin() {
            if let Err(new_head) = self.head.compare_exchange_weak(
                head & !LOCKED_BIT,
                head | LOCKED_BIT,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                head = new_head;
            } else {
                return MiniLockGuard { inner: self };
            }
        }
        Self::with_node(|node| {
            if let Some(guard) = self.try_lock() {
                return guard;
            }
            //push onto the queue
            unsafe { node.parker.prepare_park() };
            self.push(node);

            if let Some(guard) = self.try_lock() {
                // SAFETY: we now hold the lock meaning that we are allowed to modify the queue
                unsafe {
                    self.remove(node);
                }
                return guard;
            }

            unsafe { node.parker.park() };
            debug_assert_eq!(self.head.load(Ordering::Relaxed) & LOCKED_BIT, LOCKED_BIT);

            MiniLockGuard { inner: self }
        })
    }

    /// Executes the given function with a node.
    /// The node might be constructed or it might be a thread local.
    #[inline]
    fn with_node<R>(f: impl FnOnce(&Node) -> R) -> R {
        if ThreadParker::IS_CHEAP_TO_CONSTRUCT {
            let node = Node::new(ThreadParker::const_new());
            return f(&node);
        }
        thread_local! {
            static NODE: Node = const {Node::new(ThreadParker::const_new())};
        }
        NODE.with(f)
    }

    /// Attempts to acquire this lock without parking.
    pub fn try_lock(&self) -> Option<MiniLockGuard<T>> {
        let mut head = self.head.load(Ordering::Acquire);
        if self.try_lock_internal(&mut head) {
            return Some(MiniLockGuard { inner: self });
        }
        None
    }

    /// Attempts to acquire this lock without parking.
    #[inline]
    fn try_lock_internal(&self, head: &mut usize) -> bool {
        while *head & LOCKED_BIT == 0 {
            if let Err(new_head) = self.head.compare_exchange_weak(
                *head,
                *head | LOCKED_BIT,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                *head = new_head;
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
            let mut head = self.head.fetch_and(!LOCKED_BIT, Ordering::Release);

            if head == LOCKED_BIT {
                // there were no waiters
                return false;
            }
            head &= !LOCKED_BIT;
            if !self.try_lock_internal(&mut head) {
                // the lock was already locked by another thread
                return false;
            }

            if let Some(node) = self.pop() {
                // we got a waiter!
                // the lock is locked, we are passing it to the waking thread!
                unsafe {
                    node.parker.unpark();
                }
                return true;
            }
        }
    }

    /// Returns true if the mutex is locked
    pub fn is_locked(&self) -> bool {
        self.head.load(Ordering::Relaxed) & LOCKED_BIT == LOCKED_BIT
    }
}

/// A guard that will unlock the mutex when dropped and allows access to the data via [`Deref`] and [`DerefMut`]
pub struct MiniLockGuard<'lock, T> {
    inner: &'lock MiniLock<T>,
}

impl<T> Drop for MiniLockGuard<'_, T> {
    fn drop(&mut self) {
        // SAFETY: we know we hold the lock as LockGuard is only given out
        // when the lock is held and is not Sync
        unsafe { self.inner.unlock() };
    }
}

impl<T> Deref for MiniLockGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // SAFETY: we have exclusive access to the data as LockGuard is only given out
        // when the lock is held
        unsafe { &*self.inner.data.get() }
    }
}

impl<T> DerefMut for MiniLockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: we have exclusive access to the data as LockGuard is only given out
        // when the lock is held
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
        const J: u64 = 1_0000_000;
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
