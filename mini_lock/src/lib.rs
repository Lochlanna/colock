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
use std::cell::UnsafeCell;
use std::ops::{Deref, DerefMut};
use std::ptr;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, fence, Ordering};
use parking::{Parker, ThreadWaker};


const LOCKED_BIT: usize = 0b1;
const PTR_MASK: usize = !LOCKED_BIT;

trait Tagged {
    fn get_ptr(&self)->*mut Node;
    fn get_flag(&self)->bool;
}

impl Tagged for usize {
    fn get_ptr(&self) -> *mut Node {
        (*self & PTR_MASK) as *mut Node
    }

    fn get_flag(&self) -> bool {
        (*self & LOCKED_BIT) == LOCKED_BIT
    }
}


#[repr(align(2))]
struct Node {
    next: *mut Self,
    waker: Option<ThreadWaker>
}

impl Node {
    const fn new(waker: ThreadWaker) -> Self {
        Self {
            next: null_mut(),
            waker: Some(waker),
        }
    }

    fn as_usize_ptr(&self)->usize {
        ptr::from_ref(self) as usize
    }
}

pub struct MiniLock<T>{
    head: AtomicUsize,
    data: UnsafeCell<T>
}

unsafe impl<T> Send for MiniLock<T> where T:Send {}
unsafe impl<T> Sync for MiniLock<T> where T:Send {}

impl<T> MiniLock<T> {
    pub const fn new(data: T)-> Self {
        Self {
            head: AtomicUsize::new(0),
            data: UnsafeCell::new(data),
        }
    }

    fn push(&self, node: &mut Node) {
        assert_eq!(node.next, null_mut());

        if let Err(mut head) = self.head.compare_exchange(LOCKED_BIT, node.as_usize_ptr() | LOCKED_BIT, Ordering::AcqRel, Ordering::Acquire) {
            loop {
                node.next = (head & PTR_MASK) as *mut Node;

                match self.head.compare_exchange(head, node.as_usize_ptr() | (head & LOCKED_BIT), Ordering::AcqRel, Ordering::Acquire) {
                    Err(new_head) => head = new_head,
                    Ok(_) => return,
                }
            }
        }
    }

    fn push_if_locked(&self, node: &mut Node) -> bool {
        assert_eq!(node.next, null_mut());

        if let Err(mut head) = self.head.compare_exchange(LOCKED_BIT, node.as_usize_ptr() | LOCKED_BIT, Ordering::AcqRel, Ordering::Acquire) {
            while head.get_flag() { // while locked
                node.next = (head & PTR_MASK) as *mut Node;

                match self.head.compare_exchange(head, node.as_usize_ptr() | LOCKED_BIT, Ordering::AcqRel, Ordering::Acquire) {
                    Err(new_head) => head = new_head,
                    Ok(_) => return true,
                }
            }
            return false
        }
        true
    }

    fn push_or_lock(&self, node: &mut Node) -> bool {
        assert_eq!(node.next, null_mut());

        let mut head = 0;
        loop {
            if head.get_flag() {
                // it's locked, so we will just try and push the node onto the list
                node.next = (head & PTR_MASK) as *mut Node;
                match self.head.compare_exchange(head, node.as_usize_ptr() | LOCKED_BIT, Ordering::AcqRel, Ordering::Acquire) {
                    Err(new_head) => head = new_head,
                    Ok(_) => return false, // we didn't lock the lock, but we did push the node
                }
            } else {
                // it's not locked. Try and grab the lock!
                match self.head.compare_exchange(head, head | LOCKED_BIT, Ordering::Acquire, Ordering::Acquire) {
                    Err(new_head) => head = new_head,
                    Ok(_) => return true, // we locked the lock
                }
            }
        }
    }

    fn pop(&self)->Option<ThreadWaker> {
        assert!(self.is_locked());

        let mut head = self.head.load(Ordering::Acquire);

        while head != LOCKED_BIT {
            let head_ref = unsafe {&mut *head.get_ptr()};
            let next = head_ref.next;
            if let Err(new_head) = self.head.compare_exchange(head, next as usize | LOCKED_BIT, Ordering::Release, Ordering::Acquire) {
                head = new_head;
            } else {
                // success!
                return head_ref.waker.take()
            }
        }

        None
    }


    fn remove_node(&self, node: &mut Node) {
        assert!(self.is_locked());

        node.waker = None;

        let mut current_node = self.head.load(Ordering::Acquire).get_ptr();
        let mut previous_node: *mut Node = null_mut();
        while !current_node.is_null() {
            let current_ref = unsafe {&mut *current_node};
            if current_node != node {
                previous_node = current_node;
                current_node = current_ref.next;
            } else if previous_node.is_null() {
                // we are popping the head
                if let Err(new_head) = self.head.compare_exchange(current_node as usize | LOCKED_BIT, current_ref.next as usize | LOCKED_BIT, Ordering::Release, Ordering::Acquire) {
                    current_node = new_head.get_ptr();
                } else {
                    // success!
                    return
                }
            } else {
                // our node is not the head
                debug_assert_eq!(current_node, node as *mut Node);
                let previous_ref = unsafe {&mut *previous_node};
                previous_ref.next = current_ref.next;
                return
            }
        }
    }


    pub fn try_lock(&self)->Option<MiniLockGuard<T>> {
        let Err(head) = self.head.compare_exchange(0, LOCKED_BIT, Ordering::Acquire, Ordering::Acquire) else {
            return Some(MiniLockGuard {
                inner: self,
            })
        };
        if self.head.compare_exchange(head & PTR_MASK, head | LOCKED_BIT, Ordering::Acquire, Ordering::Relaxed).is_ok() {
            return Some(MiniLockGuard {
                inner: self,
            })
        }
        None
    }

    pub fn unlock(&self) {
        debug_assert!(self.is_locked());

        loop {
            if self.head.compare_exchange(LOCKED_BIT, 0, Ordering::Release, Ordering::Acquire).is_ok() {
                return;
            }
            // there are waiting nodes to be popped!
            if let Some(waker) = self.pop() {
                // unlock the lock
                self.head.fetch_and(PTR_MASK, Ordering::Release);
                // wake the thread!
                waker.wake();
                return;
            }
        }

    }

    pub fn lock(&self)->MiniLockGuard<T> {
        loop {
            if let Some(guard) = self.try_lock() {
                return guard;
            }

            // we will push ourselves onto the queue and then sleep

            let parker = Parker::new();
            parker.prepare_park();
            let mut node = Node::new(parker.waker());

            if let Some(guard) = self.try_lock() {
                return guard;
            }

            if self.push_or_lock(&mut node) {
                return MiniLockGuard {
                    inner: self,
                }
            }
            parker.park();
        }
    }

    pub fn is_locked(&self)->bool {
        self.head.load(Ordering::Acquire).get_flag()
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
                    while mutex.head.load(Ordering::Acquire).get_ptr().is_null() {
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
