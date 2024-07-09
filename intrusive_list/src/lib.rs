#![allow(dead_code)]
// #![warn(missing_docs)]
// #![warn(missing_docs_in_private_items)]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::module_name_repetitions)]
#![warn(clippy::undocumented_unsafe_blocks)]

use core::cell::Cell;
use core::fmt::{Debug, Formatter};
use core::pin::Pin;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};

// Alias MiniLock to allow for testing with other lock implementations
use mini_lock::{MiniLock as InnerLock, MiniLockGuard};

/// Outer container for intrusive linked list. Proxies calls to the inner list
/// through the inner lock
#[derive(Default)]
pub struct ConcurrentIntrusiveLinkedList<T> {
    inner: InnerLock<IntrusiveLinkedList<T>>,
}

impl<T> Debug for ConcurrentIntrusiveLinkedList<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let guard = self.inner.lock();
        f.debug_struct("IntrusiveLinkedList")
            .field("inner", &*guard)
            .finish()
    }
}

impl<NODE> ConcurrentIntrusiveLinkedList<NODE> {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            inner: InnerLock::new(IntrusiveLinkedList::new()),
        }
    }
}

impl<T> ConcurrentIntrusiveLinkedList<T> {
    /// Pops the tail from the list if the condition is true
    ///
    /// This function proxies to the inner list via a spin lock making it safe to call from
    /// multiple threads.
    ///
    /// Refer to [`IntrusiveLinkedList::pop_if`] for more information.
    pub fn pop_if<R>(
        &self,
        condition: impl FnMut(&mut T, usize) -> Option<R>,
        on_empty: impl FnMut(),
    ) -> Option<R> {
        let mut inner = self.inner.lock();
        inner.pop_if(condition, on_empty)
    }

    /// Create a new node object.
    ///
    /// Node that this does not push the node onto the list.
    #[must_use]
    pub const fn new_node_data(data: T) -> Node<T> {
        Node::new(data)
    }

    /// Build a token that references this list
    pub const fn build_token<'a>(&'a self, node: Node<T>) -> ListToken<'a, T>
    where
        T: 'a + Send,
    {
        ListToken {
            queue: self,
            node,
            _unpin: core::marker::PhantomPinned,
        }
    }

    pub fn len(&self) -> usize {
        self.inner.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

struct IntrusiveLinkedList<T> {
    head: *const Node<T>,
    tail: *const Node<T>,
    length: usize,
}

// SAFETY: raw pointers do not implement Send or Sync but are safe to Send
unsafe impl<T> Send for IntrusiveLinkedList<T> {}

impl<T> Default for IntrusiveLinkedList<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<NODE> IntrusiveLinkedList<NODE> {
    const fn new() -> Self {
        Self {
            head: core::ptr::null(),
            tail: core::ptr::null(),
            length: 0,
        }
    }
}

impl<T> IntrusiveLinkedList<T> {
    /// Pops the tail from the list if the condition is true
    /// note that this function does return the value that was popped.
    /// It does however return a value from the condition check.
    /// This condition check is where the popped value can be used.
    /// If the popped value needs to be recovered from the list it should be cloned and
    /// returned from the condition check.
    fn pop_if<R>(
        &mut self,
        mut condition: impl FnMut(&mut T, usize) -> Option<R>,
        mut on_empty: impl FnMut(),
    ) -> Option<R> {
        if self.head.is_null() {
            //it's empty!
            debug_assert_eq!(self.length, 0);
            on_empty();
            return None;
        }
        unsafe {
            let tail = &*self.tail;
            debug_assert!(tail.is_on_queue.load(Ordering::Relaxed));
            let ret;
            {
                let length = self.length;
                let tail = tail.as_mut(self);
                ret = condition(&mut tail.data, length);
            }
            if ret.is_none() {
                return None;
            }
            tail.remove(self);
            debug_assert!(!tail.is_on_queue.load(Ordering::Relaxed));
            ret
        }
    }

    const fn len(&self) -> usize {
        self.length
    }
}

#[allow(clippy::missing_fields_in_debug)]
impl<T> Debug for IntrusiveLinkedList<T> {
    /// An unconventional debug implementation but it's much more useful for debugging
    /// than simply printing the pointer values
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        if self.head.is_null() {
            return f.debug_struct("IntrusiveLinkedList").finish();
        }
        let mut output = String::from("IntrusiveLinkedList [");
        let mut current = self.head;
        let mut next_str = String::from("\n");
        while !current.is_null() {
            // Safety: we've checked for current not null
            let current_ref = unsafe { &*current };
            next_str.push_str(format!("\t↑ {:?}\n", current_ref.prev.get()).as_str());
            output.push_str(
                format!(
                    "{}{:?} → {:?},",
                    next_str, current_ref as *const _, current_ref
                )
                .as_str(),
            );
            next_str = format!("\n\t↓ {:?}", current_ref.next.get());
            current = current_ref.next.get();
        }
        output += next_str.as_str();
        output.push_str("\n]");
        f.write_str(output.as_str())
    }
}

pub trait HasNode<S> {
    fn get_node(&self) -> &Node<S>;
}

/// An intrusive linked list node.
/// This node should be embedded within another struct and pinned before it is pushed to the list
/// If it moves while on the list it will invalidate the list!
pub struct Node<T> {
    next: Cell<*const Self>,
    prev: Cell<*const Self>,
    is_on_queue: AtomicBool,
    data: T,
}

//SAFETY: This is safe as long as T is Send. Raw pointers are safe to send just not de-reference
unsafe impl<T> Send for Node<T> where T: Send {}

#[allow(clippy::missing_fields_in_debug)]
impl<T> Debug for Node<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Node")
            .field("next", &self.next.get())
            .field("prev", &self.prev.get())
            .field("is_on_queue", &self.is_on_queue.load(Ordering::Relaxed))
            .finish()
    }
}

impl<T> Node<T> {
    #[must_use]
    pub const fn new(data: T) -> Self {
        Self {
            next: Cell::new(core::ptr::null()),
            prev: Cell::new(core::ptr::null()),
            is_on_queue: AtomicBool::new(false),
            data,
        }
    }

    fn as_mut<'l, 's>(&self, _guard: &'l mut IntrusiveLinkedList<T>) -> &'s mut Self
    where
        'l: 's,
    {
        unsafe { ptr::from_ref(self).cast_mut().as_mut().unwrap() }
    }
}
impl<T> Node<T> {
    /// helper method to unconditionally push the node to the front of the queue
    fn push(&self, queue: &mut IntrusiveLinkedList<T>) {
        self.push_if(queue, |_| true);
    }

    fn push_if(
        &self,
        queue: &mut IntrusiveLinkedList<T>,
        mut condition: impl FnMut(usize) -> bool,
    ) -> bool {
        if self.is_on_queue.load(Ordering::Acquire) {
            //already on the queue
            return true;
        }
        let inner_node = self;
        if !condition(queue.length) {
            debug_assert!(!inner_node.is_on_queue.load(Ordering::Relaxed));
            return false;
        }
        // At this point we are sure that we are going to push to the queue.
        inner_node.is_on_queue.store(true, Ordering::Release);

        inner_node.prev.set(core::ptr::null());
        inner_node.next.set(queue.head);
        if queue.head.is_null() {
            // This will be the only item in the list and is therefore also the tail
            queue.tail = self;
        } else {
            let head_ref = unsafe { &*queue.head };
            head_ref.prev.set(self);
        }
        queue.head = self;
        queue.length += 1;
        true
    }

    /// If the node is on the queue it will be removed and true will be returned.
    /// If the node wasn't already on the queue false will be returned.
    fn remove(&self, queue: &mut IntrusiveLinkedList<T>) -> bool {
        if !self.is_on_queue.swap(false, Ordering::AcqRel) {
            return false;
        }
        let inner_node = self;
        queue.length -= 1;

        if inner_node.prev.get().is_null() {
            // This is the head
            let next = inner_node.next.get();
            if let Some(next) = unsafe { next.as_ref() } {
                next.prev.set(core::ptr::null());
            } else {
                // This is also the tail (only item in the list)
                debug_assert_eq!(queue.tail, self as *const _);
                queue.tail = core::ptr::null();
            }
            queue.head = next;
        } else if inner_node.next.get().is_null() {
            // This is the tail
            debug_assert_eq!(queue.tail, self as *const _);
            let prev = unsafe { &*inner_node.prev.get() };
            prev.next.set(core::ptr::null());
            queue.tail = prev;
        } else {
            // This is in the middle somewhere
            let prev = unsafe { &*inner_node.prev.get() };
            let next = unsafe { &*inner_node.next.get() };
            next.prev.set(prev);
            prev.next.set(next);
        }

        inner_node.next.set(core::ptr::null());
        inner_node.prev.set(core::ptr::null());
        true
    }
}

/// A `ListToken` represents an interest in an entry in the list.
/// If the list token is dropped the entry will be removed from the list.
///
/// Once the token has been pushed to the list it cannot be moved again. Even if it's revoked.
#[derive(Debug)]
pub struct ListToken<'a, T>
where
    T: Send,
{
    queue: &'a ConcurrentIntrusiveLinkedList<T>,
    node: Node<T>,
    _unpin: core::marker::PhantomPinned,
}

impl<'a, T> Drop for ListToken<'a, T>
where
    T: Send,
{
    fn drop(&mut self) {
        // it's possible that the node isn't actually on the queue even if is_on_queue is true
        // it's never possible that it's on the queue if is_on_queue is false
        self.revoke();
    }
}

impl<'a, T> ListToken<'a, T>
where
    T: Send,
{
    /// Push the node onto the front of the queue.
    pub fn push(self: Pin<&Self>) {
        self.push_if(|_| true);
    }

    /// Conditionally push the node onto the front of the queue.
    pub fn push_if(self: Pin<&Self>, condition: impl FnMut(usize) -> bool) -> bool {
        let mut queue = self.queue.inner.lock();
        self.node.push_if(&mut queue, condition)
    }

    /// Revoke the node from the queue.
    ///
    /// Returns true if the node was on the queue and was revoked.
    /// Returns false if the node was not on the queue.
    pub fn revoke(&self) -> bool {
        // Don't take out the lock if it is already off the queue
        if !self.node.is_on_queue.load(Ordering::Acquire) {
            return false;
        }
        let mut queue = self.queue.inner.lock();
        self.node.remove(&mut queue)
    }

    /// Access the inner value of the token.
    pub const fn inner(&self) -> &T {
        &self.node.data
    }

    // TODO proper error type here
    pub fn fast_modify_payload<R>(&mut self, f: impl Fn(&mut T) -> R) -> Result<R, ()> {
        if self.node.is_on_queue.load(Ordering::Acquire) {
            return Err(());
        }
        Ok(f(&mut self.node.data))
    }

    pub fn modify_payload<R>(&self, f: impl Fn(&mut T, bool) -> R) -> R {
        // while we have the guard the node cannot be popped and used, and therefore we can modify its
        // contents safely
        let mut guard = self.queue.inner.lock();
        let mut_self = self.get_mut(&mut guard);
        f(
            &mut mut_self.node.data,
            self.node.is_on_queue.load(Ordering::Acquire),
        )
    }

    pub fn is_on_queue(&self) -> bool {
        self.node.is_on_queue.load(Ordering::Acquire)
    }

    fn get_mut<'g, 's>(&self, _guard: &'g mut MiniLockGuard<IntrusiveLinkedList<T>>) -> &'s mut Self
    where
        'g: 'a,
    {
        // Safety: This is safe because we have a mutble reference to an exclusive lock on the list
        unsafe { ptr::from_ref(self).cast_mut().as_mut().unwrap() }
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

    impl<T> IntrusiveLinkedList<T>
    where
        T: Clone,
    {
        /// helper function that clones the items from the list into a vector.
        /// Items are cloned from the head first.
        fn clone_to_vec(&self) -> Vec<T> {
            let mut data = Vec::new();
            if self.head.is_null() {
                return data;
            }
            let mut current = self.head;
            while !current.is_null() {
                let current_ref = unsafe { &*current };
                data.push(current_ref.data.clone());
                current = current_ref.next.get();
            }
            data
        }
    }

    impl<T> ConcurrentIntrusiveLinkedList<T>
    where
        T: Clone,
    {
        pub fn clone_to_vec(&self) -> Vec<T> {
            self.inner.lock().clone_to_vec()
        }

        /// helper function that pops the tail and clones it
        fn pop_clone(&self) -> Option<T> {
            self.inner.lock().pop_if(|v, _| Some(v.clone()), || {})
        }
    }

    #[test]
    fn it_works() {
        let ill = ConcurrentIntrusiveLinkedList::new();
        let node_a = Node::new(42);
        let node_b = Node::new(32);
        {
            let mut ill_lock = ill.inner.lock();
            node_a.push(&mut ill_lock);
            node_b.push(&mut ill_lock);
        }
        debug_assert_eq!(ill.pop_clone(), Some(42));
        debug_assert_eq!(ill.pop_clone(), Some(32));
    }

    #[test]
    fn basic_push() {
        let queue = ConcurrentIntrusiveLinkedList::new();
        let node_a = pin!(queue.build_token(Node::new(32)));
        node_a.as_ref().push();
        let node_b = pin!(queue.build_token(Node::new(42)));
        node_b.as_ref().push();
        let node_c = pin!(queue.build_token(Node::new(21)));
        node_c.as_ref().push();
        let elements = queue.clone_to_vec();
        assert_eq!(elements, vec![21, 42, 32]);
        println!("queue: {queue:?}");
    }

    #[test]
    fn revoke_head() {
        let queue = ConcurrentIntrusiveLinkedList::new();
        let node_a = pin!(queue.build_token(Node::new(32)));
        node_a.as_ref().push();
        let node_b = pin!(queue.build_token(Node::new(42)));
        node_b.as_ref().push();
        let node_c = pin!(queue.build_token(Node::new(21)));
        node_c.as_ref().push();
        let elements = queue.clone_to_vec();
        assert_eq!(elements, vec![21, 42, 32]);
        node_c.revoke();
        let elements = queue.clone_to_vec();
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
        let queue = ConcurrentIntrusiveLinkedList::new();
        let node_a = pin!(queue.build_token(Node::new(32)));
        node_a.as_ref().push();
        let node_b = pin!(queue.build_token(Node::new(42)));
        node_b.as_ref().push();
        let node_c = pin!(queue.build_token(Node::new(21)));
        node_c.as_ref().push();
        let elements = queue.clone_to_vec();
        assert_eq!(elements, vec![21, 42, 32]);
        node_a.revoke();
        let elements = queue.clone_to_vec();
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
        let queue = ConcurrentIntrusiveLinkedList::new();
        let node_a = pin!(queue.build_token(Node::new(32)));
        node_a.as_ref().push();
        let node_b = pin!(queue.build_token(Node::new(42)));
        node_b.as_ref().push();
        let node_c = pin!(queue.build_token(Node::new(21)));
        node_c.as_ref().push();
        let elements = queue.clone_to_vec();

        assert_eq!(elements, vec![21, 42, 32]);
        node_b.revoke();
        let elements = queue.clone_to_vec();
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
        let queue = ConcurrentIntrusiveLinkedList::new();
        let node_a = pin!(queue.build_token(Node::new(32)));
        node_a.as_ref().push();
        let node_c = pin!(queue.build_token(Node::new(21)));
        {
            let node_b = pin!(queue.build_token(Node::new(42)));
            node_b.as_ref().push();
            node_c.as_ref().push();

            assert_eq!(queue.clone_to_vec(), vec![21, 42, 32]);
        }
        assert_eq!(queue.clone_to_vec(), vec![21, 32]);
    }

    #[test]
    fn pop() {
        let queue = ConcurrentIntrusiveLinkedList::new();
        let node_a = pin!(queue.build_token(Node::new(32)));
        node_a.as_ref().push();
        let node_b = pin!(queue.build_token(Node::new(42)));
        node_b.as_ref().push();
        let node_c = pin!(queue.build_token(Node::new(21)));
        node_c.as_ref().push();
        assert_eq!(queue.clone_to_vec(), vec![21, 42, 32]);

        queue.pop_if(
            |v, len| {
                assert_eq!(*v, 32);
                assert_eq!(len, 3);
                Some(())
            },
            || panic!("shouldn't fail"),
        );
        assert_eq!(queue.clone_to_vec(), vec![21, 42]);
        queue.pop_if(
            |v, len| {
                assert_eq!(*v, 42);
                assert_eq!(len, 2);
                Some(())
            },
            || panic!("shouldn't fail"),
        );
        assert_eq!(queue.clone_to_vec(), vec![21]);
        queue.pop_if(
            |v, len| {
                assert_eq!(*v, 21);
                assert_eq!(len, 1);
                Some(())
            },
            || panic!("shouldn't fail"),
        );
        assert_eq!(queue.clone_to_vec(), vec![]);
        let mut did_fail = false;
        queue.pop_if(
            |_v, _len| -> Option<()> { panic!("shouldn't pop") },
            || {
                did_fail = true;
            },
        );
        assert!(did_fail);
    }

    fn do_pipe_test(num_elements: usize, num_senders: usize, num_receivers: usize) {
        // std::ops::Div; does floor by default
        let elements_per_receiver = (num_senders * num_elements).div(num_receivers);

        let barrier = std::sync::Barrier::new(num_senders + num_receivers);
        let queue = ConcurrentIntrusiveLinkedList::new();
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
                        let token = Box::pin(queue.build_token(Node::new(i)));
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
        let num_elements = 50;
        let num_senders = 2;
        let num_receivers = 2;
        do_pipe_test(num_elements, num_senders, num_receivers);
    }
}
