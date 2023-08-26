#![allow(dead_code)]

mod const_atomic_ptr;
mod intrusive_list;
mod maybe_ref;
mod parker;

use crate::intrusive_list::intrusive_linked_list::Node;
use crate::parker::State;
use intrusive_list::intrusive_linked_list::IntrusiveLinkedList;
use intrusive_list::{IntrusiveList, IntrusiveToken};
use parker::Parker;

pub trait Listener {
    fn register_if(&mut self, condition: impl Fn() -> bool) -> bool;
    //TODO pin me!
    fn register(&mut self) {
        self.register_if(|| true);
    }
    //TODO pin me!
    fn wait(&mut self);
}
pub trait EventApi {
    #[allow(clippy::declare_interior_mutable_const)]
    const NEW: Self;
    type Listener<'a>: Listener
    where
        Self: 'a;
    fn new_listener(&self) -> Self::Listener<'_>;

    fn notify_one(&self) -> bool {
        self.notify_if(|_| true, || {})
    }
    fn notify_if(&self, condition: impl Fn(usize) -> bool, on_empty: impl Fn()) -> bool;
}

pub struct EventImpl<L>
where
    L: IntrusiveList<Parker>,
{
    inner: L,
}

impl<L> EventImpl<L>
where
    L: IntrusiveList<Parker>,
{
    pub const fn new() -> Self {
        Self { inner: L::NEW }
    }
}
impl EventApi for EventImpl<IntrusiveLinkedList<Parker>> {
    #[allow(clippy::declare_interior_mutable_const)]
    const NEW: Self = Self::new();
    type Listener<'a> = EventTokenImpl<'a, IntrusiveLinkedList<Parker>> where Self: 'a;

    fn new_listener(&self) -> Self::Listener<'_> {
        thread_local! {
            static NODE: Node<Parker> = const {Node::new(Parker::new())}
        }
        NODE.with(|node| {
            let node: &'static Node<Parker> = unsafe { core::mem::transmute(node) };
            EventTokenImpl {
                event: self,
                list_token: self.inner.build_token(node),
                is_on_queue: false,
            }
        })
    }

    fn notify_if(&self, condition: impl Fn(usize) -> bool, on_empty: impl Fn()) -> bool {
        if let Some(unpark_handle) = self.inner.pop_if(
            |parker, num_left| {
                if !condition(num_left) {
                    return None;
                }
                Some(parker.unpark_handle())
            },
            on_empty,
        ) {
            unpark_handle.un_park();
            return true;
        }
        false
    }
}

pub type Event = EventImpl<IntrusiveLinkedList<Parker>>;
pub type Token<'a> = EventTokenImpl<'a, IntrusiveLinkedList<Parker>>;
pub struct EventTokenImpl<'a, L>
where
    L: IntrusiveList<Parker>,
{
    event: &'a EventImpl<L>,
    list_token: L::Token<'a>,
    is_on_queue: bool,
}

impl<L> Drop for EventTokenImpl<'_, L>
where
    L: IntrusiveList<Parker>,
{
    fn drop(&mut self) {
        if !self.is_on_queue {
            return;
        }
        if self.list_token.revoke() {
            return;
        }
        while self.list_token.inner().get_state() != State::Notified {
            // the list token has already been revoked but we haven't been woken up yet!
            core::hint::spin_loop()
        }
    }
}

impl<L> Listener for EventTokenImpl<'_, L>
where
    L: IntrusiveList<Parker>,
{
    fn register_if(&mut self, condition: impl Fn() -> bool) -> bool {
        debug_assert!(!self.is_on_queue);
        self.list_token.inner().prepare_park();
        let did_push = self.list_token.push_if(condition);
        if did_push {
            self.is_on_queue = true;
        }
        did_push
    }
    fn wait(&mut self) {
        self.list_token.inner().park();
        self.is_on_queue = false;
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
                thread::sleep(std::time::Duration::from_millis(50));
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
            thread::sleep(std::time::Duration::from_millis(50));
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
