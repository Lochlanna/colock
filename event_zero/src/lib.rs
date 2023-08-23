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
use std::cell::Cell;
use std::sync::atomic::{fence, Ordering};

pub trait Listener {
    fn register_with_callback<R>(&self, callback: impl Fn() -> R) -> R;
    //TODO pin me!
    fn register(&self) {
        self.register_with_callback(|| ())
    }
    //TODO pin me!
    fn wait(&self);
}
pub trait EventApi {
    #[allow(clippy::declare_interior_mutable_const)]
    const NEW: Self;
    type Listener<'a>: Listener
    where
        Self: 'a;
    fn new_listener(&self) -> Self::Listener<'_>;

    fn notify_one(&self) -> bool {
        self.notify_one_with_callback(|_| ()).is_some()
    }
    fn notify_one_with_callback<R>(&self, callback: impl Fn(usize) -> R) -> Option<R>;
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
                is_on_queue: Cell::new(false),
            }
        })
    }

    fn notify_one_with_callback<R>(&self, callback: impl Fn(usize) -> R) -> Option<R> {
        full_fence();
        self.inner
            .pop(|parker, num_left| (parker.unpark_handle(), callback(num_left)))
            .map(|(unpark_handle, callback_result)| {
                unpark_handle.un_park();
                callback_result
            })
    }
}

#[inline]
fn full_fence() {
    #[cfg(all(any(target_arch = "x86", target_arch = "x86_64"), not(miri), not(loom)))]
    {
        use core::{arch::asm, cell::UnsafeCell};
        // HACK(stjepang): On x86 architectures there are two different ways of executing
        // a `SeqCst` fence.
        //
        // 1. `atomic::fence(SeqCst)`, which compiles into a `mfence` instruction.
        // 2. A `lock <op>` instruction.
        //
        // Both instructions have the effect of a full barrier, but empirical benchmarks have shown
        // that the second one is sometimes a bit faster.
        let a = UnsafeCell::new(0_usize);
        // It is common to use `lock or` here, but when using a local variable, `lock not`, which
        // does not change the flag, should be slightly more efficient.
        // Refs: https://www.felixcloutier.com/x86/not
        unsafe {
            #[cfg(target_pointer_width = "64")]
            asm!("lock not qword ptr [{0}]", in(reg) a.get(), options(nostack, preserves_flags));
            #[cfg(target_pointer_width = "32")]
            asm!("lock not dword ptr [{0:e}]", in(reg) a.get(), options(nostack, preserves_flags));
        }
        return;
    }
    #[allow(unreachable_code)]
    {
        fence(Ordering::SeqCst);
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
    is_on_queue: Cell<bool>,
}

impl<L> Drop for EventTokenImpl<'_, L>
where
    L: IntrusiveList<Parker>,
{
    fn drop(&mut self) {
        if !self.is_on_queue.get() {
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
    fn register_with_callback<R>(&self, callback: impl Fn() -> R) -> R {
        self.is_on_queue.set(true);
        self.list_token.inner().prepare_park();
        let callback_res = self.list_token.push_with_callback(callback);
        full_fence();
        callback_res
    }
    fn wait(&self) {
        self.list_token.inner().park();
        self.is_on_queue.set(false);
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
            let listen_guard = event.new_listener();
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
                let listen_guard = event.new_listener();
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
    fn hypothesis() {
        let thread = thread::current();
        thread.unpark();
        thread::park();
    }

    #[test]
    fn test_drop_spin() {
        let event = Event::new();
        let barrier = std::sync::Barrier::new(2);
        thread::scope(|s| {
            s.spawn(|| {
                let listen_guard = event.new_listener();
                listen_guard.register();
                barrier.wait();
                barrier.wait();
                drop(listen_guard);
                barrier.wait();
            });
            barrier.wait();
            let handle = event.inner.pop(|p, _| p.unpark_handle());
            barrier.wait();
            thread::sleep(Duration::from_millis(20));
            debug_assert!(handle.expect("couldn't unwrap unpark handle").un_park());
            barrier.wait();
        });
    }
}
