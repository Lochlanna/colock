#![allow(dead_code)]
// #![warn(missing_docs)]
// #![warn(missing_docs_in_private_items)]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::module_name_repetitions)]
#![warn(clippy::undocumented_unsafe_blocks)]

mod handle;

use core::cell::Cell;
use core::pin::{pin, Pin};
use core::task::{Context, Poll};
use handle::{AsyncHandle, Handle, ThreadHandle, Wakeable};
use intrusive_list::maybe_ref::MaybeRef;
use intrusive_list::{HasNode, IntrusiveLinkedList, ListToken, NodeData};
use parking::{ThreadParker, ThreadParkerT};
use std::fmt::Debug;
use std::time::Instant;

#[derive(Debug)]
struct IntrusiveNode<T> {
    node_data: NodeData<Self>,
    handle: MaybeRef<'static, Handle>,
    metadata: T,
}

impl<T> HasNode<Self> for IntrusiveNode<T> {
    fn get_node(&self) -> &NodeData<Self> {
        &self.node_data
    }
}

#[derive(Debug)]
pub struct TaggedEvent<T> {
    queue: IntrusiveLinkedList<IntrusiveNode<T>>,
}

impl<T> Default for TaggedEvent<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> TaggedEvent<T> {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            queue: IntrusiveLinkedList::new(),
        }
    }
    fn notify_one(&self) -> bool {
        self.notify_if(|_, _| true, || {})
    }

    pub fn notify_all_while(
        &self,
        condition: impl Fn(usize, &T) -> bool,
        on_empty: impl Fn(),
    ) -> usize {
        self.notify_many_while(usize::MAX, condition, on_empty)
    }

    pub fn notify_many_while(
        &self,
        max: usize,
        condition: impl Fn(usize, &T) -> bool,
        on_empty: impl Fn(),
    ) -> usize {
        if max == 0 {
            return 0;
        }
        let num_waiting = Cell::new(max);
        let get_count = |num_in_queue, metadata: &T| {
            num_waiting.set(num_waiting.get().min(num_in_queue));
            condition(num_in_queue, metadata)
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
    pub fn notify_if(&self, condition: impl Fn(usize, &T) -> bool, on_empty: impl Fn()) -> bool {
        if let Some(unpark_handle) = self.queue.pop_if(
            |parker, num_left| {
                if !condition(num_left, &parker.metadata) {
                    return None;
                }
                Some(parker.handle.unpark_handle())
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
    pub const fn wait_while_async<S, W>(
        &self,
        will_sleep: S,
        should_wake: W,
        metadata: T,
    ) -> Poller<'_, T, S, W>
    where
        S: Fn(usize) -> bool + Send,
        W: Fn() -> bool + Send,
    {
        let handle = Handle::Async(Cell::new(None));
        let node = IntrusiveNode {
            node_data: NodeData::new(),
            handle: MaybeRef::Owned(handle),
            metadata,
        };
        Poller {
            event: self,
            will_sleep,
            should_wake,
            list_token: self.queue.build_token(MaybeRef::Owned(node)),
            token_on_queue: false,
        }
    }

    pub fn wait_once(&self, will_sleep: impl Fn(usize) -> bool, metadata: T) {
        self.wait_while(will_sleep, || true, metadata);
    }

    fn with_thread_token<R>(
        &self,
        f: impl FnOnce(&Pin<&mut ListToken<IntrusiveNode<T>>>) -> R,
        metadata: T,
    ) -> R {
        if ThreadParker::IS_CHEAP_TO_CONSTRUCT {
            let node = IntrusiveNode {
                node_data: NodeData::new(),
                handle: MaybeRef::Owned(Handle::Sync(ThreadParker::const_new())),
                metadata,
            };
            let token = pin!(self.queue.build_token(node.into()));
            return f(&token);
        }

        thread_local! {
            static HANDLE: Handle = const {Handle::Sync(ThreadParker::const_new())}
        }
        HANDLE.with(|handle| {
            let handle: &'static Handle = unsafe { core::mem::transmute(handle) };
            let node = IntrusiveNode {
                node_data: NodeData::new(),
                handle: MaybeRef::Ref(handle),
                metadata,
            };
            let token = pin!(self.queue.build_token(node.into()));
            f(&token)
        })
    }

    //TODO refactor so there's one inner wait while func with optional timeout to reduce code replication
    pub fn wait_while(
        &self,
        should_sleep: impl Fn(usize) -> bool,
        should_wake: impl Fn() -> bool,
        metadata: T,
    ) {
        self.with_thread_token(
            |token| {
                let parker = token
                    .inner()
                    .handle
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
            },
            metadata,
        );
    }

    pub fn wait_while_until(
        &self,
        should_sleep: impl Fn(usize) -> bool,
        should_wake: impl Fn() -> bool,
        timeout: Instant,
        metadata: T,
    ) -> bool {
        self.with_thread_token(
            |token| {
                let parker = token
                    .inner()
                    .handle
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
            },
            metadata,
        )
    }

    pub fn num_waiting(&self) -> usize {
        self.queue.len()
    }
}

pub struct Poller<'a, T, S, W>
where
    S: Fn(usize) -> bool + Send,
    W: Fn() -> bool + Send,
{
    event: &'a TaggedEvent<T>,
    will_sleep: S,
    should_wake: W,
    list_token: ListToken<'a, IntrusiveNode<T>>,
    token_on_queue: bool,
}

unsafe impl<T, S, W> Send for Poller<'_, T, S, W>
where
    S: Fn(usize) -> bool + Send,
    W: Fn() -> bool + Send,
    T: Send,
{
}

impl<T, S, W> Drop for Poller<'_, T, S, W>
where
    S: Fn(usize) -> bool + Send,
    W: Fn() -> bool + Send,
{
    fn drop(&mut self) {
        unsafe {
            self.list_token.set_off_queue();
        }
        if self.token_on_queue && !self.list_token.revoke() {
            // Our waker was popped of the queue but we didn't get polled. This means the notification was lost
            // We have to blindly attempt to wake someone else. This could be expensive but should be rare!
            self.event.notify_one();
        }
    }
}

impl<T, S, W> core::future::Future for Poller<'_, T, S, W>
where
    S: Fn(usize) -> bool + Send,
    W: Fn() -> bool + Send,
{
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = unsafe { self.get_unchecked_mut() };
        if this.token_on_queue {
            this.token_on_queue = false;
            if (this.should_wake)() {
                return Poll::Ready(());
            }
            if !this.list_token.revoke() && (this.should_wake)() {
                // our token has already been popped meaning we are about to be woken anyway. Since shoudl wake is true we can just return ready
                return Poll::Ready(());
            }
        }
        this.list_token
            .inner()
            .handle
            .replace_waker(cx.waker().clone());

        let pinned_token = unsafe { Pin::new_unchecked(&this.list_token) };
        let did_push = pinned_token.push_if(&this.will_sleep);
        if did_push {
            this.token_on_queue = true;
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    }
}

#[derive(Debug, Default)]
pub struct Event {
    inner: TaggedEvent<()>,
}

impl Event {
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
        condition: impl Fn(usize) -> bool,
        on_empty: impl Fn(),
    ) -> usize {
        let condition = |num_waiting, _: &_| condition(num_waiting);
        self.inner.notify_all_while(condition, on_empty)
    }

    pub fn notify_many_while(
        &self,
        max: usize,
        condition: impl Fn(usize) -> bool,
        on_empty: impl Fn(),
    ) -> usize {
        let condition = |num_waiting, _: &_| condition(num_waiting);
        self.inner.notify_many_while(max, condition, on_empty)
    }

    pub fn notify_if(&self, condition: impl Fn(usize) -> bool, on_empty: impl Fn()) -> bool {
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
        S: Fn(usize) -> bool + Send,
        W: Fn() -> bool + Send,
    {
        self.inner.wait_while_async(will_sleep, should_wake, ())
    }

    pub fn wait_once(&self, will_sleep: impl Fn(usize) -> bool) {
        self.inner.wait_once(will_sleep, ());
    }

    pub fn wait_while(&self, should_sleep: impl Fn(usize) -> bool, should_wake: impl Fn() -> bool) {
        self.inner.wait_while(should_sleep, should_wake, ());
    }

    pub fn wait_while_until(
        &self,
        should_sleep: impl Fn(usize) -> bool,
        should_wake: impl Fn() -> bool,
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
            let poller = pin!(event.wait_while_async(|_| true, || true));
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
            let poller = pin!(event.wait_while_async(|_| true, || true));
            let mut polling = poller.polling();
            let result = polling.poll_once().await;
            assert_eq!(result, Poll::Pending);
            assert_eq!(event.inner.queue.len(), 1);
            event
                .inner
                .queue
                .pop_if(|_, _| Some(()), || panic!("shouldn't be empty"));
            second_entry = Box::pin(event.wait_while_async(|_| true, || true));
            second_polling = second_entry.polling();
            let result = second_polling.poll_once().await;
            assert_eq!(result, Poll::Pending);
            assert_eq!(event.inner.queue.len(), 1);
        }
        let result = second_polling.poll_once().await;
        assert_eq!(result, Poll::Ready(()));
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
