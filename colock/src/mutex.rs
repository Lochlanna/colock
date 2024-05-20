use crate::raw_mutex::RawMutex;
use core::cell::Cell;
use core::fmt::{Debug, Formatter};
use core::ops::{Deref, DerefMut};
use core::pin::Pin;
use core::task::{Context, Poll};

pub type MutexGuard<'a, T> = lock_api::MutexGuard<'a, RawMutex, T>;
pub type Mutex<T> = lock_api::Mutex<RawMutex, T>;

pub trait AsyncMutex {
    type InnerType: ?Sized;
    fn lock_async(
        &self,
    ) -> impl core::future::Future<Output = MutexGuard<'_, Self::InnerType>> + Send;
}
impl<T> AsyncMutex for Mutex<T>
where
    T: ?Sized,
{
    type InnerType = T;

    /// Asynchronously acquires the lock.
    ///
    /// If the lock is already held, the current task will be queued and awoken when the lock is
    /// locked.
    ///
    /// It is safe to drop the returned future at any time, even once its been polled.
    ///
    /// # Examples
    /// ```rust
    ///# use colock::mutex::Mutex;
    ///# use colock::mutex::AsyncMutex;
    ///# tokio_test::block_on(async {
    /// let mutex = Mutex::new(42);
    /// let guard = mutex.lock_async().await;
    /// assert_eq!(*guard, 42);
    ///# });
    /// ```
    ///
    /// ## Timeout
    /// ```rust
    ///# use tokio::select;
    /// use colock::mutex::Mutex;
    /// use colock::mutex::AsyncMutex;
    ///# tokio_test::block_on(async {
    /// let mutex = Mutex::new(());
    /// let _guard = mutex.lock();
    /// //mutex is already locked so this call should timeout
    /// select! {
    ///   _ = mutex.lock_async() => { panic!("should have timed out")},
    ///  _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => { assert!(mutex.is_locked())}
    /// }
    ///# });
    /// ```

    fn lock_async(&self) -> impl core::future::Future<Output = MutexGuard<'_, T>> + Send {
        if let Some(guard) = self.try_lock() {
            return AsyncLockFut::Complete(Cell::new(Some(guard)));
        }
        let make_guard = || unsafe { self.make_guard_unchecked() };
        unsafe { AsyncLockFut::Pending(self.raw().lock_async(), make_guard) }
    }
}

pub enum AsyncLockFut<'a, T, F, G>
where
    T: ?Sized,
{
    Complete(Cell<Option<MutexGuard<'a, T>>>),
    Pending(F, G),
}

unsafe impl<'a, T, F, G> Send for AsyncLockFut<'a, T, F, G> where T: ?Sized {}

impl<'a, T, F, G> core::future::Future for AsyncLockFut<'a, T, F, G>
where
    T: ?Sized,
    F: core::future::Future<Output = ()>,
    G: Fn() -> MutexGuard<'a, T>,
{
    type Output = MutexGuard<'a, T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = unsafe { self.get_unchecked_mut() };
        match this {
            AsyncLockFut::Complete(guard) => {
                Poll::Ready(guard.replace(None).expect("guard was None"))
            }
            AsyncLockFut::Pending(fut, mg) => {
                let fut = unsafe { Pin::new_unchecked(fut) };
                fut.poll(cx).map(|_| mg())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    #[derive(Eq, PartialEq, Debug)]
    struct NonCopy(i32);

    #[test]
    fn it_works_threaded() {
        let mutex = Mutex::new(());
        let barrier = std::sync::Barrier::new(2);
        let num_iterations = 10;
        thread::scope(|s| {
            s.spawn(|| {
                for _ in 0..num_iterations {
                    let guard = mutex.lock();
                    barrier.wait();
                    while unsafe { mutex.raw() }.queue().num_waiting() == 0 {
                        thread::yield_now();
                    }
                    thread::sleep(Duration::from_millis(5));
                    drop(guard);
                    barrier.wait();
                }
            });
            for _ in 0..num_iterations {
                barrier.wait();
                assert!(mutex.is_locked());
                let guard = mutex.lock();
                drop(guard);
                barrier.wait();
            }
        });
        assert!(!mutex.is_locked());
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn it_works_threaded_async() {
        let mutex = Mutex::new(());
        let barrier = tokio::sync::Barrier::new(2);
        let num_iterations = 10;
        tokio_scoped::scope(|s| {
            s.spawn(async {
                for _ in 0..num_iterations {
                    let guard = mutex.lock_async().await;
                    barrier.wait().await;
                    while unsafe { mutex.raw() }.queue().num_waiting() == 0 {
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    drop(guard);
                    barrier.wait().await;
                }
            });
            s.spawn(async {
                for _ in 0..num_iterations {
                    barrier.wait().await;
                    assert!(mutex.is_locked());
                    let guard = mutex.lock_async().await;
                    drop(guard);
                    barrier.wait().await;
                }
            });
        });
        assert!(!mutex.is_locked());
    }

    fn do_lots_and_lots(j: u64, k: u64) {
        let m = Mutex::new(0_u64);

        thread::scope(|s| {
            for _ in 0..k {
                s.spawn(|| {
                    for _ in 0..j {
                        *m.lock() += 1;
                    }
                });
            }
        });

        assert_eq!(*m.lock(), j * k);
    }

    async fn do_lots_and_lots_async(j: u64, k: u64) {
        let m = Mutex::new(0_u64);

        tokio_scoped::scope(|s| {
            for _ in 0..k {
                s.spawn(async {
                    for _ in 0..j {
                        *m.lock_async().await += 1;
                    }
                });
            }
        });

        assert_eq!(*m.lock_async().await, j * k);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn lots_and_lots() {
        const J: u64 = 1_000_000;
        // const J: u64 = 5000000;
        // const J: u64 = 50000000;
        const K: u64 = 6;
        do_lots_and_lots(J, K);
    }

    #[test]
    fn lots_and_lots_miri() {
        const J: u64 = 400;
        const K: u64 = 5;

        do_lots_and_lots(J, K);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[cfg_attr(miri, ignore)]
    async fn lots_and_lots_async() {
        const J: u64 = 100_000;
        // const J: u64 = 5000000;
        // const J: u64 = 50000000;
        const K: u64 = 6;
        do_lots_and_lots_async(J, K).await;
    }

    #[test]
    fn test_mutex_unsized() {
        let mutex: &Mutex<[i32]> = &Mutex::new([1, 2, 3]);
        {
            let b = &mut *mutex.lock();
            b[0] = 4;
            b[2] = 5;
        }
        let comp: &[i32] = &[4, 2, 5];
        assert_eq!(&*mutex.lock(), comp);
    }

    #[test]
    fn test_mutex_guard_sync() {
        fn sync<T: Sync>(_: T) {}

        let mutex = Mutex::new(());
        sync(mutex.lock());
    }

    #[test]
    fn test_get_mut() {
        let mut m = Mutex::new(NonCopy(10));
        *m.get_mut() = NonCopy(20);
        assert_eq!(m.into_inner(), NonCopy(20));
    }

    #[test]
    fn test_into_inner() {
        let m = Mutex::new(NonCopy(10));
        assert_eq!(m.into_inner(), NonCopy(10));
    }

    #[test]
    fn test_into_inner_drop() {
        struct Foo(Arc<AtomicUsize>);
        impl Drop for Foo {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let num_drops = Arc::new(AtomicUsize::new(0));
        let m = Mutex::new(Foo(num_drops.clone()));
        assert_eq!(num_drops.load(Ordering::SeqCst), 0);
        {
            let _inner = m.into_inner();
            assert_eq!(num_drops.load(Ordering::SeqCst), 0);
        }
        assert_eq!(num_drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_mutex_arc_access_in_unwind() {
        let arc = Arc::new(Mutex::new(1));
        let arc2 = arc.clone();
        let _ = thread::spawn(move || {
            struct Unwinder {
                i: Arc<Mutex<i32>>,
            }
            impl Drop for Unwinder {
                fn drop(&mut self) {
                    *self.i.lock() += 1;
                }
            }
            let _u = Unwinder { i: arc2 };
            panic!();
        })
        .join();
        let lock = arc.lock();
        assert_eq!(*lock, 2);
    }

    #[test]
    fn debug() {
        let mutex = Mutex::new(32);
        println!("{mutex:?}",);
    }
}
