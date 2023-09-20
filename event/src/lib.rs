#![allow(dead_code)]
// #![warn(missing_docs)]
// #![warn(missing_docs_in_private_items)]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::module_name_repetitions)]
#![warn(clippy::undocumented_unsafe_blocks)]

use core::cell::Cell;
use core::pin::{pin, Pin};
use core::task::{Context, Poll, Waker};
use intrusive_list::maybe_ref::MaybeRef;
use intrusive_list::{AsyncListToken, IntrusiveLinkedList, ListToken, Node};
use parking::{ThreadParker, ThreadParkerT};
use std::fmt::{Debug, Formatter};
use std::future::Future;
use std::time::Instant;

enum Handle {
    Sync(ThreadParker),
    Async(Cell<Option<Waker>>),
}

impl Debug for Handle {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sync(_) => write!(f, "Handle::Sync"),
            Self::Async(_) => write!(f, "Handle::Async"),
        }
    }
}

trait ThreadHandle: Wakeable {
    fn thread_parker(&self) -> Option<&ThreadParker>;
}

trait AsyncHandle: Wakeable {
    fn replace_waker(&self, new_waker: Waker);
}

trait Wakeable {
    fn unpark_handle(&self) -> UnparkHandle;
}
impl Wakeable for Handle {
    fn unpark_handle(&self) -> UnparkHandle {
        match self {
            Self::Sync(thread_parker) => UnparkHandle::Sync(thread_parker),
            Self::Async(waker) => UnparkHandle::Async(waker.take()),
        }
    }
}

impl ThreadHandle for Handle {
    fn thread_parker(&self) -> Option<&ThreadParker> {
        match self {
            Self::Sync(thread_parker) => Some(thread_parker),
            Self::Async(_) => None,
        }
    }
}
impl AsyncHandle for Handle {
    fn replace_waker(&self, new_waker: Waker) {
        match self {
            Self::Sync(_) => panic!("should not replace waker for sync code"),
            Self::Async(waker) => waker.set(Some(new_waker)),
        };
    }
}
enum UnparkHandle {
    Sync(*const ThreadParker),
    Async(Option<Waker>),
}

impl UnparkHandle {
    unsafe fn unpark(self) {
        match self {
            Self::Sync(thread_parker) => {
                (*thread_parker).unpark();
            }
            Self::Async(maybe_waker) => maybe_waker
                .expect("should always have a waker. Node was re-used")
                .wake(),
        }
    }
}

#[derive(Debug)]
pub struct Event {
    inner: IntrusiveLinkedList<Handle>,
}

impl Event {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            inner: IntrusiveLinkedList::new(),
        }
    }
    fn notify_one(&self) -> bool {
        self.notify_if(|_| true, || {})
    }

    pub fn notify_all_while(
        &self,
        condition: impl Fn(usize) -> bool,
        on_empty: impl Fn(),
    ) -> usize {
        self.notify_many_while(usize::MAX, condition, on_empty)
    }

    pub fn notify_many_while(
        &self,
        max: usize,
        condition: impl Fn(usize) -> bool,
        on_empty: impl Fn(),
    ) -> usize {
        if max == 0 {
            return 0;
        }
        let num_waiting = Cell::new(max);
        let get_count = |num_in_queue| {
            num_waiting.set(num_waiting.get().min(num_in_queue));
            condition(num_in_queue)
        };
        if !self.notify_if(get_count, &on_empty) {
            return 0;
        }
        let mut num_notified = 1;
        let max = num_waiting.get();
        while num_notified < max && num_waiting.get() > 0 && self.notify_if(get_count, &on_empty) {
            num_notified += 1;
        }

        num_notified
    }
    pub fn notify_if(&self, condition: impl Fn(usize) -> bool, on_empty: impl Fn()) -> bool {
        if let Some(unpark_handle) = self.inner.pop_if(
            |parker, num_left| {
                if !condition(num_left) {
                    return None;
                }
                Some(parker.unpark_handle())
            },
            &on_empty,
        ) {
            unsafe {
                unpark_handle.unpark();
            }
            return true;
        }
        false
    }

    #[must_use]
    pub const fn wait_while_async<S, W>(&self, will_sleep: S, should_wake: W) -> Poller<'_, S, W>
    where
        S: Fn(usize) -> bool + Send + Sync,
        W: Fn() -> bool + Send,
    {
        Poller {
            event: self,
            will_sleep,
            should_wake,
            state: EventPollerState::Initial,
            token: None,
        }
    }

    pub fn wait_once(&self, will_sleep: impl Fn(usize) -> bool) {
        self.wait_while(will_sleep, || true);
    }

    fn with_thread_token<R>(&self, f: impl FnOnce(&Pin<&mut ListToken<Handle>>) -> R) -> R {
        if ThreadParker::IS_CHEAP_TO_CONSTRUCT {
            let node = Node::new(Handle::Sync(ThreadParker::const_new()));
            let token = pin!(self.inner.build_token(node.into()));
            return f(&token);
        }

        thread_local! {
            static NODE: Node<Handle> = const {Node::new(Handle::Sync(ThreadParker::const_new()))}
        }
        NODE.with(|node| {
            let token = pin!(self.inner.build_token(node.into()));
            f(&token)
        })
    }

    //TODO refactor so there's one inner wait while func with optional timeout to reduce code replication
    pub fn wait_while(&self, should_sleep: impl Fn(usize) -> bool, should_wake: impl Fn() -> bool) {
        self.with_thread_token(|token| {
            let parker = token
                .inner()
                .thread_parker()
                .expect("token should be a thread parker");
            unsafe {
                parker.prepare_park();
            }
            while token.as_ref().push_if(&should_sleep) {
                unsafe {
                    parker.park();
                }
                unsafe {
                    // This is an optimisation that means it won't try to revoke itself on drop
                    token.as_ref().set_off_queue();
                }
                if should_wake() {
                    return;
                }
                unsafe {
                    parker.prepare_park();
                }
            }
        });
    }

    pub fn wait_while_until(
        &self,
        should_sleep: impl Fn(usize) -> bool,
        should_wake: impl Fn() -> bool,
        timeout: Instant,
    ) -> bool {
        self.with_thread_token(|token| {
            let parker = token
                .inner()
                .thread_parker()
                .expect("token should be a thread parker");
            let did_time_out = Cell::new(false);

            // extend the will sleep function with timeout check code
            let will_sleep = |num_waiting| {
                let now = Instant::now();
                if now >= timeout {
                    did_time_out.set(true);
                    return false;
                }
                should_sleep(num_waiting)
            };

            if Instant::now() >= timeout {
                unsafe {
                    // This is an optimisation that means it won't try to revoke itself on drop
                    token.as_ref().set_off_queue();
                }
                return false;
            }

            unsafe {
                parker.prepare_park();
            }
            while token.as_ref().push_if(will_sleep) {
                let was_woken = unsafe { parker.park_until(timeout) };
                if !was_woken {
                    //TODO it should be possible to test this using a similar method to the async abort
                    if token.revoke() {
                        return false;
                    }
                    // We have already been popped of the queue
                    // We need to wait for the notification so that the notify function doesn't
                    // access the data we are holding on our stack in the waker
                    unsafe {
                        parker.park();
                    }
                }
                unsafe {
                    // This is an optimisation that means it won't try to revoke itself on drop
                    token.as_ref().set_off_queue();
                }
                if should_wake() {
                    return true;
                }
                unsafe {
                    parker.prepare_park();
                }
                if Instant::now() >= timeout {
                    return false;
                }
            }
            !did_time_out.get()
        })
    }

    pub fn num_waiting(&self) -> usize {
        self.inner.len()
    }
}

enum EventPollerState {
    Initial,
    Pushing,
    Pushed,
}

pub struct Poller<'a, S, W>
where
    S: Fn(usize) -> bool + Send + Sync,
    W: Fn() -> bool + Send,
{
    event: &'a Event,
    will_sleep: S,
    should_wake: W,
    state: EventPollerState,
    token: Option<AsyncListToken<'a, Handle, &'a S>>,
}

impl<S, W> Unpin for Poller<'_, S, W>
where
    S: Fn(usize) -> bool + Send + Sync,
    W: Fn() -> bool + Send,
{
}

unsafe impl<S, W> Send for Poller<'_, S, W>
where
    S: Fn(usize) -> bool + Send + Sync,
    W: Fn() -> bool + Send,
{
}

impl<S, W> Drop for Poller<'_, S, W>
where
    S: Fn(usize) -> bool + Send + Sync,
    W: Fn() -> bool + Send,
{
    fn drop(&mut self) {
        match &self.state {
            EventPollerState::Initial => {}
            EventPollerState::Pushing => {}
            EventPollerState::Pushed => {}
        }
    }
}

impl<S, W> Poller<'_, S, W>
where
    S: Fn(usize) -> bool + Send + Sync,
    W: Fn() -> bool + Send,
{
    fn on_initial(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        // we need to lock the inner list
        let node = Node::new(Handle::Async(Cell::new(None)));
        let token = self.event.inner.build_token(MaybeRef::Owned(node));
        let will_sleep = unsafe { &*std::ptr::addr_of!(self.will_sleep) };
        let poller = token.push_if_async(
            |token, cx| {
                token.inner().replace_waker(cx.waker().clone());
            },
            will_sleep,
        );
        self.token = Some(poller);
        self.state = EventPollerState::Pushing;
        self.on_pushing(cx)
    }

    fn on_pushing(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let Some(poller) = &mut self.token else {
            unreachable!()
        };
        let poller = pin!(poller);
        if let Poll::Ready(did_push) = poller.poll(cx) {
            if did_push {
                let this = unsafe { self.get_unchecked_mut() };
                this.state = EventPollerState::Pushed;
                return Poll::Pending;
            }
            return self.on_pushed(cx);
        }
        Poll::Pending
    }

    fn on_pushed(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if (self.should_wake)() {
            return Poll::Ready(());
        }

        let Some(poller) = &mut self.token else {
            unreachable!()
        };
        if !poller.revoke() && (self.should_wake)() {
            // our token has already been popped meaning we are about to be woken anyway
            return Poll::Ready(());
        }

        self.state = EventPollerState::Initial;
        self.on_initial(cx)
    }
}

impl<S, W> Future for Poller<'_, S, W>
where
    S: Fn(usize) -> bool + Send + Sync,
    W: Fn() -> bool + Send,
{
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match &mut self.state {
            EventPollerState::Initial => self.on_initial(cx),
            EventPollerState::Pushing => self.on_pushing(cx),
            EventPollerState::Pushed => self.on_pushed(cx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_polling::FuturePollingExt;
    use itertools::Itertools;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
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
                while event.num_waiting() == 0 {
                    thread::yield_now();
                }
                //make sure the other thread goes to sleep
                thread::sleep(Duration::from_millis(10));
                test_val.store(true, Ordering::SeqCst);
                assert!(event.notify_one());
            });
            assert!(!test_val.load(Ordering::SeqCst));
            barrier.wait();
            event.wait_while(|_| true, || true);
            assert!(test_val.load(Ordering::SeqCst));
        });
    }

    // Test for https://github.com/Lochlanna/colock4/issues/1
    #[tokio::test(flavor = "multi_thread")]
    #[cfg_attr(miri, ignore)]
    async fn test_async_timeout() {
        let event = Event::new();
        tokio::select! {
            _ = event.wait_while_async(|_| true, || false) => panic!("should have timed out"),
            _ = tokio::time::sleep(Duration::from_millis(5)) => {}
        }
        assert_eq!(event.num_waiting(), 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    #[cfg_attr(miri, ignore)]
    async fn async_event() {
        let event = Event::new();
        let test_val = AtomicBool::new(false);
        let barrier = tokio::sync::Barrier::new(2);
        let test_data = Arc::new((event, test_val, barrier));
        let test_data_cloned = test_data.clone();

        let handle_a = tokio::spawn(async move {
            test_data.2.wait().await;
            while test_data.0.num_waiting() == 0 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
            test_data.1.store(true, Ordering::SeqCst);
            assert!(test_data.0.notify_one());
        });

        let handle_b = tokio::spawn(async move {
            assert!(!test_data_cloned.1.load(Ordering::SeqCst));
            test_data_cloned.2.wait().await;
            test_data_cloned.0.wait_while_async(|_| true, || true).await;
            assert!(test_data_cloned.1.load(Ordering::SeqCst));
        });

        handle_a.await.expect("tokio task a panicked");
        handle_b.await.expect("tokio task b panicked");
    }

    /// Test that dropping the future before it is complete will result in it being revoked from
    /// the queue
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn cancel_async() {
        let event = Event::new();
        {
            let poller = pin!(event.wait_while_async(|_| true, || true));
            let mut polling = poller.polling();
            let result = polling.poll_once().await;
            assert_eq!(result, Poll::Pending);
            assert_eq!(event.inner.len(), 1);
        }
        assert_eq!(event.inner.len(), 0);
    }

    /// This test ensures that another thread is woken up in the case that a future is aborted
    /// in the time between it being popped from the queue by a notify and the wake up being registered.
    /// In this case it should detect this on drop and fire the notify function again
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn abort_wake_async() {
        let event = Event::new();
        let second_entry;
        let mut second_polling;
        {
            let poller = pin!(event.wait_while_async(|_| true, || true));
            let mut polling = poller.polling();
            let result = polling.poll_once().await;
            assert_eq!(result, Poll::Pending);
            assert_eq!(event.inner.len(), 1);
            event
                .inner
                .pop_if(|_, _| Some(()), || panic!("shouldn't be empty"));
            second_entry = Box::pin(event.wait_while_async(|_| true, || true));
            second_polling = second_entry.polling();
            let result = second_polling.poll_once().await;
            assert_eq!(result, Poll::Pending);
            assert_eq!(event.inner.len(), 1);
        }
        let result = second_polling.poll_once().await;
        assert_eq!(result, Poll::Ready(()));
        assert_eq!(event.inner.len(), 0);
    }

    #[test]
    fn notify_many() {
        let event = Arc::new(Event::new());
        let barrier = Arc::new(std::sync::Barrier::new(6));
        let handles = (0..5)
            .map(|_| {
                let event = event.clone();
                let barrier = barrier.clone();
                thread::spawn(move || {
                    barrier.wait();
                    event.wait_while(|_| true, || true);
                })
            })
            .collect_vec();

        barrier.wait();
        while event.inner.len() < 5 {
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(event.inner.len(), 5);
        let num_woken = event.notify_many_while(2, |_| true, || panic!("shouldn't be empty"));
        assert_eq!(num_woken, 2);
        assert_eq!(event.inner.len(), 3);
        let num_woken = event.notify_all_while(|_| true, || panic!("shouldn't be empty"));
        assert_eq!(num_woken, 3);
        assert_eq!(event.inner.len(), 0);

        handles
            .into_iter()
            .for_each(|jh| jh.join().expect("couldn't join thread"));
    }
}
