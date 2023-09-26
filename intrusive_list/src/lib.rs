#![allow(dead_code)]
// #![warn(missing_docs)]
// #![warn(missing_docs_in_private_items)]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::module_name_repetitions)]
#![warn(clippy::undocumented_unsafe_blocks)]

use core::cell::Cell;
use core::fmt::{Debug, Formatter};
use core::pin::Pin;

// Alias MiniLock to allow for testing with other lock implementations
use mini_lock::MiniLock as InnerLock;

/// Outer container for intrusive linked list. Proxies calls to the inner list
/// through the inner lock
#[derive(Default)]
pub struct IntrusiveLinkedList<T> {
    inner: InnerLock<IntrusiveLinkedListInner<T>>,
}

impl<NODE> Debug for IntrusiveLinkedList<NODE>
where
    NODE: Debug + Node<NODE>,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let guard = self.inner.lock();
        f.debug_struct("IntrusiveLinkedList")
            .field("inner", &*guard)
            .finish()
    }
}

impl<NODE> IntrusiveLinkedList<NODE> {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            inner: InnerLock::new(IntrusiveLinkedListInner::new()),
        }
    }
}

impl<NODE> IntrusiveLinkedList<NODE>
where
    NODE: HasNode<NODE>,
{
    /// Pops the tail from the list if the condition is true
    ///
    /// This function proxies to the inner list via a spin lock making it safe to call from
    /// multiple threads.
    ///
    /// Refer to [`IntrusiveLinkedListInner::pop_if`] for more information.
    pub fn pop_if<R>(
        &self,
        condition: impl FnMut(&NODE, usize) -> Option<R>,
        on_empty: impl FnMut(),
    ) -> Option<R> {
        let mut inner = self.inner.lock();
        inner.pop_if(condition, on_empty)
    }

    /// Create a new node object.
    ///
    /// Node that this does not push the node onto the list.
    #[must_use]
    pub const fn new_node_data() -> NodeData<NODE> {
        NodeData::new()
    }

    /// Build a token that references this list
    pub const fn build_token<'a>(&'a self, node: NODE) -> ListToken<'a, NODE>
    where
        NODE: 'a,
    {
        ListToken {
            is_on_queue: Cell::new(false),
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

struct IntrusiveLinkedListInner<NODE> {
    head: *const NODE,
    tail: *const NODE,
    length: usize,
}

unsafe impl<NODE> Send for IntrusiveLinkedListInner<NODE> {}

impl<T> Default for IntrusiveLinkedListInner<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<NODE> IntrusiveLinkedListInner<NODE> {
    const fn new() -> Self {
        Self {
            head: core::ptr::null(),
            tail: core::ptr::null(),
            length: 0,
        }
    }
}

impl<NODE> IntrusiveLinkedListInner<NODE>
where
    NODE: Node<NODE>,
{
    /// Pops the tail from the list if the condition is true
    /// note that this function does return the value that was popped.
    /// It does however return a value from the condition check.
    /// This condition check is where the popped value can be used.
    /// If the popped value needs to be recovered from the list it should be cloned and
    /// returned from the condition check.
    fn pop_if<R>(
        &mut self,
        mut condition: impl FnMut(&NODE, usize) -> Option<R>,
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
            debug_assert!(tail.get_node().is_on_queue.get());
            let ret = condition(tail, self.length);
            ret.as_ref()?;
            tail.remove(self);
            debug_assert!(!tail.get_node().is_on_queue.get());
            ret
        }
    }

    const fn len(&self) -> usize {
        self.length
    }
}

#[allow(clippy::missing_fields_in_debug)]
impl<NODE> Debug for IntrusiveLinkedListInner<NODE>
where
    NODE: Debug + Node<NODE>,
{
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
            next_str.push_str(format!("\t↑ {:?}\n", current_ref.get_node().prev.get()).as_str());
            output.push_str(
                format!(
                    "{}{:?} → {:?},",
                    next_str, current_ref as *const _, current_ref
                )
                .as_str(),
            );
            next_str = format!("\n\t↓ {:?}", current_ref.get_node().next.get());
            current = current_ref.get_node().next.get();
        }
        output += next_str.as_str();
        output.push_str("\n]");
        f.write_str(output.as_str())
    }
}

pub trait HasNode<S> {
    fn get_node(&self) -> &NodeData<S>;
}

trait Node<S>: HasNode<S> {
    fn push(&self, queue: &mut IntrusiveLinkedListInner<S>);
    fn push_if(
        &self,
        queue: &mut IntrusiveLinkedListInner<S>,
        condition: impl FnMut(usize) -> bool,
    ) -> bool;
    fn revoke(&self, queue: &mut IntrusiveLinkedListInner<S>) -> bool;
    fn remove(&self, queue: &mut IntrusiveLinkedListInner<S>);
}

/// An intrusive linked list node.
/// This node should be embedded within another struct and pinned before it is pushed to the list
/// If it moves while on the list it will invalidate the list!
#[derive(Debug)]
pub struct NodeData<OUTER> {
    next: Cell<*const OUTER>,
    prev: Cell<*const OUTER>,
    is_on_queue: Cell<bool>,
}

unsafe impl<OUTER> Send for NodeData<OUTER> where OUTER: Send {}

impl<OUTER> NodeData<OUTER> {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            next: Cell::new(core::ptr::null()),
            prev: Cell::new(core::ptr::null()),
            is_on_queue: Cell::new(false),
        }
    }
}
impl<OUTER> Node<OUTER> for OUTER
where
    OUTER: HasNode<OUTER>,
{
    /// helper method to unconditionally push the node to the front of the queue
    fn push(&self, queue: &mut IntrusiveLinkedListInner<OUTER>) {
        self.push_if(queue, |_| true);
    }

    fn push_if(
        &self,
        queue: &mut IntrusiveLinkedListInner<OUTER>,
        mut condition: impl FnMut(usize) -> bool,
    ) -> bool {
        let inner_node = self.get_node();
        if !condition(queue.length) {
            debug_assert!(!inner_node.is_on_queue.get());
            return false;
        }

        inner_node.prev.set(core::ptr::null());
        inner_node.next.set(queue.head);
        if queue.head.is_null() {
            // This will be the only item in the list and is therefore also the tail
            queue.tail = self;
        } else {
            let head_ref = unsafe { &*queue.head };
            head_ref.get_node().prev.set(self);
        }
        queue.head = self;
        inner_node.is_on_queue.set(true);

        queue.length += 1;

        true
    }

    /// If the node is on the queue it will be removed and true will be returned.
    /// If the node wasn't already on the queue false will be returned.
    fn revoke(&self, queue: &mut IntrusiveLinkedListInner<OUTER>) -> bool {
        if !self.get_node().is_on_queue.replace(false) {
            return false;
        }
        self.remove(queue);
        true
    }

    /// Removes the node from the queue.
    fn remove(&self, queue: &mut IntrusiveLinkedListInner<OUTER>) {
        let inner_node = self.get_node();
        queue.length -= 1;

        if inner_node.prev.get().is_null() {
            // This is the head
            let next = inner_node.next.get();
            if let Some(next) = unsafe { next.as_ref() } {
                next.get_node().prev.set(core::ptr::null());
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
            prev.get_node().next.set(core::ptr::null());
            queue.tail = prev;
        } else {
            // This is in the middle somewhere
            let prev = unsafe { &*inner_node.prev.get() };
            let next = unsafe { &*inner_node.next.get() };
            next.get_node().prev.set(prev);
            prev.get_node().next.set(next);
        }

        inner_node.next.set(core::ptr::null());
        inner_node.prev.set(core::ptr::null());

        inner_node.is_on_queue.set(false);
    }
}

/// A `ListToken` represents an interest in an entry in the list.
/// If the list token is dropped the entry will be removed from the list.
///
/// Once the token has been pushed to the list it cannot be moved again. Even if it's revoked.
#[derive(Debug)]
pub struct ListToken<'a, NODE>
where
    NODE: HasNode<NODE>,
{
    is_on_queue: Cell<bool>,
    queue: &'a IntrusiveLinkedList<NODE>,
    node: NODE,
    _unpin: core::marker::PhantomPinned,
}

impl<'a, NODE> Drop for ListToken<'a, NODE>
where
    NODE: Node<NODE>,
{
    fn drop(&mut self) {
        // it's possible that the node isn't actually on the queue even if is_on_queue is true
        // it's never possible that it's on the queue if is_on_queue is false
        if self.is_on_queue.get() {
            self.revoke();
        }
    }
}

impl<'a, NODE> ListToken<'a, NODE>
where
    NODE: HasNode<NODE>,
{
    /// Push the node onto the front of the queue.
    pub fn push(self: Pin<&Self>) {
        self.push_if(|_| true);
    }

    /// Conditionally push the node onto the front of the queue.
    pub fn push_if(self: Pin<&Self>, condition: impl FnMut(usize) -> bool) -> bool {
        self.is_on_queue.set(true);
        let mut queue = self.queue.inner.lock();
        self.node.push_if(&mut queue, condition)
    }

    /// Revoke the node from the queue.
    ///
    /// Returns true if the node was on the queue and was revoked.
    /// Returns false if the node was not on the queue.
    pub fn revoke(&self) -> bool {
        self.is_on_queue.set(false);
        let mut queue = self.queue.inner.lock();
        self.node.revoke(&mut queue)
    }

    /// Access the inner value of the token.
    pub const fn inner(&self) -> &NODE {
        &self.node
    }

    /// # Safety
    /// this will cause the drop code to not attempt to revoke the node from the queue.
    /// If the node isn't actually off the queue this can result in a use after free
    pub unsafe fn set_off_queue(&self) {
        self.is_on_queue.set(false);
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

    #[derive(Debug)]
    struct TestNode<T> {
        node_data: NodeData<Self>,
        data: T,
    }
    impl<T> TestNode<T> {
        const fn new(data: T) -> Self {
            Self {
                node_data: NodeData::new(),
                data,
            }
        }

        const fn inner(&self) -> &T {
            &self.data
        }
    }

    impl<T> HasNode<Self> for TestNode<T> {
        fn get_node(&self) -> &NodeData<Self> {
            &self.node_data
        }
    }

    impl<T> IntrusiveLinkedListInner<TestNode<T>>
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
                current = current_ref.get_node().next.get();
            }
            data
        }
    }

    impl<T> IntrusiveLinkedList<TestNode<T>>
    where
        T: Clone,
    {
        pub fn clone_to_vec(&self) -> Vec<T> {
            self.inner.lock().clone_to_vec()
        }

        /// helper function that pops the tail and clones it
        fn pop_clone(&self) -> Option<T> {
            self.inner.lock().pop_if(|v, _| Some(v.data.clone()), || {})
        }
    }

    #[test]
    fn it_works() {
        let ill = IntrusiveLinkedList::new();
        let node_a = TestNode::new(42);
        let node_b = TestNode::new(32);
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
        let queue = IntrusiveLinkedList::new();
        let node_a = pin!(queue.build_token(TestNode::new(32)));
        node_a.as_ref().push();
        let node_b = pin!(queue.build_token(TestNode::new(42)));
        node_b.as_ref().push();
        let node_c = pin!(queue.build_token(TestNode::new(21)));
        node_c.as_ref().push();
        let elements = queue.clone_to_vec();
        assert_eq!(elements, vec![21, 42, 32]);
        println!("queue: {queue:?}");
    }

    #[test]
    fn revoke_head() {
        let queue = IntrusiveLinkedList::new();
        let node_a = pin!(queue.build_token(TestNode::new(32)));
        node_a.as_ref().push();
        let node_b = pin!(queue.build_token(TestNode::new(42)));
        node_b.as_ref().push();
        let node_c = pin!(queue.build_token(TestNode::new(21)));
        node_c.as_ref().push();
        let elements = queue.clone_to_vec();
        assert_eq!(elements, vec![21, 42, 32]);
        node_c.revoke();
        let elements = queue.clone_to_vec();
        assert_eq!(elements, vec![42, 32]);

        assert!(node_a.node.get_node().prev.get().not_null());
        assert!(node_a.node.get_node().next.get().is_null());

        assert!(node_b.node.get_node().prev.get().is_null());
        assert!(node_b.node.get_node().next.get().not_null());

        assert!(node_c.node.get_node().next.get().is_null());
        assert!(node_c.node.get_node().prev.get().is_null());
    }

    #[test]
    fn revoke_tail() {
        let queue = IntrusiveLinkedList::new();
        let node_a = pin!(queue.build_token(TestNode::new(32)));
        node_a.as_ref().push();
        let node_b = pin!(queue.build_token(TestNode::new(42)));
        node_b.as_ref().push();
        let node_c = pin!(queue.build_token(TestNode::new(21)));
        node_c.as_ref().push();
        let elements = queue.clone_to_vec();
        assert_eq!(elements, vec![21, 42, 32]);
        node_a.revoke();
        let elements = queue.clone_to_vec();
        assert_eq!(elements, vec![21, 42]);

        assert!(node_a.node.get_node().prev.get().is_null());
        assert!(node_a.node.get_node().next.get().is_null());

        assert!(node_b.node.get_node().prev.get().not_null());
        assert!(node_b.node.get_node().next.get().is_null());

        assert!(node_c.node.get_node().next.get().not_null());
        assert!(node_c.node.get_node().prev.get().is_null());
    }

    #[test]
    fn revoke_middle() {
        let queue = IntrusiveLinkedList::new();
        let node_a = pin!(queue.build_token(TestNode::new(32)));
        node_a.as_ref().push();
        let node_b = pin!(queue.build_token(TestNode::new(42)));
        node_b.as_ref().push();
        let node_c = pin!(queue.build_token(TestNode::new(21)));
        node_c.as_ref().push();
        let elements = queue.clone_to_vec();

        assert_eq!(elements, vec![21, 42, 32]);
        node_b.revoke();
        let elements = queue.clone_to_vec();
        assert_eq!(elements, vec![21, 32]);

        assert!(node_a.node.get_node().prev.get().not_null());
        assert!(node_a.node.get_node().next.get().is_null());

        assert!(node_b.node.get_node().prev.get().is_null());
        assert!(node_b.node.get_node().next.get().is_null());

        assert!(node_c.node.get_node().next.get().not_null());
        assert!(node_c.node.get_node().prev.get().is_null());
    }

    #[test]
    fn drop_test() {
        let queue = IntrusiveLinkedList::new();
        let node_a = pin!(queue.build_token(TestNode::new(32)));
        node_a.as_ref().push();
        let node_c = pin!(queue.build_token(TestNode::new(21)));
        {
            let node_b = pin!(queue.build_token(TestNode::new(42)));
            node_b.as_ref().push();
            node_c.as_ref().push();

            assert_eq!(queue.clone_to_vec(), vec![21, 42, 32]);
        }
        assert_eq!(queue.clone_to_vec(), vec![21, 32]);
    }

    #[test]
    fn pop() {
        let queue = IntrusiveLinkedList::new();
        let node_a = pin!(queue.build_token(TestNode::new(32)));
        node_a.as_ref().push();
        let node_b = pin!(queue.build_token(TestNode::new(42)));
        node_b.as_ref().push();
        let node_c = pin!(queue.build_token(TestNode::new(21)));
        node_c.as_ref().push();
        assert_eq!(queue.clone_to_vec(), vec![21, 42, 32]);

        queue.pop_if(
            |v, len| {
                assert_eq!(v.data, 32);
                assert_eq!(len, 3);
                Some(())
            },
            || panic!("shouldn't fail"),
        );
        assert_eq!(queue.clone_to_vec(), vec![21, 42]);
        queue.pop_if(
            |v, len| {
                assert_eq!(v.data, 42);
                assert_eq!(len, 2);
                Some(())
            },
            || panic!("shouldn't fail"),
        );
        assert_eq!(queue.clone_to_vec(), vec![21]);
        queue.pop_if(
            |v, len| {
                assert_eq!(v.data, 21);
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
                        let token = Box::pin(queue.build_token(TestNode::new(i)));
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
