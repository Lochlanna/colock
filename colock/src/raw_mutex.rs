use crate::spinwait;
use core::sync::atomic::{AtomicU8, Ordering};
use std::ops::{Add, Deref};
use std::pin::pin;
use std::task::Waker;
use std::thread::park;
use std::time::{Duration, Instant};
use intrusive_list::{ConcurrentIntrusiveList, Error};
use parking::{Parker, ThreadParker, ThreadParkerT, ThreadWaker};

const UNLOCKED: u8 = 0;
const LOCKED_BIT: u8 = 0b1;
const WAIT_BIT: u8 = 0b10;
const FAIR_BIT: u8 = 0b100;


trait Timeout {
    fn to_instant(&self)->Instant;
}

impl Timeout for Instant {
    fn to_instant(&self) -> Instant {
        self.clone()
    }
}

impl Timeout for Duration {
    fn to_instant(&self) -> Instant {
        Instant::now().add(self.clone())
    }
}

//TODO naming here is weird...
#[derive(Debug)]
pub enum MaybeAsync {
    Parker(ThreadWaker),
    Waker(Waker)
}

impl MaybeAsync {
    fn wake(self) {
        match self {
            MaybeAsync::Parker(p) => p.wake(),
            MaybeAsync::Waker(w) => w.wake()
        }
    }
}

#[derive(Debug)]
pub struct RawMutex {
    queue: ConcurrentIntrusiveList<MaybeAsync>,
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
    pub(crate) const fn queue(&self) -> &ConcurrentIntrusiveList<MaybeAsync> {
        &self.queue
    }

    pub async fn lock_async(&self) {
        todo!()
    }

    fn get_parker()-> Parker {
        if ThreadParker::IS_CHEAP_TO_CONSTRUCT {
            return Parker::Owned(ThreadParker::const_new())
        }

        thread_local! {
            static HANDLE: ThreadParker = const {ThreadParker::const_new()}
        }
        HANDLE.with(|handle| {
            unsafe {Parker::Ref(core::mem::transmute(handle))}
        })
    }

    #[inline]
    pub fn lock(&self) {
        self.lock_inner::<Instant>(None);
    }


    fn lock_inner<T:Timeout>(&self, timeout: Option<T>) -> bool {
        loop {
            if self.try_lock() {
                return true;
            }
            let parker = Self::get_parker();
            parker.prepare_park();
            let waker = MaybeAsync::Parker(parker.waker());

            let mut node = ConcurrentIntrusiveList::make_node(waker);
            let pinned_node = pin!(node);

            let (park_result, _) = self.queue.push_head(pinned_node, |_, _|{
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
                        current_state = self.state.compare_exchange(LOCKED_BIT, LOCKED_BIT | WAIT_BIT, Ordering::AcqRel, Ordering::Acquire).unwrap_or_else(|u|u);
                    }
                }
                (true, ())
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
            self.lock()
        }
    }

    pub fn try_lock_for(&self, timeout: core::time::Duration) -> bool {
        self.lock_inner(Some(timeout))
    }

    pub fn try_lock_until(&self, timeout: std::time::Instant) -> bool {
        self.lock_inner(Some(timeout))
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
        assert!(start.elapsed().as_millis() >= 150);
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
