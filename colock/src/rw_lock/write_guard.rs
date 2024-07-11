use crate::rw_lock::{ArcReadGuard, IsRWLock, ReadGuard, RwLock};
use std::fmt::{Debug, Display, Formatter};
use std::marker::PhantomData;
use std::mem::forget;
use std::ops::{Deref, DerefMut};
use std::ptr;
use std::sync::Arc;

pub struct WriteGuardBase<T: ?Sized, L: IsRWLock<T>> {
    lock: L,
    phantom_data: PhantomData<T>,
}

impl<T, L> Drop for WriteGuardBase<T, L>
where
    T: ?Sized,
    L: IsRWLock<T>,
{
    fn drop(&mut self) {
        unsafe {
            self.lock.raw().unlock_exclusive();
        }
    }
}

impl<T, L> Display for WriteGuardBase<T, L>
where
    T: ?Sized + Display,
    L: IsRWLock<T>,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&self.lock.data(), f)
    }
}

impl<T, L> Debug for WriteGuardBase<T, L>
where
    T: ?Sized + Debug,
    L: IsRWLock<T>,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Debug::fmt(&self.lock.data(), f)
    }
}

impl<T, L> Deref for WriteGuardBase<T, L>
where
    T: ?Sized,
    L: IsRWLock<T>,
{
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.lock.data()
    }
}

impl<T, L> DerefMut for WriteGuardBase<T, L>
where
    T: ?Sized,
    L: IsRWLock<T>,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe {
            ptr::from_ref(self.lock.data())
                .cast_mut()
                .as_mut()
                .expect("couldn't cast pointer to reference")
        }
    }
}

impl<T, L> WriteGuardBase<T, L>
where
    T: ?Sized,
    L: IsRWLock<T>,
{
    pub(crate) const fn new(lock: L) -> Self {
        Self {
            lock,
            phantom_data: PhantomData,
        }
    }
    pub fn rwlock(&self) -> &RwLock<T> {
        self.lock.get_lock()
    }

    pub fn unlocked<U, F>(&mut self, f: F) -> U
    where
        F: FnOnce() -> U,
    {
        unsafe {
            self.lock.raw().unlock_exclusive();
        }
        let result = f();
        self.lock.raw().lock_exclusive();
        result
    }

    pub fn unlock_fair(self) {
        unsafe {
            self.lock.raw().unlock_exclusive_fair();
        }
        forget(self);
    }
}

pub type WriteGuard<'a, T: ?Sized> = WriteGuardBase<T, &'a RwLock<T>>;

impl<'a, T> WriteGuard<'a, T> {
    pub fn downgrade(self) -> ReadGuard<'a, T> {
        let read_guard = unsafe {
            self.lock.raw().downgrade();
            self.lock.make_read_guard_unchecked()
        };
        // skip calling the destructor
        forget(self);
        read_guard
    }
}
pub type ArcWriteGuard<T: ?Sized> = WriteGuardBase<T, Arc<RwLock<T>>>;

impl<T> ArcWriteGuard<T> {
    pub fn downgrade(self) -> ArcReadGuard<T> {
        let read_guard = unsafe {
            self.lock.raw().downgrade();
            self.lock.make_arc_read_guard_unchecked()
        };
        // skip calling the destructor
        forget(self);
        read_guard
    }
}
