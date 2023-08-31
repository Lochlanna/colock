mod atomic_const_ptr;
mod spin_lock;

use crate::event::maybe_ref::MaybeRef;
use atomic_const_ptr::AtomicConstPtr;
use core::cell::Cell;
use core::fmt::{Debug, Formatter};
use core::pin::Pin;
use spin_lock::{SpinGuard, SpinLock};
use std::sync::atomic::{AtomicBool, Ordering};
struct ProtectedListData<T> {
    tail: *const Node<T>,
    length: usize,
}

impl<T> ProtectedListData<T> {
    const fn new() -> Self {
        Self {
            tail: core::ptr::null(),
            length: 0,
        }
    }
}

pub struct IntrusiveLinkedList<T> {
    head: AtomicConstPtr<Node<T>>,
    protected: SpinLock<ProtectedListData<T>>,
}

unsafe impl<T> Send for IntrusiveLinkedList<T> {}

impl<T> IntrusiveLinkedList<T> {
    pub const fn new() -> Self {
        Self {
            head: AtomicConstPtr::new(core::ptr::null()),
            protected: SpinLock::new(ProtectedListData::new()),
        }
    }

    fn back_fill(&self, guard: &mut SpinGuard<ProtectedListData<T>>) -> *const Node<T> {
        let head = self.head.load(Ordering::SeqCst);
        if head.is_null() {
            debug_assert_eq!(guard.length, 0);
            return head;
        }
        let mut current = unsafe { (*head).next.get() };
        let mut prev: *const Node<T> = head;
        let mut count = 0;
        while !current.is_null() {
            let current_ref = unsafe { &*current };
            if !current_ref.prev.get().is_null() {
                break;
            }
            count += 1;
            current_ref.prev.set(prev);
            prev = current;
            current = current_ref.next.get();
        }
        if guard.tail.is_null() {
            guard.tail = prev;
            guard.length = count + 1;
        } else {
            guard.length += count;
        }

        head
    }

    /// Pops the tail from the list if the condition is true
    ///
    /// This function proxies to the inner list via a spin lock making it safe to call from
    /// multiple threads.
    ///
    /// Refer to [`IntrusiveLinkedListInner::pop_if`] for more information.
    pub fn pop_if<R>(
        &self,
        condition: impl FnOnce(&T, usize) -> Option<R>,
        on_empty: impl FnOnce(),
    ) -> Option<R> {
        let mut guard = self.protected.lock();
        // println!("on pop: {}", self.internal_fmt(&mut guard));
        let head = self.back_fill(&mut guard);
        if head.is_null() {
            //it's empty!
            debug_assert_eq!(guard.length, 0);
            on_empty();
            return None;
        }
        unsafe {
            let tail = &*guard.tail;
            debug_assert!(tail.is_on_queue.load(Ordering::Relaxed));
            let ret = condition(&tail.data, guard.length);
            // println!("will pop {:?}", ret.is_some());
            ret.as_ref()?;
            tail.remove(self, &mut guard);
            debug_assert!(!tail.is_on_queue.load(Ordering::Relaxed));
            ret
        }
    }

    /// Create a new node object.
    ///
    /// Node that this does not push the node onto the list.
    pub const fn build_node(data: T) -> Node<T> {
        Node::new(data)
    }

    /// Build a token that references this list
    pub const fn build_token<'a>(&'a self, node: MaybeRef<'a, Node<T>>) -> ListToken<'a, T>
    where
        T: 'a,
    {
        ListToken {
            queue: self,
            node,
            _unpin: core::marker::PhantomPinned,
        }
    }

    pub fn len(&self) -> usize {
        let mut guard = self.protected.lock();
        self.back_fill(&mut guard);
        guard.length
    }
}

impl<T> IntrusiveLinkedList<T> {
    fn internal_fmt(&self, guard: &mut SpinGuard<ProtectedListData<T>>) -> String {
        let head = self.back_fill(guard);
        if head.is_null() {
            return "Intrusive Linked List []".into();
        }
        let mut output = String::from("IntrusiveLinkedList [");
        let mut current = head;
        let mut next_str = String::from("\n");
        while !current.is_null() {
            let current_ref = unsafe { &*current };
            next_str.push_str(format!("\t↑ {:?}\n", current_ref.prev.get()).as_str());
            output.push_str(format!("{}{:?},", next_str, current_ref as *const _).as_str());
            next_str = format!("\n\t↓ {:?}", current_ref.next.get());
            current = current_ref.next.get();
        }
        output += next_str.as_str();
        output.push_str("\n]");
        output
    }
}
impl<T> IntrusiveLinkedList<T>
where
    T: Clone,
{
    pub fn to_vec(&self) -> Vec<T> {
        let _guard = self.protected.lock();
        let mut data = Vec::new();
        let mut current = self.head.load(Ordering::Acquire);
        while !current.is_null() {
            let current_ref = unsafe { &*current };
            data.push(current_ref.data.clone());
            current = current_ref.next.get();
        }
        data
    }

    /// helper function that pops the tail and clones it
    fn pop_clone(&self) -> Option<T> {
        self.pop_if(|v, _| Some(v.clone()), || {})
    }
}

#[allow(clippy::missing_fields_in_debug)]
impl<T> Debug for IntrusiveLinkedList<T> {
    /// An unconventional debug implementation but it's much more useful for debugging
    /// than simply printing the pointer values
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let mut guard = self.protected.lock();
        f.write_str(self.internal_fmt(&mut guard).as_str())
    }
}

/// An intrusive linked list node.
/// This node should be embedded within another struct and pinned before it is pushed to the list
/// If it moves while on the list it will invalidate the list!
#[derive(Debug)]
pub struct Node<T> {
    next: Cell<*const Node<T>>,
    prev: Cell<*const Node<T>>,
    is_on_queue: AtomicBool,
    data: T,
}

unsafe impl<T> Send for Node<T> where T: Send {}

impl<T> Node<T> {
    pub const fn new(data: T) -> Self {
        Self {
            next: Cell::new(core::ptr::null()),
            prev: Cell::new(core::ptr::null()),
            is_on_queue: AtomicBool::new(false),
            data,
        }
    }

    /// helper method to unconditionally push the node to the front of the queue
    fn push(&self, queue: &IntrusiveLinkedList<T>) {
        self.push_if(queue, || true);
    }

    fn push_if(&self, queue: &IntrusiveLinkedList<T>, condition: impl Fn() -> bool) -> bool {
        self.is_on_queue.store(true, Ordering::Relaxed);

        self.prev.set(core::ptr::null());

        let mut current_head = queue.head.load(Ordering::Acquire);
        while condition() {
            self.next.set(current_head);
            if let Err(new_head) = queue.head.compare_exchange_weak(
                current_head,
                self,
                Ordering::SeqCst,
                Ordering::Relaxed,
            ) {
                current_head = new_head;
            } else {
                return true;
            }
        }

        self.is_on_queue.store(false, Ordering::Relaxed);
        false
    }

    /// If the node is on the queue it will be removed and true will be returned.
    /// If the node wasn't already on the queue false will be returned.
    fn revoke(&self, queue: &IntrusiveLinkedList<T>) -> bool {
        if !self.is_on_queue.load(Ordering::Acquire) {
            return false;
        }
        let mut guard = queue.protected.lock();
        if !self.is_on_queue.load(Ordering::Relaxed) {
            return false;
        }
        queue.back_fill(&mut guard);
        self.remove(queue, &mut guard);
        true
    }

    /// Removes the node from the queue.
    fn remove(&self, queue: &IntrusiveLinkedList<T>, guard: &mut SpinGuard<ProtectedListData<T>>) {
        guard.length -= 1;

        loop {
            if self.prev.get().is_null() {
                // This is the head
                let next = self.next.get();
                if queue
                    .head
                    .compare_exchange(self, next, Ordering::Release, Ordering::Relaxed)
                    .is_err()
                {
                    //we are no longer the head!
                    queue.back_fill(guard);
                    continue;
                }

                if let Some(next) = unsafe { next.as_ref() } {
                    next.prev.set(core::ptr::null());
                } else {
                    // This is also the tail (only item in the list)
                    debug_assert_eq!(guard.tail, self as *const _);
                    guard.tail = core::ptr::null();
                }
            } else if self.next.get().is_null() {
                // This is the tail
                debug_assert_eq!(guard.tail, self as *const _);
                debug_assert!(!self.prev.get().is_null());
                let prev = unsafe { &*self.prev.get() };
                prev.next.set(core::ptr::null());
                guard.tail = prev;
            } else {
                // This is in the middle somewhere
                let prev = unsafe { &*self.prev.get() };
                let next = unsafe { &*self.next.get() };
                next.prev.set(prev);
                prev.next.set(next);
            }

            self.next.set(core::ptr::null());
            self.prev.set(core::ptr::null());

            self.is_on_queue.store(false, Ordering::Relaxed);
            return;
        }
    }
}

/// A `ListToken` represents an interest in an entry in the list.
/// If the list token is dropped the entry will be removed from the list.
///
/// Once the token has been pushed to the list it cannot be moved again. Even if it's revoked.
#[derive(Debug)]
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

impl<T> ListToken<'_, T> {
    /// Push the node onto the front of the queue.
    pub fn push(self: Pin<&Self>) {
        self.push_if(|| true);
    }

    /// Conditionally push the node onto the front of the queue.
    pub fn push_if(self: Pin<&Self>, condition: impl Fn() -> bool) -> bool {
        self.node.push_if(self.queue, condition)
    }

    /// Revoke the node from the queue.
    ///
    /// Returns true if the node was on the queue and was revoked.
    /// Returns false if the node was not on the queue.
    pub fn revoke(&self) -> bool {
        self.node.revoke(self.queue)
    }

    /// Access the inner value of the node.
    pub fn inner(&self) -> &T {
        &self.node.data
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use itertools::Itertools;
    use std::collections::HashMap;
    use std::ops::Div;
    use std::pin::pin;
    use std::thread;

    trait PtrHelpers {
        fn not_null(&self) -> bool;
    }

    impl<T> PtrHelpers for *const T {
        fn not_null(&self) -> bool {
            !self.is_null()
        }
    }

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
        let node_a = pin!(queue.build_token(Node::new(32).into()));
        node_a.as_ref().push();
        let node_b = pin!(queue.build_token(Node::new(42).into()));
        node_b.as_ref().push();
        let node_c = pin!(queue.build_token(Node::new(21).into()));
        node_c.as_ref().push();
        let elements = queue.to_vec();
        assert_eq!(elements, vec![21, 42, 32]);
        println!("queue: {queue:?}");
    }

    #[test]
    fn revoke_head() {
        let queue = IntrusiveLinkedList::new();
        let node_a = pin!(queue.build_token(Node::new(32).into()));
        node_a.as_ref().push();
        let node_b = pin!(queue.build_token(Node::new(42).into()));
        node_b.as_ref().push();
        let node_c = pin!(queue.build_token(Node::new(21).into()));
        node_c.as_ref().push();
        let elements = queue.to_vec();
        assert_eq!(elements, vec![21, 42, 32]);
        node_c.revoke();
        let elements = queue.to_vec();
        assert_eq!(elements, vec![42, 32]);

        assert!(node_a.node.prev.get().not_null());
        assert!(node_a.node.next.get().is_null());

        assert!(node_b.node.prev.get().is_null());
        assert!(node_b.node.next.get().not_null());

        assert!(node_c.node.next.get().is_null());
        assert!(node_c.node.prev.get().is_null());
    }

    #[test]
    fn revoke_tail() {
        let queue = IntrusiveLinkedList::new();
        let node_a = pin!(queue.build_token(Node::new(32).into()));
        node_a.as_ref().push();
        let node_b = pin!(queue.build_token(Node::new(42).into()));
        node_b.as_ref().push();
        let node_c = pin!(queue.build_token(Node::new(21).into()));
        node_c.as_ref().push();
        let elements = queue.to_vec();
        assert_eq!(elements, vec![21, 42, 32]);
        node_a.revoke();
        let elements = queue.to_vec();
        assert_eq!(elements, vec![21, 42]);

        assert!(node_a.node.prev.get().is_null());
        assert!(node_a.node.next.get().is_null());

        assert!(node_b.node.prev.get().not_null());
        assert!(node_b.node.next.get().is_null());

        assert!(node_c.node.next.get().not_null());
        assert!(node_c.node.prev.get().is_null());
    }

    #[test]
    fn revoke_middle() {
        let queue = IntrusiveLinkedList::new();
        let node_a = pin!(queue.build_token(Node::new(32).into()));
        node_a.as_ref().push();
        let node_b = pin!(queue.build_token(Node::new(42).into()));
        node_b.as_ref().push();
        let node_c = pin!(queue.build_token(Node::new(21).into()));
        node_c.as_ref().push();
        let elements = queue.to_vec();

        assert_eq!(elements, vec![21, 42, 32]);
        node_b.revoke();
        let elements = queue.to_vec();
        assert_eq!(elements, vec![21, 32]);

        assert!(node_a.node.prev.get().not_null());
        assert!(node_a.node.next.get().is_null());

        assert!(node_b.node.prev.get().is_null());
        assert!(node_b.node.next.get().is_null());

        assert!(node_c.node.next.get().not_null());
        assert!(node_c.node.prev.get().is_null());
    }

    #[test]
    fn drop_test() {
        let queue = IntrusiveLinkedList::new();
        let node_a = pin!(queue.build_token(Box::new(Node::new(32)).into()));
        node_a.as_ref().push();
        let node_c = pin!(queue.build_token(Box::new(Node::new(21)).into()));
        {
            let node_b = pin!(queue.build_token(Box::new(Node::new(42)).into()));
            node_b.as_ref().push();
            node_c.as_ref().push();

            assert_eq!(queue.to_vec(), vec![21, 42, 32]);
        }
        assert_eq!(queue.to_vec(), vec![21, 32]);
    }

    #[test]
    fn pop() {
        let queue = IntrusiveLinkedList::new();
        let node_a = pin!(queue.build_token(Node::new(32).into()));
        node_a.as_ref().push();
        let node_b = pin!(queue.build_token(Node::new(42).into()));
        node_b.as_ref().push();
        let node_c = pin!(queue.build_token(Node::new(21).into()));
        node_c.as_ref().push();
        assert_eq!(queue.to_vec(), vec![21, 42, 32]);

        queue.pop_if(
            |v, len| {
                assert_eq!(*v, 32);
                assert_eq!(len, 3);
                Some(())
            },
            || panic!("shouldn't fail"),
        );
        assert_eq!(queue.to_vec(), vec![21, 42]);
        queue.pop_if(
            |v, len| {
                assert_eq!(*v, 42);
                assert_eq!(len, 2);
                Some(())
            },
            || panic!("shouldn't fail"),
        );
        assert_eq!(queue.to_vec(), vec![21]);
        queue.pop_if(
            |v, len| {
                assert_eq!(*v, 21);
                assert_eq!(len, 1);
                Some(())
            },
            || panic!("shouldn't fail"),
        );
        assert_eq!(queue.to_vec(), vec![]);
        let did_fail = Cell::new(false);
        queue.pop_if(
            |_v, _len| -> Option<()> { panic!("shouldn't pop") },
            || {
                did_fail.set(true);
            },
        );
        assert!(did_fail.get());
    }

    fn do_pipe_test(num_elements: usize, num_senders: usize, num_receivers: usize) {
        // std::ops::Div; does floor by default
        let elements_per_receiver = (num_senders * num_elements).div(num_receivers);

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
                        let token = Box::pin(queue.build_token(Node::new(i).into()));
                        token.as_ref().push();
                        tokens.push(token);
                    }
                    barrier.wait();
                    drop(tokens);
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
        let num_elements = 500;
        let num_senders = 4;
        let num_receivers = 4;
        do_pipe_test(num_elements, num_senders, num_receivers);
    }

    #[test]
    fn pipe_test_miri() {
        let num_elements = 100;
        let num_senders = 2;
        let num_receivers = 2;
        do_pipe_test(num_elements, num_senders, num_receivers);
    }
}
