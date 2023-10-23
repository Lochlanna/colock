use crate::raw_mutex::RawMutex;
use std::fmt::{Debug, Formatter};

pub type MutexGuard<'a, T> = lock_api::MutexGuard<'a, RawMutex, T>;

#[derive(Default)]
pub struct Mutex<T>
where
    T: ?Sized,
{
    inner: lock_api::Mutex<RawMutex, T>,
}

impl<T> Debug for Mutex<T>
where
    T: Debug,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.inner.fmt(f)
    }
}

impl<T> Mutex<T> {
    pub const fn new(val: T) -> Self {
        Self {
            inner: lock_api::Mutex::<RawMutex, T>::const_new(RawMutex::new(), val),
        }
    }
    pub const fn const_new(val: T) -> Self {
        Self::new(val)
    }
    pub fn into_inner(self) -> T {
        self.inner.into_inner()
    }
}

impl<T> Mutex<T>
where
    T: ?Sized,
{
    #![allow(clippy::inline_always)]

    /// Creates a new `MutexGuard` without checking if the mutex is locked.
    ///
    /// # Safety
    ///
    /// This method must only be called if the thread logically holds the lock.
    ///
    /// Calling this function when a guard has already been produced is undefined behaviour unless
    /// the guard was forgotten with `mem::forget`.
    #[inline(always)]
    pub unsafe fn make_guard_unchecked(&self) -> MutexGuard<'_, T> {
        self.inner.make_guard_unchecked()
    }

    /// Acquires a mutex, blocking the current thread until it is able to do so.
    ///
    /// This function will block the local thread until it is available to acquire
    /// the mutex. Upon returning, the thread is the only thread with the mutex
    /// held. An RAII guard is returned to allow scoped unlock of the lock. When
    /// the guard goes out of scope, the mutex will be unlocked.
    ///
    /// Attempts to lock a mutex in the thread which already holds the lock will
    /// result in a deadlock.
    #[inline(always)]
    pub fn lock(&self) -> MutexGuard<'_, T> {
        self.inner.lock()
    }

    /// Attempts to acquire this lock.
    ///
    /// If the lock could not be acquired at this time, then `None` is returned.
    /// Otherwise, an RAII guard is returned. The lock will be unlocked when the
    /// guard is dropped.
    ///
    /// This function does not block.
    #[inline(always)]
    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        self.inner.try_lock()
    }

    /// Returns a mutable reference to the underlying data.
    ///
    /// Since this call borrows the `Mutex` mutably, no actual locking needs to
    /// take place---the mutable borrow statically guarantees no locks exist.
    #[inline(always)]
    pub fn get_mut(&mut self) -> &mut T {
        self.inner.get_mut()
    }

    /// Checks whether the mutex is currently locked.
    #[inline(always)]
    pub fn is_locked(&self) -> bool {
        self.inner.is_locked()
    }

    /// Forcibly unlocks the mutex.
    ///
    /// This is useful when combined with `mem::forget` to hold a lock without
    /// the need to maintain a `MutexGuard` object alive, for example when
    /// dealing with FFI.
    ///
    /// # Safety
    ///
    /// This method must only be called if the current thread logically owns a
    /// `MutexGuard` but that guard has been discarded using `mem::forget`.
    /// Behavior is undefined if a mutex is unlocked when not locked.
    #[inline(always)]
    pub unsafe fn force_unlock(&self) {
        self.inner.force_unlock();
    }

    /// Returns the underlying raw mutex object.
    ///
    /// Note that you will most likely need to import the `RawMutex` trait from
    /// `lock_api` to be able to call functions on the raw mutex.
    ///
    /// # Safety
    ///
    /// This method is unsafe because it allows unlocking a mutex while
    /// still holding a reference to a `MutexGuard`.
    #[inline(always)]
    pub unsafe fn raw(&self) -> &RawMutex {
        self.inner.raw()
    }

    /// Returns a raw pointer to the underlying data.
    ///
    /// This is useful when combined with `mem::forget` to hold a lock without
    /// the need to maintain a `MutexGuard` object alive, for example when
    /// dealing with FFI.
    ///
    /// # Safety
    ///
    /// You must ensure that there are no data races when dereferencing the
    /// returned pointer, for example if the current thread logically owns
    /// a `MutexGuard` but that guard has been discarded using `mem::forget`.
    #[inline(always)]
    pub fn data_ptr(&self) -> *mut T {
        self.inner.data_ptr()
    }

    /// Forcibly unlocks the mutex using a fair unlock protocol.
    ///
    /// This is useful when combined with `mem::forget` to hold a lock without
    /// the need to maintain a `MutexGuard` object alive, for example when
    /// dealing with FFI.
    ///
    /// # Safety
    ///
    /// This method must only be called if the current thread logically owns a
    /// `MutexGuard` but that guard has been discarded using `mem::forget`.
    /// Behavior is undefined if a mutex is unlocked when not locked.
    #[inline(always)]
    pub unsafe fn force_unlock_fair(&self) {
        self.inner.force_unlock_fair();
    }

    /// Attempts to acquire this lock until a timeout is reached.
    ///
    /// If the lock could not be acquired before the timeout expired, then
    /// `None` is returned. Otherwise, an RAII guard is returned. The lock will
    /// be unlocked when the guard is dropped.
    #[inline(always)]
    pub fn try_lock_for(&self, timeout: std::time::Duration) -> Option<MutexGuard<'_, T>> {
        self.inner.try_lock_for(timeout)
    }

    /// Attempts to acquire this lock until a timeout is reached.
    ///
    /// If the lock could not be acquired before the timeout expired, then
    /// `None` is returned. Otherwise, an RAII guard is returned. The lock will
    /// be unlocked when the guard is dropped.
    #[inline(always)]
    pub fn try_lock_until(&self, timeout: std::time::Instant) -> Option<MutexGuard<'_, T>> {
        self.inner.try_lock_until(timeout)
    }

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
    #[inline]
    pub async fn lock_async(&self) -> MutexGuard<'_, T> {
        if let Some(guard) = self.inner.try_lock() {
            return guard;
        }
        unsafe {
            self.inner.raw().lock_async().await;
            self.inner.make_guard_unchecked()
        }
    }
}

impl<T> From<T> for Mutex<T> {
    fn from(value: T) -> Self {
        Self::new(value)
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
