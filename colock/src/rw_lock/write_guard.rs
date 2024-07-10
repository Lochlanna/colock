use crate::rw_lock::{ArcRwLockReadGuard, IsRWLock, RwLock, RwLockReadGuard};
use std::fmt::{Debug, Display, Formatter};
use std::marker::PhantomData;
use std::mem::forget;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use std::{mem, ptr};

pub struct RwLockWriteGuardBase<T: ?Sized, L: IsRWLock<T>> {
    lock: L,
    phantom_data: PhantomData<T>,
}

impl<T, L> Drop for RwLockWriteGuardBase<T, L>
where
    T: ?Sized,
    L: IsRWLock<T>,
{
    fn drop(&mut self) {
        unsafe {
            self.lock.get_lock().lock.unlock_exclusive();
        }
    }
}

impl<T, L> Display for RwLockWriteGuardBase<T, L>
where
    T: ?Sized + Display,
    L: IsRWLock<T>,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&self.lock.get_lock().data, f)
    }
}

impl<T, L> Debug for RwLockWriteGuardBase<T, L>
where
    T: ?Sized + Debug,
    L: IsRWLock<T>,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Debug::fmt(&self.lock.get_lock().data, f)
    }
}

impl<T, L> Deref for RwLockWriteGuardBase<T, L>
where
    T: ?Sized,
    L: IsRWLock<T>,
{
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.lock.get_lock().data
    }
}

impl<T, L> DerefMut for RwLockWriteGuardBase<T, L>
where
    T: ?Sized,
    L: IsRWLock<T>,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe {
            ptr::from_ref(&self.lock.get_lock().data)
                .cast_mut()
                .as_mut()
                .expect("couldn't cast pointer to reference")
        }
    }
}

impl<T, L> RwLockWriteGuardBase<T, L>
where
    T: ?Sized,
    L: IsRWLock<T>,
{
    pub(crate) const unsafe fn new(lock: L) -> Self {
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
            self.lock.get_lock().lock.unlock_exclusive();
        }
        let result = f();
        self.lock.get_lock().lock.lock_exclusive();
        result
    }

    pub fn unlock_fair(self) {
        unsafe {
            self.lock.get_lock().lock.unlock_exclusive_fair();
        }
        forget(self);
    }
}

pub type RwLockWriteGuard<'a, T: ?Sized> = RwLockWriteGuardBase<T, &'a RwLock<T>>;

impl<'a, T> RwLockWriteGuard<'a, T> {
    pub fn downgrade(self) -> RwLockReadGuard<'a, T> {
        let read_guard = unsafe {
            self.lock.raw().downgrade();
            self.lock.make_read_guard_unchecked()
        };
        // skip calling the destructor
        forget(self);
        read_guard
    }
}
pub type ArcRwLockWriteGuard<T: ?Sized> = RwLockWriteGuardBase<T, Arc<RwLock<T>>>;

impl<T> ArcRwLockWriteGuard<T> {
    pub fn downgrade(self) -> ArcRwLockReadGuard<T> {
        let read_guard = unsafe {
            self.lock.raw().downgrade();
            self.lock.make_arc_read_guard_unchecked()
        };
        // skip calling the destructor
        forget(self);
        read_guard
    }
}
