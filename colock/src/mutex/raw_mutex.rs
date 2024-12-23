use core::sync::atomic::{AtomicU8, Ordering};
use std::future::Future;
use std::pin::{pin, Pin};
use std::task::{Context, Poll};
use intrusive_list::{ConcurrentIntrusiveList, Error, Node};
use parking::{Parker, Waker};

const UNLOCKED: u8 = 0;
const LOCKED_BIT: u8 = 0b1;
const WAIT_BIT: u8 = 0b10;
const FAIR_BIT: u8 = 0b100;


#[derive(Debug)]
pub struct RawMutex {
    queue: ConcurrentIntrusiveList<Waker>,
    state: AtomicU8,
}

impl Default for RawMutex {
    fn default() -> Self {
        Self::new()
    }
}

impl RawMutex {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            queue: ConcurrentIntrusiveList::new(),
            state: AtomicU8::new(UNLOCKED),
        }
    }

    /// Returns a reference to the queue used by this mutex.
    /// This is for testing hence the pub(crate) visibility.
    pub(crate) const fn queue(&self) -> &ConcurrentIntrusiveList<Waker> {
        &self.queue
    }

    pub const fn lock_async(&self) -> RawMutexPoller {
        RawMutexPoller {
            raw_mutex: self,
            node: None,
        }
    }

    pub fn lock(&self) {
        self.lock_inner(None);
    }

    fn lock_inner(&self, timeout: Option<&dyn crate::lock_utils::Timeout>) -> bool {
        loop {
            if self.try_lock() {
                return true;
            }
            let parker = Parker::new();
            parker.prepare_park();
            let waker = parker.waker();

            let mut node = ConcurrentIntrusiveList::make_node(waker);
            let pinned_node = pin!(node);

            let (park_result, _) = self.queue.push_head(pinned_node, |_, _|{
                self.should_sleep()
            });

            if let Err(park_error) = park_result {
                if park_error == Error::AbortedPush {
                    //we got the lock
                    return true;
                }
                panic!("The node was dirty");
            }
            if let Some(timeout) = &timeout {
                if !parker.park_until(timeout.to_instant()) {
                    return false;
                }
            } else {
                parker.park();
            }

            if self.state.load(Ordering::Acquire) & FAIR_BIT != 0 {
                // it was fairly handed to us!
                self.state.fetch_and(!FAIR_BIT, Ordering::Relaxed);
                return true;
            }
        }
    }

    fn should_sleep(&self) -> (bool, ()) {
        let mut current_state = self.state.load(Ordering::Acquire);
        while current_state != (LOCKED_BIT | WAIT_BIT) {
            if current_state & LOCKED_BIT == 0 {
                if let Err(new_state) = self.state.compare_exchange(current_state, current_state | LOCKED_BIT, Ordering::Acquire, Ordering::Acquire) {
                    current_state = new_state;
                } else {
                    // we got the lock abort the push
                    return (false, ());
                }
            } else {
                debug_assert!(current_state == LOCKED_BIT);
                current_state = self.state.compare_exchange(LOCKED_BIT, LOCKED_BIT | WAIT_BIT, Ordering::AcqRel, Ordering::Acquire).unwrap_or_else(|u| u);
            }
        }
        (true, ())
    }

    #[inline]
    pub fn try_lock(&self) -> bool {
        let Err(mut state) = self.state.compare_exchange(UNLOCKED, LOCKED_BIT, Ordering::Acquire, Ordering::Relaxed) else {
            return true;
        };
        while state & LOCKED_BIT == 0 {
            if let Err(new_state) = self.state.compare_exchange(state, state | LOCKED_BIT, Ordering::Acquire, Ordering::Relaxed) {
                state = new_state;
            } else {
                return true;
            }
        }
        false
    }

    #[inline]
    pub unsafe fn unlock(&self) {
        let state = self.state.fetch_and(!LOCKED_BIT, Ordering::Release);
        if state & WAIT_BIT == 0 {
            return;
        }
        // there is a thread waiting!
        self.queue.pop_tail(|waker, num_waiting|{
            waker.wake();
            if num_waiting == 0 {
                self.state.fetch_and(!WAIT_BIT, Ordering::Release);
            }
        }, ||{});
    }

    pub fn is_locked(&self) -> bool {
        self.state.load(Ordering::Acquire) & LOCKED_BIT == LOCKED_BIT
    }

    pub unsafe fn unlock_fair(&self) {
        // there is a thread waiting!
        self.queue.pop_tail(|waker, num_waiting|{
            if num_waiting == 0 {
                debug_assert!(self.state.load(Ordering::Acquire) & WAIT_BIT == WAIT_BIT);
                self.state.fetch_xor(WAIT_BIT | FAIR_BIT, Ordering::Release);
            } else {
                self.state.fetch_or(FAIR_BIT, Ordering::Relaxed);
            }
            waker.wake();
        }, || { self.state.fetch_and(!LOCKED_BIT, Ordering::Release); });
    }

    pub unsafe fn bump(&self) {
        if self.state.load(Ordering::Acquire) == (LOCKED_BIT | WAIT_BIT) {
            self.unlock();
            self.lock();
        }
    }

    pub fn try_lock_for(&self, timeout: core::time::Duration) -> bool {
        self.lock_inner(Some(&timeout))
    }

    pub fn try_lock_until(&self, timeout: std::time::Instant) -> bool {
        self.lock_inner(Some(&timeout))
    }
}


pub struct RawMutexPoller<'a> {
    raw_mutex: &'a RawMutex,
    node:Option<Node<Waker>>
}
unsafe impl<'a> Send for RawMutexPoller<'a> {}
impl<'a> Future for RawMutexPoller<'a> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.raw_mutex.try_lock() {
            return Poll::Ready(())
        }
        let self_mut = unsafe {self.get_unchecked_mut()};
        let waker = Waker::new_task_unattached(cx.waker().clone());
        let node = self_mut.node.insert(ConcurrentIntrusiveList::make_node(waker));
        let node = unsafe {Pin::new_unchecked(node)};

        let (park_result, _) = self_mut.raw_mutex.queue.push_head(node, |_, _|{
            self_mut.raw_mutex.should_sleep()
        });

        if let Err(park_error) = park_result {
            if park_error == Error::AbortedPush {
                //we got the lock
                return Poll::Ready(());
            }
            panic!("The node was dirty");
        }

        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn it_works_threaded() {
        let mutex = RawMutex::new();
        let barrier = std::sync::Barrier::new(2);
        let num_iterations = 10;
        thread::scope(|s| {
            s.spawn(|| {
                for _ in 0..num_iterations {
                    mutex.lock();
                    barrier.wait();
                    while mutex.queue.count() == 0 {
                        thread::yield_now();
                    }
                    thread::sleep(Duration::from_millis(5));
                    unsafe {
                        mutex.unlock();
                    }
                    barrier.wait();
                }
            });
            for _ in 0..num_iterations {
                barrier.wait();
                assert!(mutex.is_locked());
                mutex.lock();
                unsafe {
                    mutex.unlock();
                }
                barrier.wait();
            }
        });
        assert!(!mutex.is_locked());
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn async_lock() {
        let mutex = RawMutex::new();
        mutex.lock_async().await;
        assert!(mutex.is_locked());
        unsafe {
            mutex.unlock();
        }
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn it_works_threaded_async() {
        let mutex = RawMutex::new();
        let barrier = tokio::sync::Barrier::new(2);
        let num_iterations = 10;
        tokio_scoped::scope(|s| {
            s.spawn(async {
                for _ in 0..num_iterations {
                    mutex.lock_async().await;
                    barrier.wait().await;
                    while mutex.queue.count() == 0 {
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    unsafe {
                        mutex.unlock();
                    }
                    barrier.wait().await;
                }
            });
            s.spawn(async {
                for _ in 0..num_iterations {
                    barrier.wait().await;
                    assert!(mutex.is_locked());
                    mutex.lock_async().await;
                    unsafe {
                        mutex.unlock();
                    }
                    barrier.wait().await;
                }
            });
        });
        assert!(!mutex.is_locked());
    }

    #[test]
    fn lock_timeout() {
        let mutex = RawMutex::new();
        mutex.lock();
        let start = Instant::now();
        let did_lock = mutex.try_lock_for(Duration::from_millis(150));
        assert!(!did_lock);
        let elapsed = start.elapsed().as_millis();
        assert!(elapsed >= 150);
        unsafe {
            mutex.unlock();
        }

        let start = Instant::now();
        let did_lock = mutex.try_lock_for(Duration::from_millis(150));
        assert!(did_lock);
        assert!(start.elapsed().as_millis() < 10);
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn async_lock_timeout() {
        let mutex = RawMutex::new();
        mutex.lock_async().await;
        let start = Instant::now();
        let did_lock;
        tokio::select! {
            () = mutex.lock_async() => {
                did_lock = true;
            }
            () = tokio::time::sleep(Duration::from_millis(150)) => {
                assert_eq!(mutex.queue.count(), 0);
                did_lock = false;
            }
        }
        assert!(!did_lock);
        assert!(start.elapsed().as_millis() >= 150);
        unsafe {
            mutex.unlock();
        }
    }
}
