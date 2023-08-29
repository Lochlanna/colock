#![allow(dead_code)]

mod intrusive_list;
mod maybe_ref;
mod parker;

use crate::event::maybe_ref::MaybeRef;
use intrusive_list::{IntrusiveLinkedList, ListToken, Node};
use parker::{Parker, State};
use std::pin::{pin, Pin};
use std::task::{Context, Poll};

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

    pub const fn wait_while_async<S, W>(&self, will_sleep: S, on_wake: W) -> Poller<'_, S, W>
    where
        S: Fn() -> bool + Send,
        W: Fn() -> bool + Send,
    {
        Poller {
            event: self,
            will_sleep,
            on_wake,
            list_token: self
                .inner
                .build_token(MaybeRef::Owned(Node::new(Parker::new_async()))),
            did_finish: false,
            initialised: false,
        }
    }

    pub fn wait_once(&self, will_sleep: impl Fn() -> bool) {
        self.wait_while(will_sleep, || false);
    }

    pub fn wait_while(&self, will_sleep: impl Fn() -> bool, on_wake: impl Fn() -> bool) {
        thread_local! {
            static NODE: Node<Parker> = const {Node::new(Parker::new())}
        }
        let token = pin!(NODE.with(|node| {
            let node: &'static Node<Parker> = unsafe { core::mem::transmute(node) };
            self.inner.build_token(node.into())
        }));

        token.inner().prepare_park();
        while token.as_ref().push_if(&will_sleep) {
            token.inner().park();
            debug_assert_eq!(token.inner().get_state(), State::Notified);
            if !on_wake() {
                return;
            }
            token.inner().prepare_park();
        }
    }
}

pub struct Poller<'a, S, W>
where
    S: Fn() -> bool + Send,
    W: Fn() -> bool + Send,
{
    event: &'a Event,
    will_sleep: S,
    on_wake: W,
    list_token: ListToken<'a, Parker>,
    did_finish: bool,
    initialised: bool,
}

unsafe impl<S, W> Send for Poller<'_, S, W>
where
    S: Fn() -> bool + Send,
    W: Fn() -> bool + Send,
{
}

impl<S, W> Drop for Poller<'_, S, W>
where
    S: Fn() -> bool + Send,
    W: Fn() -> bool + Send,
{
    fn drop(&mut self) {
        if self.did_finish {
            return;
        }
        if self.initialised && !self.list_token.revoke() {
            // Our waker was popped of the queue but we didn't get polled. This means the notification was lost
            // We have to blindly attempt to wake someone else. This could be expensive but should be rare!
            self.event.notify_one();
        }
    }
}

impl<S, W> core::future::Future for Poller<'_, S, W>
where
    S: Fn() -> bool + Send,
    W: Fn() -> bool + Send,
{
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = unsafe { self.get_unchecked_mut() };
        if this.initialised {
            debug_assert_ne!(this.list_token.inner().get_state(), State::Waiting);
            while this.list_token.inner().get_state() != State::Notified {
                // the list token has already been revoked but we haven't been woken up yet!
                core::hint::spin_loop();
            }
            if !(this.on_wake)() {
                this.did_finish = true;
                return Poll::Ready(());
            }
        }
        this.list_token.inner().replace_waker(cx.waker().clone());
        this.initialised = true;

        let pinned_token = unsafe { Pin::new_unchecked(&this.list_token) };
        pinned_token.inner().prepare_park();
        let did_push = pinned_token.push_if(&this.will_sleep);
        if did_push {
            Poll::Pending
        } else {
            this.did_finish = true;
            Poll::Ready(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::Duration;
    #[test]
    fn basic_wait() {
        let event = Event::new();
        let test_val = AtomicBool::new(false);
        let barrier = std::sync::Barrier::new(2);
        thread::scope(|s| {
            s.spawn(|| {
                barrier.wait();
                thread::sleep(Duration::from_millis(50));
                test_val.store(true, Ordering::SeqCst);
                assert!(event.notify_one());
            });
            assert!(!test_val.load(Ordering::SeqCst));
            barrier.wait();
            event.wait_while(|| true, || false);
            assert!(test_val.load(Ordering::SeqCst));
        });
    }

    #[tokio::test(flavor = "multi_thread")]
    #[cfg_attr(miri, ignore)]
    async fn async_event() {
        let event = Event::new();
        let test_val = AtomicBool::new(false);
        let barrier = tokio::sync::Barrier::new(2);
        tokio_scoped::scope(|s| {
            s.spawn(async {
                barrier.wait().await;
                thread::sleep(Duration::from_millis(50));
                test_val.store(true, Ordering::SeqCst);
                assert!(event.notify_one());
            });
            s.spawn(async {
                assert!(!test_val.load(Ordering::SeqCst));
                barrier.wait().await;
                event.wait_while_async(|| true, || false).await;
                assert!(test_val.load(Ordering::SeqCst));
            });
        });
    }
}
