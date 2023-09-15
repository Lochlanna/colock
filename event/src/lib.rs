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
use intrusive_list::{IntrusiveLinkedList, ListToken, Node};
use parking::{ThreadParker, ThreadParkerT};
use std::fmt::{Debug, Formatter};
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
    unsafe fn unpark(self) -> bool {
        match self {
            Self::Sync(thread_parker) => {
                (*thread_parker).unpark();
                //TODO unpark could also return a bool..?
                true
            }
            Self::Async(maybe_waker) => maybe_waker.map_or(false, |waker| {
                waker.wake();
                true
            }),
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
    pub fn notify_one(&self) -> bool {
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
        let max = Cell::new(max);
        let get_count = |num_in_queue| {
            max.set(max.get().min(num_in_queue));
            condition(num_in_queue)
        };
        if !self.notify_if(get_count, &on_empty) {
            return 0;
        }
        let mut num_notified = 1;
        let max = max.get();
        while num_notified < max && self.notify_if(&condition, &on_empty) {
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
                .build_token(MaybeRef::Owned(Node::new(Handle::Async(Cell::new(None))))),
            did_finish: false,
            initialised: false,
        }
    }

    pub fn wait_once(&self, will_sleep: impl Fn() -> bool) {
        self.wait_while(will_sleep, || false);
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

    pub fn wait_while(&self, should_sleep: impl Fn() -> bool, on_wake: impl Fn() -> bool) {
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
                if !on_wake() {
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
        should_sleep: impl Fn() -> bool,
        on_wake: impl Fn() -> bool,
        timeout: Instant,
    ) -> bool {
        self.with_thread_token(|token| {
            let parker = token
                .inner()
                .thread_parker()
                .expect("token should be a thread parker");
            let did_time_out = Cell::new(false);

            // extend the will sleep function with timeout check code
            let will_sleep = || {
                let now = Instant::now();
                if now >= timeout {
                    did_time_out.set(true);
                    return false;
                }
                should_sleep()
            };

            if Instant::now() >= timeout {
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
                if !on_wake() {
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

pub struct Poller<'a, S, W>
where
    S: Fn() -> bool + Send,
    W: Fn() -> bool + Send,
{
    event: &'a Event,
    will_sleep: S,
    on_wake: W,
    list_token: ListToken<'a, Handle>,
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
            if !(this.on_wake)() {
                this.did_finish = true;
                return Poll::Ready(());
            }
            this.list_token.revoke();
        }
        this.list_token.inner().replace_waker(cx.waker().clone());
        this.initialised = true;

        let pinned_token = unsafe { Pin::new_unchecked(&this.list_token) };
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
            event.wait_while(|| true, || false);
            assert!(test_val.load(Ordering::SeqCst));
        });
    }

    // Test for https://github.com/Lochlanna/colock4/issues/1
    #[tokio::test(flavor = "multi_thread")]
    #[cfg_attr(miri, ignore)]
    async fn test_async_timeout() {
        let event = Event::new();
        tokio::select! {
            _ = event.wait_while_async(|| true, || true) => panic!("should have timed out"),
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
            test_data_cloned.0.wait_while_async(|| true, || false).await;
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
            let poller = pin!(event.wait_while_async(|| true, || false));
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
            let poller = pin!(event.wait_while_async(|| true, || false));
            let mut polling = poller.polling();
            let result = polling.poll_once().await;
            assert_eq!(result, Poll::Pending);
            assert_eq!(event.inner.len(), 1);
            event
                .inner
                .pop_if(|_, _| Some(()), || panic!("shouldn't be empty"));
            second_entry = Box::pin(event.wait_while_async(|| true, || false));
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
                    event.wait_while(|| true, || false);
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
