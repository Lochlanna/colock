use crate::event::maybe_ref::MaybeRef;
use core::cell::Cell;
use core::fmt::{Debug, Formatter};

#[derive(Debug)]
pub struct IntrusiveLinkedList<T> {
    inner: spin::Mutex<IntrusiveLinkedListInner<T>>,
}

unsafe impl<T> Sync for IntrusiveLinkedList<T> {}
unsafe impl<T> Send for IntrusiveLinkedList<T> {}

impl<T> IntrusiveLinkedList<T> {
    pub const fn new() -> Self {
        Self {
            inner: spin::Mutex::new(IntrusiveLinkedListInner::new()),
        }
    }

    pub fn pop_if<R>(
        &self,
        condition: impl Fn(&T, usize) -> Option<R>,
        on_empty: impl Fn(),
    ) -> Option<R> {
        let mut inner = self.inner.lock();
        inner.pop_if(condition, on_empty)
    }

    pub fn build_node(data: T) -> Node<T> {
        Node::new(data)
    }

    pub fn build_token<'a>(&'a self, node: impl Into<MaybeRef<'a, Node<T>>>) -> ListToken<'a, T>
    where
        T: 'a,
    {
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
        self.inner.lock().to_vec()
    }

    pub fn pop_clone(&self) -> Option<T> {
        self.inner.lock().pop_if(|v, _| Some(v.clone()), || {})
    }
}
struct IntrusiveLinkedListInner<T> {
    head: *const Node<T>,
    tail: *const Node<T>,
    length: usize,
}

impl<T> IntrusiveLinkedListInner<T> {
    const fn new() -> Self {
        Self {
            head: core::ptr::null(),
            length: 0,
            tail: core::ptr::null(),
        }
    }

    fn pop_if<R>(
        &mut self,
        condition: impl Fn(&T, usize) -> Option<R>,
        on_empty: impl Fn(),
    ) -> Option<R> {
        if self.head.is_null() {
            //it's empty!
            debug_assert_eq!(self.length, 0);
            on_empty();
            return None;
        }
        unsafe {
            let tail = &*self.tail;
            debug_assert!(tail.is_on_queue.get());
            let ret = condition(&tail.data, self.length);
            ret.as_ref()?;
            tail.remove(self);
            debug_assert!(!tail.is_on_queue.get());
            ret
        }
    }
}

impl<T> IntrusiveLinkedListInner<T>
where
    T: Clone,
{
    fn to_vec(&self) -> Vec<T> {
        let mut data = Vec::new();
        if self.head.is_null() {
            return data;
        }
        let mut current = self.head;
        while !current.is_null() {
            let current_ref = unsafe { &*current };
            data.push(current_ref.data.clone());
            current = current_ref.next.get()
        }
        data
    }
}

impl<T> Debug for IntrusiveLinkedListInner<T>
where
    T: Debug,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        if self.head.is_null() {
            return f.debug_struct("IntrusiveLinkedList").finish();
        }
        let mut output = String::from("IntrusiveLinkedList [");
        let mut current = self.head;
        let mut next_str = String::from("\n");
        while !current.is_null() {
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
            current = current_ref.next.get()
        }
        output += next_str.as_str();
        output.push_str("\n]");
        f.write_str(output.as_str())
    }
}

#[repr(align(2))]
#[derive(Debug)]
pub struct Node<T> {
    next: Cell<*const Node<T>>,
    prev: Cell<*const Node<T>>,
    is_on_queue: Cell<bool>,
    data: T,
}
impl<T> Node<T> {
    pub const fn new(data: T) -> Self {
        Self {
            next: Cell::new(core::ptr::null()),
            prev: Cell::new(core::ptr::null()),
            is_on_queue: Cell::new(false),
            data,
        }
    }

    fn push(&self, queue: &mut IntrusiveLinkedListInner<T>) {
        self.push_if(queue, || true);
    }

    fn push_if(
        &self,
        queue: &mut IntrusiveLinkedListInner<T>,
        condition: impl Fn() -> bool,
    ) -> bool {
        let should_push = condition();
        if !should_push {
            debug_assert!(!self.is_on_queue.get());
            return should_push;
        }

        self.prev.set(core::ptr::null());
        self.next.set(queue.head);
        if queue.head.is_null() {
            queue.tail = self;
        } else {
            let head_ref = unsafe { &*queue.head };
            head_ref.prev.set(self);
        }
        queue.head = self;
        self.is_on_queue.set(true);

        queue.length += 1;

        true
    }

    fn revoke(&self, queue: &mut IntrusiveLinkedListInner<T>) -> bool {
        if !self.is_on_queue.replace(false) {
            return false;
        }
        self.remove(queue);
        true
    }
    fn remove(&self, queue: &mut IntrusiveLinkedListInner<T>) {
        queue.length -= 1;

        if self.prev.get().is_null() {
            // This is the head
            let next = self.next.get();
            if let Some(next) = unsafe { next.as_ref() } {
                next.prev.set(core::ptr::null());
            } else {
                // and the tail!
                debug_assert_eq!(queue.tail, self as *const _);
                queue.tail = core::ptr::null();
            }
            queue.head = next;
        } else if self.next.get().is_null() {
            // this is the tail!
            debug_assert_eq!(queue.tail, self as *const _);
            let prev = unsafe { &*self.prev.get() };
            prev.next.set(core::ptr::null());
            queue.tail = prev;
        } else {
            // we are in the middle somewhere
            let prev = unsafe { &*self.prev.get() };
            let next = unsafe { &*self.next.get() };
            next.prev.set(prev);
            prev.next.set(next);
        }

        self.next.set(core::ptr::null());
        self.prev.set(core::ptr::null());

        self.is_on_queue.set(false)
    }
}

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
    pub fn push(&self) {
        self.push_if(|| true);
    }
    pub fn push_if(&self, condition: impl Fn() -> bool) -> bool {
        let mut queue = self.queue.inner.lock();
        self.node.push_if(&mut queue, condition)
    }

    pub fn revoke(&self) -> bool {
        let mut queue = self.queue.inner.lock();
        self.node.revoke(&mut queue)
    }

    pub fn inner(&self) -> &T {
        &self.node.data
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use itertools::Itertools;
    use std::collections::HashMap;
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