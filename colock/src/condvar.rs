use crate::mutex::MutexGuard;
use crate::raw_mutex::RawMutex;
use event::Event;
use lock_api::RawMutex as LockApiRawMutex;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct Condvar {
    wait_queue: Event,
    current_mutex: AtomicPtr<RawMutex>,
}

impl Condvar {
    pub const fn new() -> Self {
        Self {
            wait_queue: Event::new(),
            current_mutex: AtomicPtr::new(null_mut()),
        }
    }

    pub fn notify_one(&self) -> bool {
        self.wait_queue.notify_if(
            |num_left| {
                if num_left == 1 {
                    self.current_mutex.store(null_mut(), Ordering::Relaxed);
                }
                true
            },
            || {
                self.current_mutex.store(null_mut(), Ordering::Relaxed);
            },
        )
    }

    pub fn notify_all(&self) -> usize {
        self.wait_queue.notify_all_while(
            |num_left| {
                if num_left == 1 {
                    self.current_mutex.store(null_mut(), Ordering::Relaxed);
                }
                true
            },
            || {
                self.current_mutex.store(null_mut(), Ordering::Relaxed);
            },
        )
    }

    pub fn notify_many(&self, max: usize) -> usize {
        self.wait_queue.notify_many_while(
            max,
            |num_left| {
                if num_left == 1 {
                    self.current_mutex.store(null_mut(), Ordering::Relaxed);
                }
                true
            },
            || {
                self.current_mutex.store(null_mut(), Ordering::Relaxed);
            },
        )
    }

    const fn will_sleep<'a>(&'a self, mutex: &'a RawMutex) -> impl Fn(usize) -> bool + 'a {
        move |_| {
            let current_mutex = self
                .current_mutex
                .swap(mutex as *const _ as *mut _, Ordering::Relaxed)
                .cast_const();
            assert!(
                current_mutex.is_null() || current_mutex == mutex,
                "Condvar wait called with two different mutexes"
            );
            debug_assert!(mutex.is_locked());
            unsafe {
                mutex.unlock();
            }
            true
        }
    }

    pub fn wait<T: ?Sized>(&self, guard: &mut MutexGuard<'_, T>) {
        let mutex = guard.get_raw_mutex();
        debug_assert!(mutex.is_locked());
        self.wait_queue.wait_once(self.will_sleep(mutex));
        mutex.lock();
    }

    pub fn wait_until<T: ?Sized>(&self, guard: &mut MutexGuard<'_, T>, timeout: Instant) -> bool {
        todo!("Condvar::wait_until")
    }

    pub fn wait_for<T: ?Sized>(&self, guard: &mut MutexGuard<'_, T>, timeout: Duration) -> bool {
        todo!("Condvar::wait_for")
    }

    pub fn wait_while<T: ?Sized>(
        &self,
        guard: &mut MutexGuard<'_, T>,
        condition: impl FnMut(&mut T) -> bool,
    ) {
        todo!("Condvar::wait_while")
    }

    pub fn wait_while_until<T: ?Sized>(
        &self,
        guard: &mut MutexGuard<'_, T>,
        condition: impl FnMut(&mut T) -> bool,
        timeout: Instant,
    ) -> bool {
        todo!("Condvar::wait_while_until")
    }

    pub fn wait_while_for<T: ?Sized>(
        &self,
        guard: &mut MutexGuard<'_, T>,
        condition: impl FnMut(&mut T) -> bool,
        timeout: Duration,
    ) -> bool {
        todo!("Condvar::wait_while_for")
    }

    pub async fn async_wait<T: ?Sized>(&self, guard: &mut MutexGuard<'_, T>) {
        let mutex = guard.get_raw_mutex();
        debug_assert!(mutex.is_locked());
        self.wait_queue
            .wait_while_async(self.will_sleep(mutex), || true)
            .await;
        mutex.lock_async().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mutex::Mutex;
    use std::sync::Arc;

    #[test]
    fn test_condvar() {
        static CONDVAR: Condvar = Condvar::new();
        let mutex = Arc::new(Mutex::new(()));
        let mutex2 = mutex.clone();
        let mut guard = mutex.lock();
        let handle = std::thread::spawn(move || {
            let _guard = mutex2.lock();
            CONDVAR.notify_one();
        });
        CONDVAR.wait(&mut guard);
        handle.join().unwrap();
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn async_test_condvar() {
        static CONDVAR: Condvar = Condvar::new();
        let mutex = Arc::new(Mutex::new(()));
        let mutex2 = mutex.clone();
        let mut guard = mutex.lock();
        let handle = tokio::spawn(async move {
            let _guard = mutex2.lock_async().await;
            CONDVAR.notify_one();
        });
        CONDVAR.async_wait(&mut guard).await;
        handle.await.unwrap();
    }
}
