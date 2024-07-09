#![allow(dead_code)]
// #![warn(missing_docs)]
// #![warn(missing_docs_in_private_items)]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::module_name_repetitions)]
#![warn(clippy::undocumented_unsafe_blocks)]

mod handle;
mod maybe_ref;

use core::cell::Cell;
use core::pin::{pin, Pin};
use core::task::{Context, Poll};
use handle::Handle;
use intrusive_list::{ConcurrentIntrusiveLinkedList, HasNode, ListToken, Node};
use maybe_ref::MaybeRef;
use parking::{ThreadParker, ThreadParkerT};
use std::fmt::Debug;
use std::ops::Deref;
use std::time::Instant;

#[derive(Debug)]
struct TokenData<T>
where
    T: Send,
{
    handle: Option<MaybeRef<'static, Handle>>,
    metadata: T,
}

impl<T> TokenData<T>
where
    T: Send,
{
    fn thread_parker(&self) -> Option<&ThreadParker> {
        match self.handle.as_ref()?.deref() {
            Handle::Sync(thread_parker) => Some(thread_parker),
            Handle::Async(_) => None,
        }
    }
}

#[derive(Debug)]
pub struct TaggedEvent<T>
where
    T: Send,
{
    queue: ConcurrentIntrusiveLinkedList<TokenData<T>>,
}

impl<T> Default for TaggedEvent<T>
where
    T: Send,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<T> TaggedEvent<T>
where
    T: Send,
{
    #[must_use]
    pub const fn new() -> Self {
        Self {
            queue: ConcurrentIntrusiveLinkedList::new(),
        }
    }
    fn notify_one(&self) -> bool {
        self.notify_if(|_, _| true, || {})
    }

    pub fn notify_all_while(
        &self,
        condition: impl FnMut(usize, &T) -> bool,
        on_empty: impl FnMut(),
    ) -> usize {
        self.notify_many_while(usize::MAX, condition, on_empty)
    }

    pub fn notify_many_while(
        &self,
        max: usize,
        mut condition: impl FnMut(usize, &T) -> bool,
        mut on_empty: impl FnMut(),
    ) -> usize {
        if max == 0 {
            return 0;
        }
        let num_waiting = Cell::new(max);
        let mut get_count = |num_in_queue, metadata: &T| {
            num_waiting.set(num_waiting.get().min(num_in_queue));
            condition(num_in_queue, metadata)
        };
        if !self.notify_if(&mut get_count, &mut on_empty) {
            return 0;
        }
        let mut num_notified = 1;
        let max = num_waiting.get();
        while num_notified < max
            && num_waiting.get() > 0
            && self.notify_if(&mut get_count, &mut on_empty)
        {
            num_notified += 1;
        }

        num_notified
    }
    pub fn notify_if(
        &self,
        mut condition: impl FnMut(usize, &T) -> bool,
        on_empty: impl FnMut(),
    ) -> bool {
        if let Some(unpark_handle) = self.queue.pop_if(
            |parker, num_left| {
                if !condition(num_left, &parker.metadata) {
                    return None;
                }
                let handle = parker.handle.take().expect("waker was none");
                Some(handle)
            },
            on_empty,
        ) {
            unsafe {
                match unpark_handle {
                    MaybeRef::Ref(unpark_ref) => unpark_ref.unpark_ref(),
                    MaybeRef::Owned(unpark_owned) => unpark_owned.unpark_owned(),
                    MaybeRef::Boxed(boxed) => boxed.unpark_ref(),
                }
            }
            return true;
        }
        false
    }

    #[must_use]
    pub const fn wait_while_async<S, W>(
        &self,
        will_sleep: S,
        should_wake: W,
        metadata: T,
    ) -> Poller<'_, T, S, W>
    where
        S: FnMut(usize) -> bool + Send,
        W: FnMut() -> bool + Send,
    {
        let node = Node::new(TokenData {
            handle: None,
            metadata,
        });
        Poller {
            event: self,
            will_sleep,
            should_wake,
            list_token: self.queue.build_token(node),
            poller_state: PollerState::Initialised,
        }
    }

    pub fn wait_once(&self, will_sleep: impl FnMut(usize) -> bool, metadata: T) {
        self.wait_while(will_sleep, || true, metadata);
    }

    fn with_thread_token<R>(
        &self,
        f: impl FnOnce(&Pin<&mut ListToken<TokenData<T>>>) -> R,
        metadata: T,
    ) -> R {
        if ThreadParker::IS_CHEAP_TO_CONSTRUCT {
            let node = Node::new(TokenData {
                handle: Some(MaybeRef::Owned(Handle::Sync(ThreadParker::const_new()))),
                metadata,
            });
            let token = pin!(self.queue.build_token(node));
            return f(&token);
        }

        thread_local! {
            static HANDLE: Handle = const {Handle::Sync(ThreadParker::const_new())}
        }
        HANDLE.with(|handle| {
            let handle: &'static Handle = unsafe { core::mem::transmute(handle) };
            let node = Node::new(TokenData {
                handle: Some(MaybeRef::Ref(handle)),
                metadata,
            });
            let token = pin!(self.queue.build_token(node));
            f(&token)
        })
    }

    //TODO refactor so there's one inner wait while func with optional timeout to reduce code replication
    pub fn wait_while(
        &self,
        mut should_sleep: impl FnMut(usize) -> bool,
        mut should_wake: impl FnMut() -> bool,
        metadata: T,
    ) {
        self.with_thread_token(
            |token| {
                let parker = token
                    .inner()
                    .thread_parker()
                    .expect("token should be a thread parker");
                unsafe {
                    parker.prepare_park();
                }
                while token.as_ref().push_if(&mut should_sleep) {
                    unsafe {
                        parker.park();
                    }
                    if should_wake() {
                        return;
                    }
                    unsafe {
                        parker.prepare_park();
                    }
                }
            },
            metadata,
        );
    }

    pub fn wait_while_until(
        &self,
        mut should_sleep: impl FnMut(usize) -> bool,
        mut should_wake: impl FnMut() -> bool,
        timeout: Instant,
        metadata: T,
    ) -> bool {
        self.with_thread_token(
            |token| {
                let parker = token
                    .inner()
                    .thread_parker()
                    .expect("token should be a thread parker");

                let mut did_time_out = false;
                // extend the will sleep function with timeout check code
                let mut will_sleep = |num_waiting| {
                    let now = Instant::now();
                    if now >= timeout {
                        did_time_out = true;
                        return false;
                    }
                    should_sleep(num_waiting)
                };

                if Instant::now() >= timeout {
                    return false;
                }

                unsafe {
                    parker.prepare_park();
                }
                while token.as_ref().push_if(&mut will_sleep) {
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
                !did_time_out
            },
            metadata,
        )
    }

    pub fn num_waiting(&self) -> usize {
        self.queue.len()
    }
}

enum PollerState {
    Initialised,
    Pushed,
    Complete,
}

pub struct Poller<'a, T, S, W>
where
    S: FnMut(usize) -> bool + Send,
    W: FnMut() -> bool + Send,
    T: Send,
{
    event: &'a TaggedEvent<T>,
    will_sleep: S,
    should_wake: W,
    list_token: ListToken<'a, TokenData<T>>,
    poller_state: PollerState,
}

impl<T, S, W> Drop for Poller<'_, T, S, W>
where
    S: FnMut(usize) -> bool + Send,
    W: FnMut() -> bool + Send,
    T: Send,
{
    fn drop(&mut self) {
        match self.poller_state {
            PollerState::Initialised => {}
            PollerState::Pushed => {
                if !self.list_token.revoke() {
                    self.event.notify_one();
                }
            }
            PollerState::Complete => {
                debug_assert!(!self.list_token.revoke());
            }
        }
    }
}

impl<T, S, W> core::future::Future for Poller<'_, T, S, W>
where
    S: FnMut(usize) -> bool + Send,
    W: FnMut() -> bool + Send,
    T: Send,
{
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = unsafe { self.get_unchecked_mut() };
        match this.poller_state {
            PollerState::Initialised => {}
            PollerState::Pushed => {
                if (this.should_wake)() {
                    this.list_token.revoke();
                    this.poller_state = PollerState::Complete;
                    return Poll::Ready(());
                }
            }
            PollerState::Complete => {
                //TODO maybe this is an error??
                return Poll::Ready(());
            }
        }
        this.list_token
            .fast_modify_payload(|handle| {
                handle.handle = Some(MaybeRef::Owned(Handle::Async(cx.waker().clone())))
            })
            .expect("node is already on queue we cannot use fast modify here!");

        let pinned_token = unsafe { Pin::new_unchecked(&this.list_token) };
        let did_push = pinned_token.push_if(&mut this.will_sleep);
        if did_push {
            this.poller_state = PollerState::Pushed;
            Poll::Pending
        } else {
            this.poller_state = PollerState::Complete;
            Poll::Ready(())
        }
    }
}

#[derive(Debug, Default)]
pub struct Event {
    inner: TaggedEvent<()>,
}

impl Event {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            inner: TaggedEvent::new(),
        }
    }
    fn notify_one(&self) -> bool {
        self.inner.notify_one()
    }

    pub fn notify_all_while(
        &self,
        mut condition: impl FnMut(usize) -> bool,
        on_empty: impl FnMut(),
    ) -> usize {
        let condition = |num_waiting, _: &_| condition(num_waiting);
        self.inner.notify_all_while(condition, on_empty)
    }

    pub fn notify_many_while(
        &self,
        max: usize,
        mut condition: impl FnMut(usize) -> bool,
        on_empty: impl FnMut(),
    ) -> usize {
        let condition = |num_waiting, _: &_| condition(num_waiting);
        self.inner.notify_many_while(max, condition, on_empty)
    }

    pub fn notify_if(
        &self,
        mut condition: impl FnMut(usize) -> bool,
        on_empty: impl FnMut(),
    ) -> bool {
        let condition = |num_waiting, _: &_| condition(num_waiting);
        self.inner.notify_if(condition, on_empty)
    }

    #[must_use]
    pub const fn wait_while_async<S, W>(
        &self,
        will_sleep: S,
        should_wake: W,
    ) -> Poller<'_, (), S, W>
    where
        S: FnMut(usize) -> bool + Send,
        W: FnMut() -> bool + Send,
    {
        self.inner.wait_while_async(will_sleep, should_wake, ())
    }

    pub fn wait_once(&self, will_sleep: impl FnMut(usize) -> bool) {
        self.inner.wait_once(will_sleep, ());
    }

    pub fn wait_while(
        &self,
        should_sleep: impl FnMut(usize) -> bool,
        should_wake: impl FnMut() -> bool,
    ) {
        self.inner.wait_while(should_sleep, should_wake, ());
    }

    pub fn wait_while_until(
        &self,
        should_sleep: impl FnMut(usize) -> bool,
        should_wake: impl FnMut() -> bool,
        timeout: Instant,
    ) -> bool {
        self.inner
            .wait_while_until(should_sleep, should_wake, timeout, ())
    }

    pub fn num_waiting(&self) -> usize {
        self.inner.num_waiting()
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
            let poller = pin!(event.wait_while_async(|_| true, || false));
            let mut polling = poller.polling();
            let result = polling.poll_once().await;
            assert_eq!(result, Poll::Pending);
            assert_eq!(event.inner.queue.len(), 1);
        }
        assert_eq!(event.inner.queue.len(), 0);
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
            let poller = pin!(event.wait_while_async(|_| true, || false));
            let mut polling = poller.polling();
            let result = polling.poll_once().await;
            assert_eq!(result, Poll::Pending);
            assert_eq!(event.inner.queue.len(), 1);
            event
                .inner
                .queue
                .pop_if(|_, _| Some(()), || panic!("shouldn't be empty"));
            second_entry = Box::pin(event.wait_while_async(|_| true, || false));
            second_polling = second_entry.polling();
            let result = second_polling.poll_once().await;
            assert_eq!(result, Poll::Pending);
            assert_eq!(event.inner.queue.len(), 1);
        }
        assert_eq!(event.inner.queue.len(), 0);
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
        while event.inner.queue.len() < 5 {
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(event.inner.queue.len(), 5);
        let num_woken = event.notify_many_while(2, |_| true, || panic!("shouldn't be empty"));
        assert_eq!(num_woken, 2);
        assert_eq!(event.inner.queue.len(), 3);
        let num_woken = event.notify_all_while(|_| true, || panic!("shouldn't be empty"));
        assert_eq!(num_woken, 3);
        assert_eq!(event.inner.queue.len(), 0);

        handles
            .into_iter()
            .for_each(|jh| jh.join().expect("couldn't join thread"));
    }
}
