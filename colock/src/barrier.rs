use crate::mutex::{Mutex, MutexGuard};
use std::cell::Cell;
use std::future::Future;
use std::pin::{pin, Pin};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::task::{Context, Poll};
use intrusive_list::{ConcurrentIntrusiveList, IntrusiveList, Node};
use parking::Parker;
use crate::lock_utils::MaybeAsyncWaker;

#[derive(Debug)]
pub struct Barrier {
    target: usize,
    wait_queue: ConcurrentIntrusiveList<MaybeAsyncWaker>,
    queue_lock: Mutex<usize>,
}

impl Barrier {
    #[must_use]
    pub const fn new(count: usize) -> Self {
        Self {
            target: count,
            wait_queue: ConcurrentIntrusiveList::new(),
            queue_lock: Mutex::new(0),
        }
    }
    
    fn wait_inner(&self, list: &mut IntrusiveList<MaybeAsyncWaker>, pinned_node: Pin<&mut Node<MaybeAsyncWaker>>) {
        let num_waiting = list.count() + 1;
        if num_waiting < self.target {
            //TODO handle the error here...
            unsafe {
                list.push_head(pinned_node.get_unchecked_mut(), &self.wait_queue);
            }
            
        } else {
            while let Some(waker) = list.pop_head() {
                waker.wake();
            }
        }
    }

    /// Increments the internal count and blocks the thread until the count reaches the target if it
    /// is not already at the target.
    pub fn wait(&self) {
        if self.target == 0 {
            return;
        }
        let parker = Parker::new();
        parker.prepare_park();
        let waker = MaybeAsyncWaker::Parker(parker.waker());

        let mut node = ConcurrentIntrusiveList::make_node(waker);
        let pinned_node = pin!(node);
        
        self.wait_queue.with_lock(|list| {
            self.wait_inner(list, pinned_node);
        });
    }

    /// Increments the internal count and waits until the count reaches the target if it
    /// is not already at the target.
    pub async fn wait_async(&self) {
        todo!();
        // if self.target == 0 {
        //     return;
        // }
        // // we use an atomic here rather than a cell just to appease async send bounds. It doesn't
        // // actually get run from multiple threads. This means relaxed ordering is always fine
        // let wake_all = AtomicBool::new(false);
        // let mut guard = Some(self.queue_lock.lock_async().await);
        // let expected_generation = self.generation.load(Ordering::Relaxed);
        // self.wait_queue
        //     .wait_while_async(
        //         |_| {
        //             if let Some(num_waiting) = guard.as_mut() {
        //                 **num_waiting += 1;
        //                 if **num_waiting == self.target {
        //                     wake_all.store(true, Ordering::Relaxed);
        //                     return false;
        //                 }
        //             }
        //             // we have to drop the guard here as returning true will put this thread to sleep
        //             guard = None;
        //             true
        //         },
        //         || self.generation.load(Ordering::Acquire) > expected_generation,
        //     )
        //     .await;
        // if wake_all.load(Ordering::Relaxed) {
        //     self.wake_all(&mut guard);
        // }
    }
}

pub struct BarrierPoller<'a> {
    barrier: &'a Barrier,
    node: Option<Node<MaybeAsyncWaker>>
}

unsafe impl<'a> Send for BarrierPoller<'a> {}

impl<'a> Future for BarrierPoller<'a> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.node.is_some() {
            return Poll::Ready(())
        }
        
        let self_mut = unsafe {self.get_unchecked_mut()};
        let waker = MaybeAsyncWaker::Waker(cx.waker().clone());
        let node = self_mut.node.insert(ConcurrentIntrusiveList::make_node(waker));
        let node = unsafe {Pin::new_unchecked(node)};
        
        self_mut.barrier.wait_queue.with_lock(|list| {
           self_mut.barrier.wait_inner(list, node); 
        });
        
        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;
    use tokio::select;

    #[test]
    fn test_barrier() {
        let barrier = Arc::new(Barrier::new(2));
        let barrier_clone = barrier.clone();
        let handle = std::thread::spawn(move || {
            barrier.wait();
        });
        thread::sleep(Duration::from_millis(100));
        barrier_clone.wait();
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
    fn lots_and_lots() {
        let n = 20;
        let mut handles = Vec::with_capacity(n);
        let barrier = Arc::new(Barrier::new(n));
        for _ in 0..n {
            let c = Arc::clone(&barrier);
            // The same messages will be printed together.
            // You will NOT see any interleaving.
            handles.push(thread::spawn(move || {
                c.wait();
            }));
        }
        // Wait for other threads to finish.
        for handle in handles {
            handle.join().unwrap();
        }
        assert!(!barrier.queue_lock.is_locked());
    }
}
