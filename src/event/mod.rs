#![allow(dead_code)]

mod intrusive_list;
mod maybe_ref;
mod parker;

use core::task::Waker;
use intrusive_list::{IntrusiveLinkedList, ListToken, Node};
use parker::{Parker, State};

#[derive(Debug)]
pub struct Event {
    inner: IntrusiveLinkedList<Parker>,
}

impl Event {
    pub const fn new() -> Self {
        Self {
            inner: IntrusiveLinkedList::new(),
        }
    }

    pub fn new_listener(&self) -> EventListener<'_> {
        thread_local! {
            static NODE: Node<Parker> = const {Node::new(Parker::new())}
        }
        NODE.with(|node| {
            let node: &'static Node<Parker> = unsafe { core::mem::transmute(node) };
            EventListener {
                event: self,
                list_token: self.inner.build_token(node),
                is_on_queue: false,
            }
        })
    }

    pub fn new_async_listener(&self, waker: Waker) -> EventListener<'_> {
        let node = Node::new(Parker::new_with_waker(waker));
        EventListener {
            event: self,
            list_token: self.inner.build_token(node),
            is_on_queue: false,
        }
    }

    pub fn notify_one(&self) -> bool {
        self.notify_if(|_| true, || {})
    }
    pub fn notify_if(&self, condition: impl Fn(usize) -> bool, on_empty: impl Fn()) -> bool {
        while let Some(unpark_handle) = self.inner.pop_if(
            |parker, num_left| {
                if !condition(num_left) {
                    return None;
                }
                Some(parker.unpark_handle())
            },
            &on_empty,
        ) {
            if unpark_handle.un_park() {
                return true;
            }
        }
        false
    }
}
#[derive(Debug)]
pub struct EventListener<'a> {
    event: &'a Event,
    list_token: ListToken<'a, Parker>,
    is_on_queue: bool,
}

impl Drop for EventListener<'_> {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl EventListener<'_> {
    pub fn register_if(&mut self, condition: impl Fn() -> bool) -> bool {
        debug_assert!(!self.is_on_queue || self.list_token.inner().get_state() == State::Notified);
        self.list_token.inner().prepare_park();
        let did_push = self.list_token.push_if(condition);
        if did_push {
            self.is_on_queue = true;
        }
        did_push
    }

    pub fn register(&mut self) {
        self.register_if(|| true);
    }
    pub fn wait(&mut self) {
        self.list_token.inner().park();
        self.is_on_queue = false;
    }

    /// Returns true if the timeout was not reached (it was woken up!)
    pub fn wait_until(&mut self, timeout: std::time::Instant) -> bool {
        let result = self.list_token.inner().park_until(timeout);
        if !result {
            self.cancel();
        }
        self.is_on_queue = false;
        result
    }
    pub fn is_on_queue(&self) -> bool {
        self.is_on_queue
    }

    pub(crate) fn set_off_queue(&mut self) {
        self.is_on_queue = false;
    }

    /// Returns true if the cancellation was successful (had not been woken up)
    pub fn cancel(&mut self) -> bool {
        if !self.is_on_queue {
            return true;
        }
        self.is_on_queue = false;
        if self.list_token.revoke() {
            return true;
        }
        while self.list_token.inner().get_state() != State::Notified {
            // the list token has already been revoked but we haven't been woken up yet!
            core::hint::spin_loop()
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::Duration;
    #[test]
    fn test_event() {
        let event = Event::new();
        let barrier = std::sync::Barrier::new(2);
        let test_val = AtomicBool::new(false);
        thread::scope(|s| {
            s.spawn(|| {
                barrier.wait();
                thread::sleep(Duration::from_millis(50));
                test_val.store(true, Ordering::SeqCst);
                debug_assert!(event.notify_one());
                barrier.wait();
            });
            let mut listen_guard = event.new_listener();
            listen_guard.register();
            barrier.wait();
            assert!(!test_val.load(Ordering::SeqCst));
            listen_guard.wait();
            assert!(test_val.load(Ordering::SeqCst));
            barrier.wait();
        });
    }

    #[test]
    fn test_drop() {
        let event = Event::new();
        let barrier = std::sync::Barrier::new(2);
        thread::scope(|s| {
            s.spawn(|| {
                let mut listen_guard = event.new_listener();
                listen_guard.register();
                barrier.wait();
                drop(listen_guard);
                barrier.wait();
            });
            barrier.wait();
            thread::sleep(Duration::from_millis(50));
            assert!(!event.notify_one());
            barrier.wait();
        });
    }

    #[test]
    fn test_drop_spin() {
        let event = Event::new();
        let barrier = std::sync::Barrier::new(2);
        thread::scope(|s| {
            s.spawn(|| {
                let mut listen_guard = event.new_listener();
                listen_guard.register();
                barrier.wait();
                barrier.wait();
                drop(listen_guard);
                barrier.wait();
            });
            barrier.wait();
            let handle = event.inner.pop_if(
                |p, _| Some(p.unpark_handle()),
                || panic!("shouldn't fail to pop"),
            );
            barrier.wait();
            thread::sleep(Duration::from_millis(20));
            debug_assert!(handle.expect("pop failed unpark handle was None").un_park());
            barrier.wait();
        });
    }
}
