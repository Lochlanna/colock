use crate::raw_rw_lock::RawRwLock;
use lock_api;
use std::fmt::{Debug, Formatter};
use std::ops::{Deref, DerefMut};

#[derive(Default)]
pub struct RwLock<T>(lock_api::RwLock<RawRwLock, T>)
where
    T: ?Sized;

impl<T> Debug for RwLock<T>
where
    T: Debug,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl<T> Deref for RwLock<T>
where
    T: ?Sized,
{
    type Target = lock_api::RwLock<RawRwLock, T>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> DerefMut for RwLock<T>
where
    T: ?Sized,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<T> RwLock<T> {
    pub const fn new(val: T) -> Self {
        Self(lock_api::RwLock::<RawRwLock, T>::const_new(
            RawRwLock::new(),
            val,
        ))
    }

    pub const fn const_new(val: T) -> Self {
        Self::new(val)
    }

    // Consumes this mutex, returning the underlying data.
    #[inline]
    pub fn into_inner(self) -> T {
        self.0.into_inner()
    }
}

impl<T> RwLock<T>
where
    T: ?Sized,
{
    pub async fn read_async(&self) -> RwLockReadGuard<'_, T> {
        if let Some(guard) = self.try_read() {
            return guard;
        }
        unsafe {
            self.raw().lock_shared_async().await;
            self.make_read_guard_unchecked()
        }
    }

    pub async fn write_async(&self) -> RwLockWriteGuard<'_, T> {
        if let Some(guard) = self.try_write() {
            return guard;
        }
        unsafe {
            self.raw().lock_exclusive_async().await;
            self.make_write_guard_unchecked()
        }
    }
}

pub type RwLockReadGuard<'a, T> = lock_api::RwLockReadGuard<'a, RawRwLock, T>;
pub type RwLockWriteGuard<'a, T> = lock_api::RwLockWriteGuard<'a, RawRwLock, T>;

#[cfg(test)]
mod tests {
    use super::*;
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
            let b = &mut *rw.write();
            b[0] = 4;
            b[2] = 5;
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
                    let reader = RwLockWriteGuard::downgrade(writer);
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
