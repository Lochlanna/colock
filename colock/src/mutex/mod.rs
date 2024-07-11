mod guard;

use crate::raw_mutex::RawMutex;
use std::fmt::Debug;
use std::ops::DerefMut;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

pub use guard::*;

#[derive(Debug)]
pub struct Mutex<T: ?Sized> {
    raw_mutex: RawMutex,
    data: T,
}

impl<T> Default for Mutex<T>
where
    T: Default + ?Sized,
{
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T> From<T> for Mutex<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

unsafe impl<T> Send for Mutex<T> where T: Send + ?Sized {}
unsafe impl<T> Sync for Mutex<T> where T: Send + ?Sized {}

impl<T> Mutex<T> {
    pub const fn new(data: T) -> Self {
        Self {
            data,
            raw_mutex: RawMutex::new(),
        }
    }

    pub const fn from_raw(raw_mutex: RawMutex, data: T) -> Self {
        Self { data, raw_mutex }
    }

    pub fn into_inner(self) -> T {
        self.data
    }
}

impl<T> Mutex<T>
where
    T: ?Sized,
{
    pub fn data_ptr(&self) -> *mut T {
        core::ptr::from_ref(&self.data).cast_mut()
    }

    pub unsafe fn force_unlock(&self) {
        self.raw_mutex.unlock()
    }

    pub unsafe fn force_unlock_fair(&self) {
        self.raw_mutex.unlock_fair()
    }

    pub fn get_mut(&mut self) -> &mut T {
        &mut self.data
    }

    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        self.raw().try_lock().then(|| self.make_guard())
    }

    pub fn try_lock_for(&self, duration: Duration) -> Option<MutexGuard<'_, T>> {
        self.raw().try_lock_for(duration).then(|| self.make_guard())
    }

    pub fn try_lock_until(&self, timeout: Instant) -> Option<MutexGuard<'_, T>> {
        self.raw()
            .try_lock_until(timeout)
            .then(|| self.make_guard())
    }

    pub fn try_lock_arc(self: &Arc<Self>) -> Option<ArcMutexGuard<T>> {
        self.raw().try_lock().then(|| self.make_arc_guard())
    }

    pub fn try_lock_arc_for(self: &Arc<Self>, duration: Duration) -> Option<ArcMutexGuard<T>> {
        self.raw()
            .try_lock_for(duration)
            .then(|| self.make_arc_guard())
    }

    pub fn try_lock_arc_until(self: &Arc<Self>, timeout: Instant) -> Option<ArcMutexGuard<T>> {
        self.raw()
            .try_lock_until(timeout)
            .then(|| self.make_arc_guard())
    }

    pub fn lock(&self) -> MutexGuard<'_, T> {
        self.raw().lock();
        self.make_guard()
    }

    pub fn lock_arc(self: &Arc<Self>) -> ArcMutexGuard<T> {
        self.raw().lock();
        self.make_arc_guard()
    }

    pub fn raw(&self) -> &RawMutex {
        &self.raw_mutex
    }

    pub unsafe fn make_guard_unchecked(&self) -> MutexGuard<'_, T> {
        self.make_guard()
    }

    fn make_guard(&self) -> MutexGuard<'_, T> {
        unsafe { MutexGuard::new(self) }
    }

    pub unsafe fn make_arc_guard_unchecked(self: &Arc<Self>) -> ArcMutexGuard<T> {
        self.make_arc_guard()
    }

    fn make_arc_guard(self: &Arc<Self>) -> ArcMutexGuard<T> {
        unsafe { ArcMutexGuard::new(Arc::clone(self)) }
    }

    pub fn is_locked(&self) -> bool {
        self.raw().is_locked()
    }
}

impl<T> Mutex<T>
where
    T: ?Sized + Send + Sync,
{
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

    pub async fn lock_async(&self) -> MutexGuard<'_, T> {
        self.raw().lock_async().await;
        self.make_guard()
    }

    pub async fn lock_async_arc(self: &Arc<Self>) -> ArcMutexGuard<T> {
        self.raw().lock_async().await;
        self.make_arc_guard()
    }
}

trait IsMutex<T: ?Sized> {
    fn get_mutex(&self) -> &Mutex<T>;
    fn raw(&self) -> &RawMutex;
    fn data(&self) -> &T;
}

impl<T> IsMutex<T> for &Mutex<T>
where
    T: ?Sized,
{
    fn get_mutex(&self) -> &Mutex<T> {
        self
    }

    fn raw(&self) -> &RawMutex {
        &self.raw_mutex
    }

    fn data(&self) -> &T {
        &self.data
    }
}

impl<T> IsMutex<T> for Arc<Mutex<T>>
where
    T: ?Sized,
{
    fn get_mutex(&self) -> &Mutex<T> {
        Arc::as_ref(self)
    }

    fn raw(&self) -> &RawMutex {
        &self.raw_mutex
    }

    fn data(&self) -> &T {
        &self.data
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
