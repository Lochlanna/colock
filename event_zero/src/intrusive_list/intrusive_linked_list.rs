use crate::intrusive_list::IntrusiveToken;
use crate::maybe_ref::MaybeRef;
use core::cell::Cell;
use core::fmt::{Debug, Formatter};
use core::ptr::NonNull;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

trait LockablePtr: Copy + Sized {
    fn as_non_null<T>(self) -> Option<NonNull<Node<T>>>;
    fn is_locked(self) -> bool;
    fn is_null(self) -> bool {
        self.as_non_null::<()>().is_none()
    }
    fn with_ptr<T>(self, ptr: Option<NonNull<Node<T>>>) -> Self;
}

const LOCKED: usize = 0x1;
impl LockablePtr for usize {
    fn as_non_null<T>(self) -> Option<NonNull<Node<T>>> {
        let ptr = (self & !LOCKED) as *mut Node<T>;
        NonNull::new(ptr)
    }

    fn is_locked(self) -> bool {
        self & LOCKED == 1
    }

    fn is_null(self) -> bool {
        self & !LOCKED == 0
    }

    fn with_ptr<T>(self, ptr: Option<NonNull<Node<T>>>) -> Self {
        if let Some(ptr) = ptr {
            (self & LOCKED) | (ptr.as_ptr() as usize)
        } else {
            self & LOCKED
        }
    }
}

trait PtrOps {
    fn as_usize(&self) -> usize;
}

impl<T> PtrOps for Option<NonNull<Node<T>>> {
    fn as_usize(&self) -> usize {
        0_usize.with_ptr(*self)
    }
}

struct LockGuard<'a> {
    inner: &'a AtomicUsize,
}

impl Drop for LockGuard<'_> {
    fn drop(&mut self) {
        self.inner.fetch_and(!LOCKED, Ordering::Release);
    }
}

pub struct IntrusiveLinkedList<T> {
    head: AtomicUsize,
    tail: Cell<Option<NonNull<Node<T>>>>,
}

unsafe impl<T> Sync for IntrusiveLinkedList<T> {}
unsafe impl<T> Send for IntrusiveLinkedList<T> {}

impl<T> IntrusiveLinkedList<T> {
    pub const fn new() -> Self {
        Self {
            head: AtomicUsize::new(0),
            tail: Cell::new(None),
        }
    }

    fn lock(&self) -> (LockGuard, usize) {
        let mut head = self.head.load(Ordering::Acquire);
        loop {
            while head.is_locked() {
                core::hint::spin_loop();
                head = self.head.load(Ordering::Relaxed);
            }
            if self
                .head
                .compare_exchange(head, head | LOCKED, Ordering::Acquire, Ordering::Acquire)
                .is_ok()
            {
                return (LockGuard { inner: &self.head }, head | LOCKED);
            }
        }
    }
}

impl<T> super::IntrusiveList<T> for IntrusiveLinkedList<T> {
    #[allow(clippy::declare_interior_mutable_const)]
    const NEW: Self = Self::new();
    type Token<'a> = ListToken<'a, T> where Self: 'a;
    type Node = Node<T>;

    fn pop<R>(&self, on_pop: impl Fn(&T) -> R) -> Option<R> {
        let mut head = self.head.load(Ordering::SeqCst);
        if head.is_null() {
            //it's empty!
            return None;
        }
        while let Err(new_head) = self.head.compare_exchange_weak(
            head & !LOCKED,
            head | LOCKED,
            Ordering::Acquire,
            Ordering::Relaxed,
        ) {
            head = new_head;
            if head.is_null() {
                //it's empty!
                return None;
            }
        }
        let _guard = LockGuard { inner: &self.head };
        if head.is_null() {
            //it's empty!
            return None;
        }
        debug_assert!(!head.is_null());
        unsafe {
            let tail = self
                .tail
                .get()
                .expect("there is no tail but there is a head!")
                .as_ref();
            debug_assert!(tail.is_on_queue.load(Ordering::Relaxed));
            tail.remove(self, false);
            let r_val = Some(on_pop(&tail.data));
            tail.is_on_queue.store(false, Ordering::Relaxed);
            r_val
        }
    }

    fn build_node(data: T) -> Self::Node {
        Node::new(data)
    }

    fn build_token<'a>(&'a self, node: impl Into<MaybeRef<'a, Self::Node>>) -> Self::Token<'a> {
        ListToken {
            queue: self,
            node: node.into(),
            _unpin: core::marker::PhantomPinned,
        }
    }
}

impl<T> IntrusiveLinkedList<T>
where
    T: Clone,
{
    pub fn to_vec(&self) -> Vec<T> {
        let mut data = Vec::new();
        let (_guard, head) = self.lock();
        if head.is_null() {
            return data;
        }
        let mut current = head.as_non_null::<T>();
        while let Some(current_non_null) = current {
            let current_ref = unsafe { current_non_null.as_ref() };
            data.push(current_ref.data.clone());
            current = current_ref.next.get()
        }
        data
    }
}

impl<T> Debug for IntrusiveLinkedList<T>
where
    T: Debug,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let (_guard, head) = self.lock();
        if head.is_null() {
            return f.debug_struct("IntrusiveLinkedList").finish();
        }
        let mut output = String::from("IntrusiveLinkedList [");
        let mut current = head.as_non_null::<T>();
        let mut next_str = String::from("\n");
        while let Some(current_non_null) = current {
            let current_ref = unsafe { current_non_null.as_ref() };
            if let Some(prev) = current_ref.prev.get() {
                next_str.push_str(format!("\t↑ {:?}\n", prev.as_ptr()).as_str());
            } else {
                next_str.push_str(format!("\t↑ {:?}\n", core::ptr::null::<()>()).as_str());
            }
            output.push_str(
                format!(
                    "{}{:?} → {:?},",
                    next_str, current_ref as *const _, current_ref
                )
                .as_str(),
            );
            if let Some(next) = current_ref.next.get() {
                next_str = format!("\n\t↓ {:?}", next.as_ptr())
            } else {
                next_str = format!("\n\t↓ {:?}", core::ptr::null::<()>())
            }
            current = current_ref.next.get()
        }
        output += next_str.as_str();
        output.push_str("\n]");
        f.write_str(output.as_str())
    }
}

#[repr(align(8))]
pub struct Node<T> {
    next: Cell<Option<NonNull<Node<T>>>>,
    prev: Cell<Option<NonNull<Node<T>>>>,
    is_on_queue: AtomicBool,
    data: T,
}

impl<T> super::Node<T> for Node<T> {}

impl<T> Debug for Node<T>
where
    T: Debug,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Node")
            .field("data", &self.data)
            .field("is_on_queue", &self.is_on_queue)
            .finish()
    }
}

impl<T> Node<T> {
    pub const fn new(data: T) -> Self {
        Self {
            next: Cell::new(None),
            prev: Cell::new(None),
            is_on_queue: AtomicBool::new(false),
            data,
        }
    }

    fn as_non_null(&self) -> Option<NonNull<Self>> {
        unsafe { Some(NonNull::new_unchecked(self as *const _ as *mut _)) }
    }

    pub fn push(&self, queue: &IntrusiveLinkedList<T>) {
        self.prev.set(None);
        self.is_on_queue.store(true, Ordering::Release);
        let self_non_null = self.as_non_null();
        let self_non_null_usize = self_non_null.as_usize();
        let mut head = queue.head.load(Ordering::Acquire);
        loop {
            self.next.set(head.as_non_null());
            if let Err(new_head) = queue.head.compare_exchange_weak(
                head & !LOCKED,
                self_non_null_usize | LOCKED,
                Ordering::SeqCst,
                Ordering::Relaxed,
            ) {
                head = new_head;
            } else {
                break;
            }
        }
        if head.is_null() {
            queue.tail.set(self_non_null);
        } else {
            let head_ref = unsafe {
                head.as_non_null::<T>()
                    .expect("head was null on push")
                    .as_ref()
            };
            head_ref.prev.set(self_non_null);
        }

        // the next line unlocks the queue!
        let old = queue.head.swap(self_non_null_usize, Ordering::Release);
        debug_assert_eq!(old, self_non_null_usize | LOCKED);
    }

    pub fn revoke(&self, queue: &IntrusiveLinkedList<T>) -> bool {
        if !self.is_on_queue.load(Ordering::Acquire) {
            return false;
        }
        let mut head = queue.head.load(Ordering::Acquire);
        while let Err(new_head) = queue.head.compare_exchange_weak(
            head & !LOCKED,
            head | LOCKED,
            Ordering::Acquire,
            Ordering::Relaxed,
        ) {
            core::hint::spin_loop();
            head = new_head;
            if head.is_null() {
                //TODO are there more optimisations here? check is on queue every time?
                return false;
            }
        }
        if !self.is_on_queue.swap(false, Ordering::Relaxed) {
            queue.head.fetch_and(!LOCKED, Ordering::Release);
            return false;
        }
        if !self.remove(queue, true) {
            queue.head.fetch_and(!LOCKED, Ordering::Release);
        }
        true
    }
    fn remove(&self, queue: &IntrusiveLinkedList<T>, allow_unlock: bool) -> bool {
        let mut did_unlock = false;
        if self.prev.get().is_none() {
            // This is the head
            let next = self.next.get();
            if let Some(next) = next {
                let next_ref = unsafe { next.as_ref() };
                next_ref.prev.set(None);
            } else {
                // and the tail!
                debug_assert!(queue
                    .tail
                    .get()
                    .is_some_and(|nn| nn.as_ptr() == self as *const _ as *mut _));
                queue.tail.set(None);
            }
            let next = next.as_usize();
            if allow_unlock {
                // this unlocks the queue!
                debug_assert!(!next.is_locked());
                queue.head.store(next, Ordering::Release);
                did_unlock = true;
            } else {
                queue.head.store(next | LOCKED, Ordering::Relaxed);
            }
        } else if self.next.get().is_none() {
            // this is the tail!
            debug_assert!(queue
                .tail
                .get()
                .is_some_and(|nn| nn.as_ptr() == self as *const _ as *mut _));
            let new_tail = self.prev.get();
            if let Some(new_tail) = new_tail {
                unsafe { new_tail.as_ref().next.set(None) }
            }
            queue.tail.set(new_tail);
        } else {
            let prev = self.prev.get();
            let prev_ref = unsafe { prev.expect("prev was nul!").as_ref() };
            let next = self.next.get();
            let next_ref = unsafe { next.expect("next was nul!").as_ref() };

            next_ref.prev.set(prev);
            prev_ref.next.set(next);
        }
        self.next.set(None);
        self.prev.set(None);
        did_unlock
    }
}

pub struct ListToken<'a, T> {
    queue: &'a IntrusiveLinkedList<T>,
    node: MaybeRef<'a, Node<T>>,
    _unpin: core::marker::PhantomPinned,
}

impl<T> Drop for ListToken<'_, T> {
    fn drop(&mut self) {
        self.revoke();
    }
}

impl<T> IntrusiveToken<T> for ListToken<'_, T> {
    fn push(&self) {
        self.node.push(self.queue);
    }

    fn revoke(&self) -> bool {
        self.node.revoke(self.queue)
    }

    fn inner(&self) -> &T {
        &self.node.data
    }

    fn is_on_queue(&self) -> bool {
        self.node.is_on_queue.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intrusive_list::{IntrusiveList, IntrusiveListCloneExt};
    use itertools::Itertools;
    use std::collections::HashMap;
    use std::thread;

    #[test]
    fn it_works() {
        let ill = IntrusiveLinkedList::new();
        let node_a = Node::new(42);
        let node_b = Node::new(32);
        node_a.push(&ill);
        node_b.push(&ill);
        debug_assert_eq!(ill.pop_clone(), Some(42));
        debug_assert_eq!(ill.pop_clone(), Some(32));
    }

    #[test]
    fn basic_push() {
        let queue = IntrusiveLinkedList::new();
        let node_a = queue.build_token(Node::new(32));
        node_a.push();
        let node_b = queue.build_token(Node::new(42));
        node_b.push();
        let node_c = queue.build_token(Node::new(21));
        node_c.push();
        let elements = queue.to_vec();
        assert_eq!(elements, vec![21, 42, 32]);
        println!("queue: {:?}", queue);
    }

    #[test]
    fn revoke_head() {
        let queue = IntrusiveLinkedList::new();
        let node_a = queue.build_token(Node::new(32));
        node_a.push();
        let node_b = queue.build_token(Node::new(42));
        node_b.push();
        let node_c = queue.build_token(Node::new(21));
        node_c.push();
        let elements = queue.to_vec();
        assert_eq!(elements, vec![21, 42, 32]);
        node_c.revoke();
        let elements = queue.to_vec();
        assert_eq!(elements, vec![42, 32]);

        assert!(node_a.node.prev.get().is_some());
        assert!(node_a.node.next.get().is_none());

        assert!(node_b.node.prev.get().is_none());
        assert!(node_b.node.next.get().is_some());

        assert!(node_c.node.next.get().is_none());
        assert!(node_c.node.prev.get().is_none());
    }

    #[test]
    fn revoke_tail() {
        let queue = IntrusiveLinkedList::new();
        let node_a = queue.build_token(Node::new(32));
        node_a.push();
        let node_b = queue.build_token(Node::new(42));
        node_b.push();
        let node_c = queue.build_token(Node::new(21));
        node_c.push();
        let elements = queue.to_vec();
        assert_eq!(elements, vec![21, 42, 32]);
        node_a.revoke();
        let elements = queue.to_vec();
        assert_eq!(elements, vec![21, 42]);

        assert!(node_a.node.prev.get().is_none());
        assert!(node_a.node.next.get().is_none());

        assert!(node_b.node.prev.get().is_some());
        assert!(node_b.node.next.get().is_none());

        assert!(node_c.node.next.get().is_some());
        assert!(node_c.node.prev.get().is_none());
    }

    #[test]
    fn revoke_middle() {
        let queue = IntrusiveLinkedList::new();
        let node_a = queue.build_token(Node::new(32));
        node_a.push();
        let node_b = queue.build_token(Node::new(42));
        node_b.push();
        let node_c = queue.build_token(Node::new(21));
        node_c.push();
        let elements = queue.to_vec();

        assert_eq!(elements, vec![21, 42, 32]);
        node_b.revoke();
        let elements = queue.to_vec();
        assert_eq!(elements, vec![21, 32]);

        assert!(node_a.node.prev.get().is_some());
        assert!(node_a.node.next.get().is_none());

        assert!(node_b.node.prev.get().is_none());
        assert!(node_b.node.next.get().is_none());

        assert!(node_c.node.next.get().is_some());
        assert!(node_c.node.prev.get().is_none());
    }

    #[test]
    fn drop_test() {
        let queue = IntrusiveLinkedList::new();
        let node_a = queue.build_token(Box::new(Node::new(32)));
        node_a.push();
        let node_b = queue.build_token(Box::new(Node::new(42)));
        node_b.push();
        let node_c = queue.build_token(Box::new(Node::new(21)));
        node_c.push();

        assert_eq!(queue.to_vec(), vec![21, 42, 32]);
        drop(node_b);
        assert_eq!(queue.to_vec(), vec![21, 32]);
    }

    #[test]
    fn pop() {
        let queue = IntrusiveLinkedList::new();
        let node_a = queue.build_token(Node::new(32));
        node_a.push();
        let node_b = queue.build_token(Node::new(42));
        node_b.push();
        let node_c = queue.build_token(Node::new(21));
        node_c.push();
        assert_eq!(queue.to_vec(), vec![21, 42, 32]);

        assert!(queue.pop(|v| assert_eq!(*v, 32)).is_some());
        assert_eq!(queue.to_vec(), vec![21, 42]);
        assert!(queue.pop(|v| assert_eq!(*v, 42)).is_some());
        assert_eq!(queue.to_vec(), vec![21]);
        assert!(queue.pop(|v| assert_eq!(*v, 21)).is_some());
        assert_eq!(queue.to_vec(), vec![]);
    }

    fn do_pipe_test(num_elements: usize, num_senders: usize, num_receivers: usize) {
        let elements_per_receiver =
            (num_senders as f64 * num_elements as f64) / num_receivers as f64;
        let elements_per_receiver = elements_per_receiver.floor() as usize;

        let barrier = std::sync::Barrier::new(num_senders + num_receivers);
        let queue = IntrusiveLinkedList::new();
        thread::scope(|s| {
            let receiver_handles = (0..num_receivers)
                .map(|_| {
                    s.spawn(|| {
                        let mut results = Vec::with_capacity(elements_per_receiver);
                        barrier.wait();
                        for _ in 0..elements_per_receiver {
                            loop {
                                if let Some(v) = queue.pop_clone() {
                                    results.push(v);
                                    break;
                                }
                            }
                        }
                        barrier.wait();
                        results
                    })
                })
                .collect_vec();

            for _ in 0..num_senders {
                s.spawn(|| {
                    let mut tokens = Vec::with_capacity(num_elements);
                    barrier.wait();
                    for i in 0..num_elements {
                        let token = Box::new(queue.build_token(Node::new(i)));
                        token.push();
                        tokens.push(token);
                    }
                    barrier.wait();
                });
            }

            let mut value_count = HashMap::with_capacity(num_elements);
            for handle in receiver_handles {
                let results = handle.join().expect("couldn't join thread");
                for result in results {
                    *value_count.entry(result).or_insert(0) += 1;
                }
            }

            assert_eq!(value_count.len(), num_elements);
            for (_, count) in value_count {
                assert_eq!(count, num_senders);
            }
        });
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn pipe_test() {
        let num_elements = 50000;
        let num_senders = 4;
        let num_receivers = 4;
        do_pipe_test(num_elements, num_senders, num_receivers);
    }

    #[test]
    fn pipe_test_miri() {
        let num_elements = 50;
        let num_senders = 2;
        let num_receivers = 2;
        do_pipe_test(num_elements, num_senders, num_receivers);
    }
}
