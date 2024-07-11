#![allow(clippy::inline_always)]

mod read_guard;
mod write_guard;

use crate::raw_rw_lock::RawRwLock;
pub use read_guard::*;
use std::fmt::{Debug, Formatter};
use std::ptr;
use std::sync::Arc;
use std::time::{Duration, Instant};
pub use write_guard::*;

#[derive(Default)]
pub struct RwLock<T: ?Sized> {
    lock: RawRwLock,
    data: T,
}

impl<T> Debug for RwLock<T>
where
    T: ?Sized + Debug,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let guard = self.try_read();
        if let Some(guard) = guard {
            f.debug_struct("RwLock").field("data", &&(*guard)).finish()
        } else {
            struct LockedPlaceholder;
            impl Debug for LockedPlaceholder {
                fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
                    f.write_str("<locked>")
                }
            }

            f.debug_struct("RwLock")
                .field("data", &LockedPlaceholder)
                .finish()
        }
    }
}

impl<T> RwLock<T> {
    pub const fn new(data: T) -> Self {
        Self {
            lock: RawRwLock::new(),
            data,
        }
    }

    pub fn into_inner(self) -> T {
        self.data
    }

    pub const fn from_raw(raw_rw_lock: RawRwLock, data: T) -> Self {
        Self {
            lock: raw_rw_lock,
            data,
        }
    }
}

impl<T> RwLock<T>
where
    T: ?Sized,
{
    pub const fn data_ptr(&self) -> *mut T {
        ptr::from_ref(&self.data).cast_mut()
    }

    const fn make_read_guard(&self) -> ReadGuard<'_, T> {
        ReadGuard::new(self)
    }
    pub const unsafe fn make_read_guard_unchecked(&self) -> ReadGuard<'_, T> {
        self.make_read_guard()
    }

    fn make_arc_read_guard(self: &Arc<Self>) -> ArcReadGuard<T> {
        ArcReadGuard::new(Arc::clone(self))
    }
    pub unsafe fn make_arc_read_guard_unchecked(self: &Arc<Self>) -> ArcReadGuard<T> {
        self.make_arc_read_guard()
    }

    const fn make_write_guard(&self) -> WriteGuard<'_, T> {
        WriteGuard::new(self)
    }
    pub const unsafe fn make_write_guard_unchecked(&self) -> WriteGuard<'_, T> {
        self.make_write_guard()
    }

    fn make_arc_write_guard(self: &Arc<Self>) -> ArcWriteGuard<T> {
        ArcWriteGuard::new(Arc::clone(self))
    }

    pub unsafe fn make_arc_write_guard_unchecked(self: &Arc<Self>) -> ArcWriteGuard<T> {
        self.make_arc_write_guard()
    }

    pub fn read(&self) -> ReadGuard<T> {
        self.lock.lock_shared();
        self.make_read_guard()
    }

    pub fn arc_read(self: &Arc<Self>) -> ArcReadGuard<T> {
        self.lock.lock_shared();
        self.make_arc_read_guard()
    }

    pub fn try_read(&self) -> Option<ReadGuard<T>> {
        self.lock.try_lock_shared().then(|| self.make_read_guard())
    }

    pub fn try_read_arc(self: &Arc<Self>) -> Option<ArcReadGuard<T>> {
        self.lock
            .try_lock_shared()
            .then(|| self.make_arc_read_guard())
    }

    pub fn try_read_for(&self, timeout: Duration) -> Option<ReadGuard<T>> {
        self.lock
            .try_lock_shared_for(timeout)
            .then(|| self.make_read_guard())
    }

    pub fn try_read_for_arc(self: &Arc<Self>, timeout: Duration) -> Option<ArcReadGuard<T>> {
        self.lock
            .try_lock_shared_for(timeout)
            .then(|| self.make_arc_read_guard())
    }

    pub fn try_read_until(&self, timeout: Instant) -> Option<ReadGuard<T>> {
        self.lock
            .try_lock_shared_until(timeout)
            .then(|| self.make_read_guard())
    }

    pub fn try_read_until_arc(self: &Arc<Self>, timeout: Instant) -> Option<ArcReadGuard<T>> {
        self.lock
            .try_lock_shared_until(timeout)
            .then(|| self.make_arc_read_guard())
    }

    pub fn write(&self) -> WriteGuard<T> {
        self.lock.lock_exclusive();
        self.make_write_guard()
    }

    pub fn arc_write(self: &Arc<Self>) -> ArcWriteGuard<T> {
        self.lock.lock_exclusive();
        self.make_arc_write_guard()
    }

    pub fn try_write(&self) -> Option<WriteGuard<T>> {
        self.lock
            .try_lock_exclusive()
            .then(|| self.make_write_guard())
    }

    pub fn try_write_arc(self: &Arc<Self>) -> Option<ArcWriteGuard<T>> {
        self.lock
            .try_lock_exclusive()
            .then(|| self.make_arc_write_guard())
    }

    pub fn try_write_for(&self, timeout: Duration) -> Option<WriteGuard<T>> {
        self.lock
            .try_lock_exclusive_for(timeout)
            .then(|| self.make_write_guard())
    }

    pub fn try_write_for_arc(self: &Arc<Self>, timeout: Duration) -> Option<ArcWriteGuard<T>> {
        self.lock
            .try_lock_exclusive_for(timeout)
            .then(|| self.make_arc_write_guard())
    }

    pub fn try_write_until(&self, timeout: Instant) -> Option<WriteGuard<T>> {
        self.lock
            .try_lock_exclusive_until(timeout)
            .then(|| self.make_write_guard())
    }

    pub fn try_write_until_arc(self: &Arc<Self>, timeout: Instant) -> Option<ArcWriteGuard<T>> {
        self.lock
            .try_lock_exclusive_until(timeout)
            .then(|| self.make_arc_write_guard())
    }

    ///Returns a mutable reference to the underlying data.
    ///
    /// Since this call borrows the RwLock mutably, no actual locking needs to take place—the mutable borrow statically guarantees no locks exist.
    pub fn get_mut(&mut self) -> &mut T {
        &mut self.data
    }

    /// Checks whether this RwLock is currently locked in any way.
    pub fn is_locked(&self) -> bool {
        self.lock.is_locked()
    }

    ///Check if this RwLock is currently exclusively locked.
    pub fn is_locked_exclusive(&self) -> bool {
        self.lock.is_locked_exclusive()
    }

    ///Forcibly unlocks a read lock.
    ///
    /// This is useful when combined with `mem::forget` to hold a lock without the need to maintain a RwLockReadGuard object alive, for example when dealing with FFI.
    /// Safety
    ///
    /// This method must only be called if the current thread logically owns a RwLockReadGuard but that guard has be discarded using `mem::forget`. Behavior is undefined if a rwlock is read-unlocked when not read-locked.
    pub unsafe fn force_unlock_read(&self) {
        self.lock.unlock_shared();
    }

    ///Forcibly unlocks a write lock.
    ///
    /// This is useful when combined with `mem::forget` to hold a lock without the need to maintain a `RwLockWriteGuard` object alive, for example when dealing with FFI.
    /// Safety
    ///
    /// This method must only be called if the current thread logically owns a RwLockWriteGuard but that guard has be discarded using `mem::forget`. Behavior is undefined if a rwlock is write-unlocked when not write-locked.
    pub unsafe fn force_unlock_write(&self) {
        self.lock.unlock_exclusive();
    }

    ///Returns the underlying raw reader-writer lock object.
    ///
    /// Safety
    ///
    /// This method is unsafe because it allows unlocking a mutex while still holding a reference to a lock guard.
    pub const unsafe fn raw_lock(&self) -> &RawRwLock {
        &self.lock
    }

    const fn raw(&self) -> &RawRwLock {
        &self.lock
    }

    const fn data(&self) -> &T {
        &self.data
    }
}

impl<T> RwLock<T>
where
    T: ?Sized + Send + Sync,
{
    async fn read_async(&self) -> ReadGuard<'_, T> {
        self.lock.lock_shared_async().await;
        self.make_read_guard()
    }

    async fn read_async_arc(self: &Arc<Self>) -> ArcReadGuard<T> {
        self.lock.lock_shared_async().await;
        self.make_arc_read_guard()
    }

    async fn write_async(&self) -> WriteGuard<'_, T> {
        self.lock.lock_exclusive_async().await;
        self.make_write_guard()
    }

    async fn write_async_arc(self: &Arc<Self>) -> ArcWriteGuard<T> {
        self.lock.lock_exclusive_async().await;
        self.make_arc_write_guard()
    }
}

trait IsRWLock<T: ?Sized> {
    fn get_lock(&self) -> &RwLock<T>;
}

impl<'a, T> IsRWLock<T> for &'a RwLock<T>
where
    T: ?Sized,
{
    fn get_lock(&self) -> &RwLock<T> {
        self
    }
}

impl<T> IsRWLock<T> for Arc<RwLock<T>>
where
    T: ?Sized,
{
    fn get_lock(&self) -> &RwLock<T> {
        Arc::as_ref(self)
    }
}

#[cfg(test)]
mod tests {
    use super::{ReadGuard, RwLock, WriteGuard};
    use rand::{Rng, SeedableRng};
    use rand_chacha::ChaCha20Rng;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::channel;
    use std::sync::Arc;
    use std::thread;

    #[cfg(feature = "serde")]
    use bincode::{deserialize, serialize};

    #[derive(Eq, PartialEq, Debug)]
    struct NonCopy(i32);

    #[test]
    fn smoke() {
        let l = RwLock::new(());
        drop(l.read());
        drop(l.write());
        drop((l.read(), l.read()));
        drop(l.write());
    }

    #[test]
    fn frob() {
        const N: u32 = 10;
        const M: u32 = 1000;

        let r = Arc::new(RwLock::new(()));

        // swap this for some set number to make the test run deterministically
        let seed: u64 = rand::random();
        println!("seed: {seed}");

        let (tx, rx) = channel::<()>();
        for i in 0..N {
            let tx = tx.clone();
            let r = r.clone();
            let mut rng = ChaCha20Rng::seed_from_u64(u64::from(i) + seed);
            thread::spawn(move || {
                for _ in 0..M {
                    if rng.gen_bool(1.0 / f64::from(N)) {
                        drop(r.write());
                    } else {
                        drop(r.read());
                    }
                }
                drop(tx);
            });
        }
        drop(tx);
        let _ = rx.recv();
    }

    #[test]
    fn test_rw_arc_no_poison_wr() {
        let arc = Arc::new(RwLock::new(1));
        let arc2 = arc.clone();
        let _: Result<(), _> = thread::spawn(move || {
            let _lock = arc2.write();
            panic!();
        })
        .join();
        let lock = arc.read();
        assert_eq!(*lock, 1);
    }

    #[test]
    fn test_rw_arc_no_poison_ww() {
        let arc = Arc::new(RwLock::new(1));
        let arc2 = arc.clone();
        let _: Result<(), _> = thread::spawn(move || {
            let _lock = arc2.write();
            panic!();
        })
        .join();
        let lock = arc.write();
        assert_eq!(*lock, 1);
    }

    #[test]
    fn test_rw_arc_no_poison_rr() {
        let arc = Arc::new(RwLock::new(1));
        let arc2 = arc.clone();
        let _: Result<(), _> = thread::spawn(move || {
            let _lock = arc2.read();
            panic!();
        })
        .join();
        let lock = arc.read();
        assert_eq!(*lock, 1);
    }

    #[test]
    fn test_rw_arc_no_poison_rw() {
        let arc = Arc::new(RwLock::new(1));
        let arc2 = arc.clone();
        let _: Result<(), _> = thread::spawn(move || {
            let _lock = arc2.read();
            panic!()
        })
        .join();
        let lock = arc.write();
        assert_eq!(*lock, 1);
    }

    #[test]
    fn test_rw_arc() {
        let arc = Arc::new(RwLock::new(0));
        let arc2 = arc.clone();
        let (tx, rx) = channel();

        thread::spawn(move || {
            let mut lock = arc2.write();
            for _ in 0..10 {
                let tmp = *lock;
                *lock = -1;
                thread::yield_now();
                *lock = tmp + 1;
            }
            tx.send(()).unwrap();
        });

        // Readers try to catch the writer in the act
        let mut children = Vec::new();
        for _ in 0..5 {
            let arc3 = arc.clone();
            children.push(thread::spawn(move || {
                let lock = arc3.read();
                assert!(*lock >= 0);
            }));
        }

        // Wait for children to pass their asserts
        for r in children {
            assert!(r.join().is_ok());
        }

        // Wait for writer to finish
        rx.recv().unwrap();
        let lock = arc.read();
        assert_eq!(*lock, 10);
    }

    #[test]
    fn test_rw_arc_access_in_unwind() {
        let arc = Arc::new(RwLock::new(1));
        let arc2 = arc.clone();
        let _ = thread::spawn(move || {
            struct Unwinder {
                i: Arc<RwLock<isize>>,
            }
            impl Drop for Unwinder {
                fn drop(&mut self) {
                    let mut lock = self.i.write();
                    *lock += 1;
                }
            }
            let _u = Unwinder { i: arc2 };
            panic!();
        })
        .join();
        let lock = arc.read();
        assert_eq!(*lock, 2);
    }

    #[test]
    fn test_rwlock_unsized() {
        let rw: &RwLock<[i32]> = &RwLock::new([1, 2, 3]);
        {
            let mut b = rw.write();
            (*b)[0] = 4;
            (*b)[2] = 5;
        }
        let comp: &[i32] = &[4, 2, 5];
        assert_eq!(&*rw.read(), comp);
    }

    #[test]
    fn test_rwlock_try_read() {
        let lock = RwLock::new(0isize);
        {
            let read_guard = lock.read();

            let read_result = lock.try_read();
            assert!(
                read_result.is_some(),
                "try_read should succeed while read_guard is in scope"
            );

            drop(read_guard);
        }
        {
            let write_guard = lock.write();

            let read_result = lock.try_read();
            assert!(
                read_result.is_none(),
                "try_read should fail while write_guard is in scope"
            );

            drop(write_guard);
        }
    }

    #[test]
    fn test_rwlock_try_write() {
        let lock = RwLock::new(0isize);
        {
            let read_guard = lock.read();

            let write_result = lock.try_write();
            assert!(
                write_result.is_none(),
                "try_write should fail while read_guard is in scope"
            );
            assert!(lock.is_locked());
            assert!(!lock.is_locked_exclusive());

            drop(read_guard);
        }
        {
            let write_guard = lock.write();

            let write_result = lock.try_write();
            assert!(
                write_result.is_none(),
                "try_write should fail while write_guard is in scope"
            );
            assert!(lock.is_locked());
            assert!(lock.is_locked_exclusive());

            drop(write_guard);
        }
    }

    #[test]
    fn test_into_inner() {
        let m = RwLock::new(NonCopy(10));
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
        let m = RwLock::new(Foo(num_drops.clone()));
        assert_eq!(num_drops.load(Ordering::SeqCst), 0);
        {
            let _inner = m.into_inner();
            assert_eq!(num_drops.load(Ordering::SeqCst), 0);
        }
        assert_eq!(num_drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_get_mut() {
        let mut m = RwLock::new(NonCopy(10));
        *m.get_mut() = NonCopy(20);
        assert_eq!(m.into_inner(), NonCopy(20));
    }

    #[test]
    fn test_rwlockguard_sync() {
        fn sync<T: Sync>(_: T) {}

        let rwlock = RwLock::new(());
        sync(rwlock.read());
        sync(rwlock.write());
    }

    #[test]
    fn test_rwlock_downgrade() {
        let x = Arc::new(RwLock::new(0));
        let mut handles = Vec::new();
        let num_iters = 100;
        let num_threads = 5;
        for _ in 0..num_threads {
            let x = x.clone();
            handles.push(thread::spawn(move || {
                for _ in 0..num_iters {
                    let mut writer = x.write();
                    *writer += 1;
                    let cur_val = *writer;
                    let reader = WriteGuard::downgrade(writer);
                    assert_eq!(cur_val, *reader);
                }
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(*x.read(), num_threads * num_iters);
    }
    #[test]
    fn test_rwlock_debug() {
        let x = RwLock::new(vec![0u8, 10]);

        assert_eq!(format!("{x:?}"), "RwLock { data: [0, 10] }");
        let _lock = x.write();
        assert_eq!(format!("{x:?}"), "RwLock { data: <locked> }");
    }

    #[cfg(feature = "serde")]
    #[test]
    fn test_serde() {
        let contents: Vec<u8> = vec![0, 1, 2];
        let mutex = RwLock::new(contents.clone());

        let serialized = serialize(&mutex).unwrap();
        let deserialized: RwLock<Vec<u8>> = deserialize(&serialized).unwrap();

        assert_eq!(*(mutex.read()), *(deserialized.read()));
        assert_eq!(contents, *(deserialized.read()));
    }

    #[test]
    fn test_issue_203() {
        struct Bar(RwLock<()>);

        impl Drop for Bar {
            fn drop(&mut self) {
                let _n = self.0.write();
            }
        }

        thread_local! {
            static B: Bar = Bar(RwLock::new(()));
        }

        thread::spawn(|| {
            B.with(|_| ());

            let a = RwLock::new(());
            let _a = a.read();
        })
        .join()
        .unwrap();
    }

    #[test]
    fn test_rw_write_is_locked() {
        let lock = RwLock::new(0isize);
        {
            let _read_guard = lock.read();

            assert!(lock.is_locked());
            assert!(!lock.is_locked_exclusive());
        }

        {
            let _write_guard = lock.write();

            assert!(lock.is_locked());
            assert!(lock.is_locked_exclusive());
        }
    }
}
