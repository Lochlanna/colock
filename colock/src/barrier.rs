use std::future::Future;
use std::pin::{pin, Pin};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};
use intrusive_list::{ConcurrentIntrusiveList, IntrusiveList, Node};
use parking::{Parker, Waker};

#[derive(Debug)]
pub struct Barrier {
    target: usize,
    wait_queue: ConcurrentIntrusiveList<Waker>,
}

impl Barrier {
    #[must_use]
    pub const fn new(count: usize) -> Self {
        Self {
            target: count,
            wait_queue: ConcurrentIntrusiveList::new(),
        }
    }

    fn lock_inner(&self, list: &mut IntrusiveList<Waker>, node: Pin<&mut Node<Waker>>) -> bool {
        if list.count() == self.target - 1 {
            // we were the last ones wake everyone
            while let Some(waker) = list.pop_head() {
                waker.wake();
            }
            return false;
        }
        unsafe {
            list.push_head(node.get_unchecked_mut(), &self.wait_queue).expect("barrier node was dirty");
        }
        true
    }

    /// Increments the internal count and blocks the thread until the count reaches the target if it
    /// is not already at the target.
    pub fn wait(&self) {
        if self.target == 0 {
            return;
        }
        let parker = Parker::new();
        parker.prepare_park();
        let waker = parker.waker();
        let mut node = ConcurrentIntrusiveList::make_node(waker);
        let pinned_node = pin!(node);

        let should_sleep = self.wait_queue.with_lock(|list| {
            self.lock_inner(list, pinned_node)
        });
        // If should_sleep is true it means we got queued.
        if should_sleep {
            parker.park();
        }
    }

   pub const fn wait_async(&self) -> BarrierPoller{
       BarrierPoller {
           barrier: self,
           node: None,
           parker: None,
       }
   }
}

pub struct BarrierPoller<'a> {
    barrier: &'a Barrier,
    node: Option<Node<Waker>>,
    parker: Option<Parker>,
}


unsafe impl<'a> Send for BarrierPoller<'a> {}

impl<'a> Future for BarrierPoller<'a> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.barrier.target == 0 || self.parker.as_ref().map(Parker::should_wakeup) == Some(true) {
            return Poll::Ready(());
        }
        let self_mut = unsafe {self.get_unchecked_mut()};

        // This will cause any existing node to drop which will remove it from the list
        // this has to be done before a new parker is made because the waker has a pointer
        // to the parker. If we kill the parker, and then it gets woken before we add the new
        // waker to the list it will hit already deleted memory
        self_mut.node = None;

        let parker = self_mut.parker.insert(Parker::new());
        let node = ConcurrentIntrusiveList::make_node(parker.async_waker(cx.waker().clone()));
        let node = self_mut.node.insert(node);
        let node = unsafe {Pin::new_unchecked(node)};

        let should_sleep = self_mut.barrier.wait_queue.with_lock(|list| {
            self_mut.barrier.lock_inner(list, node)
        });
        if should_sleep {
            return Poll::Pending;
        }
        Poll::Ready(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};
    use tokio::select;

    #[test]
    fn test_barrier() {
        let barrier = Arc::new(Barrier::new(2));
        let barrier_clone = barrier.clone();
        let handle = std::thread::spawn(move || {
            barrier.wait();
            let start = Instant::now();
            barrier.wait();
            assert!(start.elapsed() > Duration::from_millis(100));
        });
        barrier_clone.wait();
        thread::sleep(Duration::from_millis(110));
        assert_eq!(barrier_clone.wait_queue.count(), 1);
        barrier_clone.wait();
        assert_eq!(barrier_clone.wait_queue.count(), 0);
        handle.join().unwrap();
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn async_test_barrier() {
        let barrier = Arc::new(Barrier::new(2));
        let barrier_clone = barrier.clone();
        let handle = tokio::spawn(async move {
            barrier.wait_async().await;
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        barrier_clone.wait_async().await;
        handle.await.unwrap();
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn async_test_barrier_cancel() {
        let barrier = Arc::new(Barrier::new(2));
        let mut timer_triggered = false;
        select! {
            _ = barrier.wait_async() => {
                panic!("The barrier should not complete")
            }
            _ = tokio::time::sleep(Duration::from_millis(50)) => {
                timer_triggered = true;
            }
        }
        assert!(timer_triggered);
        assert_eq!(barrier.wait_queue.count(), 0);
    }

    #[test]
    fn stress_test_barrier() {
        const THREAD_COUNT: usize = 100; // Number of threads
        const WAIT_COUNT: usize = 100;  // Number of times each thread calls `wait`

        let barrier = Arc::new(Barrier::new(THREAD_COUNT));
        let mut handles = Vec::new();

        // Shared counter to track the number of waits completed across all threads
        let shared_counter = Arc::new(AtomicUsize::new(0));

        for _ in 0..THREAD_COUNT {
            let barrier_clone = Arc::clone(&barrier);
            let counter_clone = Arc::clone(&shared_counter);

            let handle = thread::spawn(move || {
                for _ in 0..WAIT_COUNT {
                    // Simulate some work before reaching the barrier
                    thread::sleep(Duration::from_millis(1));

                    // Increment the counter before waiting
                    counter_clone.fetch_add(1, Ordering::Relaxed);

                    // Wait at the barrier
                    barrier_clone.wait();
                }
            });

            handles.push(handle);
        }

        // Wait for all threads to complete
        for handle in handles {
            handle.join().unwrap();
        }

        // Verify the total number of wait operations is correct
        assert_eq!(
            shared_counter.load(Ordering::Relaxed),
            THREAD_COUNT * WAIT_COUNT
        );
    }
}
