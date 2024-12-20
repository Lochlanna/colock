use std::fmt::{Debug, Formatter};
use std::marker::PhantomPinned;
use std::ops::{Deref, DerefMut};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Eq, PartialEq)]
pub enum Error {
    DirtyNode,
    AbortedPush,
}

#[derive(Debug)]
pub struct Node<D> {
    next: *mut Self,
    prev: *mut Self,
    list: *const ConcurrentIntrusiveList<D>,
    data: Option<D>,
    is_on_list: AtomicBool,
    _phantom_pinned: PhantomPinned
}

unsafe impl<D> Send for Node<D> where D:Send{}

impl<D> Drop for Node<D> {
    fn drop(&mut self) {
        if self.is_on_list.load(Ordering::Acquire) {
            let list = unsafe {&*self.list};
            list.with_lock(|list|{
               list.remove_node(self);
            });
        }
    }
}

impl<D> Node<D> {
    pub const fn new(data: D) -> Self {
        Self {
            next: core::ptr::null_mut(),
            prev: core::ptr::null_mut(),
            list: core::ptr::null_mut(),
            data: Some(data),
            is_on_list: AtomicBool::new(false),
            _phantom_pinned: PhantomPinned,
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum NodeAction{
    Retain,
    Remove,
}

#[derive(Debug, Eq, PartialEq)]
pub enum ScanAction{
    Continue,
    Stop,
}

pub struct ConcurrentIntrusiveList<D> {
    inner_list: mini_lock::MiniLock<IntrusiveList<D>>
}

impl<D> ConcurrentIntrusiveList<D> {
    pub const fn new()-> Self {
        Self {
            inner_list: mini_lock::MiniLock::new(IntrusiveList::new()),
        }
    }

    pub fn push_head<F, R>(&self, node: Pin<&mut Node<D>>, critical_condition: F) -> (Result<usize, Error>, R) where F: FnOnce(&mut D, usize)->(bool, R) {
        let node = unsafe {node.get_unchecked_mut()};
        let data = node.data.as_mut().expect("there is no data in the node");
        let push_result;
        let critical_result;
        {
            let mut list = self.inner_list.lock();
            let should_push;
            (should_push, critical_result) = critical_condition(data, list.count);
            if !should_push {
                return (Err(Error::AbortedPush), critical_result);
            }
            push_result = list.push_head(node, self);
        }
        (push_result, critical_result)
    }

    pub fn push_tail<F, R>(&self, node: Pin<&mut Node<D>>, critical_condition: F) -> (Result<usize, Error>, R) where F: FnOnce(&mut D, usize)->(bool, R) {
        let node = unsafe {node.get_unchecked_mut()};
        let data = node.data.as_mut().expect("there is no data in the node");
        let push_result;
        let critical_result;
        {
            let mut list = self.inner_list.lock();
            let should_push;
            (should_push, critical_result) = critical_condition(data, list.count);
            if !should_push {
                return (Err(Error::AbortedPush), critical_result);
            }
            push_result = list.push_tail(node, self);
        }
        (push_result, critical_result)
    }

    pub fn pop_head<F, R>(&self, critical_condition: F, critical_on_empty: impl FnOnce()) -> Option<(R, usize)> where F: FnOnce(D, usize)->R {
        let mut list = self.inner_list.lock();
        let Some(data) = list.pop_head() else {
            critical_on_empty();
            return None;
        };
        let critical_result = critical_condition(data, list.count);
        Some((critical_result, list.count))
    }

    pub fn pop_tail<F, R>(&self, critical_condition: F, critical_on_empty: impl FnOnce()) -> Option<(R, usize)> where F: FnOnce(D, usize)->R {
        let mut list = self.inner_list.lock();
        let Some(data) = list.pop_tail() else {
            critical_on_empty();
            return None;
        };
        let critical_result = critical_condition(data, list.count);
        Some((critical_result, list.count))
    }


    /// Pop all elements without dropping the lock. This should be faster than looping with pop_tail
    pub fn pop_all<F>(&self, critical_condition: F, critical_on_empty: impl FnOnce()) -> usize  where F: Fn(D, usize) {
        let mut list = self.inner_list.lock();
        let num_popped = list.count;
        while let Some(data) = list.pop_tail() {
            critical_condition(data, list.count);
        }
        critical_on_empty();
        num_popped
    }

    pub fn count(&self)->usize {
        self.inner_list.lock().count
    }

    pub const fn make_node(data: D)->Node<D> {
        Node::new(data)
    }

    pub fn scan(&self, f: impl FnMut(&mut D, usize)->(NodeAction, ScanAction), on_empty: impl FnOnce()) -> usize {
        let mut list = self.inner_list.lock();
        if list.count == 0 {
            on_empty();
        } else {
            list.scan(f);
        }
        list.count
    }

    pub fn with_lock<F, R>(&self, f: F) -> R where F: FnOnce(&mut IntrusiveList<D>) -> R {
        let mut list = self.inner_list.lock();
        f(list.deref_mut())
    }
}

impl<D> Debug for ConcurrentIntrusiveList<D> where D:Debug {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let mut f_struct = f.debug_struct("ConcurrentIntrusiveList");
        let list = self.inner_list.lock();
        f_struct.field("inner_list", list.deref());
        f_struct.finish()
    }
}

impl<D> Default for ConcurrentIntrusiveList<D> {
    fn default() -> Self {
        Self::new()
    }
}


#[derive(Debug)]
pub struct IntrusiveList<D> {
    head: *mut Node<D>,
    tail: *mut Node<D>,
    count: usize
}

unsafe impl<D> Send for IntrusiveList<D> {}

impl<D> IntrusiveList<D> {
    const fn new()->Self {
        Self {
            head: core::ptr::null_mut(),
            tail: core::ptr::null_mut(),
            count: 0,
        }
    }

    pub fn push_head(&mut self, node: &mut Node<D>, outer_list: &ConcurrentIntrusiveList<D>) -> Result<usize, Error> {
        if node.is_on_list.swap(true, Ordering::AcqRel) || !node.prev.is_null() || !node.next.is_null() || !node.list.is_null() {
            return Err(Error::DirtyNode)
        }

        node.list = outer_list;

        if self.head.is_null() {
            debug_assert!(self.tail.is_null());
            self.head = node;
            self.tail = node;
        } else {
            debug_assert!(!self.tail.is_null());
            let existing_node = unsafe {&mut *self.head};
            debug_assert!(existing_node.prev.is_null());
            existing_node.prev = node;
            node.next = existing_node;
            self.head = node;
        }

        self.count += 1;
        Ok(self.count)
    }

    pub fn push_tail(&mut self, node: &mut Node<D>, outer_list: &ConcurrentIntrusiveList<D>) -> Result<usize, Error> {
        if node.is_on_list.swap(true, Ordering::AcqRel) || !node.prev.is_null() || !node.next.is_null() || !node.list.is_null() {
            return Err(Error::DirtyNode)
        }

        node.list = outer_list;

        if self.tail.is_null() {
            debug_assert!(self.head.is_null());
            self.head = node;
            self.tail = node;
        } else {
            debug_assert!(!self.head.is_null());
            let existing_node = unsafe {&mut *self.tail};
            debug_assert!(existing_node.next.is_null());
            existing_node.next = node;
            node.prev = existing_node;
            self.tail = node;
        }

        self.count += 1;
        Ok(self.count)
    }

    pub fn pop_head(&mut self) -> Option<D> {
        if self.head.is_null() {
            return None;
        }
        let head = unsafe {&mut* self.head};
        self.remove_node(head);
        
        let data = head.data.take();
        debug_assert!(data.is_some(), "data was null on pop head");
        head.is_on_list.store(false, Ordering::Release);
        data
    }

    pub fn pop_tail(&mut self) -> Option<D> {
        if self.tail.is_null() {
            return None;
        }
        let tail = unsafe {&mut* self.tail};
        self.remove_node(tail);


        let data = tail.data.take();
        debug_assert!(data.is_some(), "data was null on pop tail");
        tail.is_on_list.store(false, Ordering::Release);
        data
    }

    pub fn remove_node(&mut self, node: &mut Node<D>) {
        if !node.is_on_list.swap(false, Ordering::AcqRel) {
            return;
        }
        if self.head == node {
            let head = unsafe {&mut* self.head};
            debug_assert!(head.prev.is_null());
            if self.head == self.tail {
                debug_assert!(head.next.is_null());
                self.head = core::ptr::null_mut();
                self.tail = core::ptr::null_mut();
            } else {
                let next = unsafe {&mut *head.next};
                next.prev = core::ptr::null_mut();
                self.head = head.next;
            }
        } else if self.tail == node {
            let tail = unsafe {&mut* self.tail};
            if self.head == self.tail {
                debug_assert!(tail.prev.is_null());
                self.head = core::ptr::null_mut();
                self.tail = core::ptr::null_mut();
            } else {
                let prev = unsafe {&mut *tail.prev};
                prev.next = core::ptr::null_mut();
                self.tail = tail.prev;
            }
        } else {
            // we are somewhere in the middle of the list
            let prev = unsafe {&mut *node.prev};
            let next = unsafe {&mut *node.next};
            prev.next = node.next;
            next.prev = node.prev;
        }
        self.count -= 1;
    }

    pub fn scan(&mut self, mut f: impl FnMut(&mut D, usize)->(NodeAction, ScanAction)) {
        let mut current_node = self.head;
        let mut scan_action= ScanAction::Continue;
        while !current_node.is_null() && scan_action == ScanAction::Continue {
            let current_node_ref = unsafe {&mut *current_node};
            current_node = current_node_ref.next;
            let node_action;
            (node_action, scan_action) = f(current_node_ref.data.as_mut().unwrap(), self.count);
            match node_action {
                NodeAction::Retain => {
                }
                NodeAction::Remove => {
                    current_node_ref.data = None;
                    self.remove_node(current_node_ref)
                }
            }
        }
    }

    pub fn count(&self)->usize {
        self.count
    }
}

#[cfg(test)]
mod tests {
    use super::*;


}
