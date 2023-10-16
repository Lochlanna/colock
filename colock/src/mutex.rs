use crate::raw_mutex::RawMutex;
use lock_api;
use lock_api::{RawMutex as LockAPI, RawMutexFair, RawMutexTimed};
use std::cell::UnsafeCell;
use std::fmt::{Debug, Formatter};
use std::mem::forget;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

pub struct Mutex<T>
where
    T: ?Sized,
{
    raw: RawMutex,
    data: UnsafeCell<T>,
}

unsafe impl<T> Sync for Mutex<T> {}

impl<T> Default for Mutex<T>
where
    T: Default,
{
    fn default() -> Self {
        Self::new(Default::default())
    }
}

impl<T> Debug for Mutex<T>
where
    T: Debug,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        if let Some(guard) = self.try_lock() {
            f.debug_struct("Mutex").field("data", &&*guard).finish()
        } else {
            struct LockedPlaceholder;
            impl Debug for LockedPlaceholder {
                fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
                    f.write_str("<locked>")
                }
            }

            f.debug_struct("Mutex")
                .field("data", &LockedPlaceholder)
                .finish()
        }
    }
}

impl<T> Mutex<T> {
    pub const fn new(val: T) -> Self {
        Self {
            raw: RawMutex::new(),
            data: UnsafeCell::new(val),
        }
    }
    pub const fn const_new(val: T) -> Self {
        Self::new(val)
    }

    // Consumes this mutex, returning the underlying data.
    #[inline]
    pub fn into_inner(self) -> T {
        self.data.into_inner()
    }
}

impl<T> Mutex<T>
where
    T: ?Sized,
{
    pub fn lock(&self) -> MutexGuard<'_, T> {
        self.raw.lock();
        self.make_guard_unchecked()
    }

    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        if self.raw.try_lock() {
            Some(self.make_guard_unchecked())
        } else {
            None
        }
    }

    pub fn try_lock_until(&self, timeout: std::time::Instant) -> Option<MutexGuard<'_, T>> {
        if self.raw.try_lock_until(timeout) {
            Some(self.make_guard_unchecked())
        } else {
            None
        }
    }

    pub fn try_lock_for(&self, timeout: std::time::Duration) -> Option<MutexGuard<'_, T>> {
        if self.raw.try_lock_for(timeout) {
            Some(self.make_guard_unchecked())
        } else {
            None
        }
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
        if let Some(guard) = self.try_lock() {
            return guard;
        }
        unsafe {
            self.raw().lock_async().await;
            self.make_guard_unchecked()
        }
    }

    const fn make_guard_unchecked(&self) -> MutexGuard<'_, T> {
        MutexGuard { mutex: self }
    }

    fn make_arc_guard_unchecked(self: &Arc<Self>) -> ArcMutexGuard<T> {
        ArcMutexGuard {
            mutex: self.clone(),
        }
    }

    pub unsafe fn force_unlock(&self) {
        self.raw.unlock()
    }

    pub unsafe fn force_unlock_fair(&self) {
        self.raw.unlock_fair();
    }

    pub const fn raw(&self) -> &RawMutex {
        &self.raw
    }

    pub fn get_mut(&mut self) -> &mut T {
        self.data.get_mut()
    }

    pub fn is_locked(&self) -> bool {
        self.raw.is_locked()
    }

    pub fn is_unlocked(&self) -> bool {
        !self.is_locked()
    }

    pub const fn data_ptr(&self) -> *mut T {
        self.data.get()
    }

    pub fn lock_arc(self: &Arc<Self>) -> ArcMutexGuard<T> {
        self.raw.lock();
        self.make_arc_guard_unchecked()
    }

    pub fn try_lock_arc(self: &Arc<Self>) -> Option<ArcMutexGuard<T>> {
        if self.raw.try_lock() {
            Some(self.make_arc_guard_unchecked())
        } else {
            None
        }
    }

    pub fn try_lock_arc_until(
        self: &Arc<Self>,
        timeout: std::time::Instant,
    ) -> Option<ArcMutexGuard<T>> {
        if self.raw.try_lock_until(timeout) {
            Some(self.make_arc_guard_unchecked())
        } else {
            None
        }
    }

    pub fn try_lock_arc_for(
        self: &Arc<Self>,
        timeout: std::time::Duration,
    ) -> Option<ArcMutexGuard<T>> {
        if self.raw.try_lock_for(timeout) {
            Some(self.make_arc_guard_unchecked())
        } else {
            None
        }
    }
}

pub struct MutexGuard<'a, T>
where
    T: ?Sized,
{
    mutex: &'a Mutex<T>,
}

impl<T> Debug for MutexGuard<'_, T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        todo!()
    }
}

impl<T> Drop for MutexGuard<'_, T>
where
    T: ?Sized,
{
    fn drop(&mut self) {
        unsafe {
            self.mutex.force_unlock();
        }
    }
}

impl<T> Deref for MutexGuard<'_, T>
where
    T: ?Sized,
{
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.mutex.data.get() }
    }
}

impl<T> DerefMut for MutexGuard<'_, T>
where
    T: ?Sized,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.mutex.data.get() }
    }
}

pub trait Guard<'a> {
    type Data: ?Sized;
    fn mutex(guard: &'a Self) -> &'a Mutex<Self::Data>;
}

impl<'a, T> MutexGuard<'a, T>
where
    T: ?Sized,
{
    #[must_use]
    pub const fn mutex(guard: &Self) -> &'a Mutex<T> {
        guard.mutex
    }
    pub fn bump(guard: &Self) {
        unsafe {
            guard.mutex.raw.bump();
        }
    }
    pub fn leak(guard: Self) -> &'a mut T {
        let t = unsafe { &mut *guard.mutex.data.get() };
        forget(guard);
        t
    }

    pub fn unlocked<F, R>(guard: &Self, f: F) -> R
    where
        F: FnOnce() -> R,
    {
        unsafe {
            guard.mutex.force_unlock();
        }
        guard.unlocked_inner(f)
    }

    pub fn unlocked_fair<F, R>(guard: &Self, f: F) -> R
    where
        F: FnOnce() -> R,
    {
        unsafe {
            guard.mutex.force_unlock_fair();
        }
        guard.unlocked_inner(f)
    }
    fn unlocked_inner<F, R>(&self, f: F) -> R
    where
        F: FnOnce() -> R,
    {
        let r = f();
        self.mutex.lock();
        r
    }
}

pub struct ArcMutexGuard<T>
where
    T: ?Sized,
{
    mutex: Arc<Mutex<T>>,
}

impl<T> Debug for ArcMutexGuard<T>
where
    T: ?Sized,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        todo!()
    }
}

impl<T> Drop for ArcMutexGuard<T>
where
    T: ?Sized,
{
    fn drop(&mut self) {
        unsafe {
            self.mutex.raw.unlock();
        }
    }
}

impl<T> Deref for ArcMutexGuard<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.mutex.data.get() }
    }
}

impl<T> DerefMut for ArcMutexGuard<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.mutex.data.get() }
    }
}

impl<T> ArcMutexGuard<T>
where
    T: ?Sized,
{
    #[must_use]
    pub const fn mutex(guard: &Self) -> &Arc<Mutex<T>> {
        &guard.mutex
    }
    pub fn bump(guard: &Self) {
        unsafe {
            guard.mutex.raw.bump();
        }
    }

    pub fn unlocked<F, R>(guard: &Self, f: F) -> R
    where
        F: FnOnce() -> R,
    {
        unsafe {
            guard.mutex.force_unlock();
        }
        guard.unlocked_inner(f)
    }

    pub fn unlocked_fair<F, R>(guard: &Self, f: F) -> R
    where
        F: FnOnce() -> R,
    {
        unsafe {
            guard.mutex.force_unlock_fair();
        }
        guard.unlocked_inner(f)
    }
    fn unlocked_inner<F, R>(&self, f: F) -> R
    where
        F: FnOnce() -> R,
    {
        let r = f();
        self.mutex.lock();
        r
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
                    while mutex.raw().queue().num_waiting() == 0 {
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
                    while mutex.raw().queue().num_waiting() == 0 {
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
    fn debug_unlocked() {
        let mutex = Mutex::new(32);
        let debug_str = format!("{:?}", mutex);
        println!("{}", debug_str);
        assert_eq!(debug_str, "Mutex { data: 32 }");
    }

    #[test]
    fn debug_locked() {
        let mutex = Mutex::new(32);
        let _guard = mutex.lock();
        let debug_str = format!("{:?}", mutex);
        println!("{}", debug_str);
        assert_eq!(debug_str, "Mutex { data: <locked> }");
    }
}
