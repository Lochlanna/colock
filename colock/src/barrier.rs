use crate::raw_mutex::RawMutex;
use event::Event;
use lock_api::RawMutex as LockApiRawMutex;
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug)]
pub struct Barrier {
    target: usize,
    wait_queue: Event,
    queue_lock: RawMutex,
}

impl Barrier {
    #[must_use]
    pub const fn new(count: usize) -> Self {
        Self {
            target: count,
            wait_queue: Event::new(),
            queue_lock: RawMutex::new(),
        }
    }

    pub fn wait(&self) {
        if self.target == 0 {
            return;
        }
        let wake_all = Cell::new(false);
        self.queue_lock.lock();
        self.wait_queue.wait_once(|num_waiting| {
            if num_waiting == self.target - 1 {
                wake_all.set(true);
                return false;
            }
            unsafe {
                self.queue_lock.unlock();
            }
            true
        });
        if wake_all.get() {
            debug_assert!(self.queue_lock.is_locked());
            // we know threads cannot be added to the queue while we hold the lock
            self.wait_queue.notify_all_while(|_| true, || {});
            unsafe {
                self.queue_lock.unlock();
            }
        }
    }

    pub async fn wait_async(&self) {
        if self.target == 0 {
            return;
        }
        let wake_all = AtomicBool::new(false);
        self.queue_lock.lock_async().await;
        self.wait_queue
            .wait_while_async(
                |num_waiting| {
                    if num_waiting == self.target - 1 {
                        wake_all.store(true, Ordering::Relaxed);
                        return false;
                    }
                    unsafe {
                        self.queue_lock.unlock();
                    }
                    true
                },
                || true,
            )
            .await;
        if wake_all.load(Ordering::Relaxed) {
            debug_assert!(self.queue_lock.is_locked());
            // we know threads cannot be added to the queue while we hold the lock
            self.wait_queue.notify_all_while(|_| true, || {});
            unsafe {
                self.queue_lock.unlock();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

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
                println!("before wait");
                c.wait();
                println!("after wait");
            }));
        }
        // Wait for other threads to finish.
        for handle in handles {
            handle.join().unwrap();
        }
        assert!(!barrier.queue_lock.is_locked());
    }
}
