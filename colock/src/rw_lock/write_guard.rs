use crate::rw_lock::RwLock;
use std::fmt::{Debug, Display, Formatter};
use std::mem::forget;
use std::ops::{Deref, DerefMut};
use std::ptr;

pub struct RwLockWriteGuard<'a, T: ?Sized> {
    lock: &'a RwLock<T>,
}

impl<'a, T> Drop for RwLockWriteGuard<'a, T>
where
    T: ?Sized,
{
    fn drop(&mut self) {
        unsafe {
            self.lock.lock.unlock_exclusive();
        }
    }
}

impl<'a, T> Display for RwLockWriteGuard<'a, T>
where
    T: ?Sized + Display,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&self.lock.data, f)
    }
}

impl<'a, T> Debug for RwLockWriteGuard<'a, T>
where
    T: ?Sized + Debug,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Debug::fmt(&self.lock.data, f)
    }
}

impl<'a, T> Deref for RwLockWriteGuard<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.lock.data
    }
}

impl<'a, T> DerefMut for RwLockWriteGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe {
            ptr::from_ref(&self.lock.data)
                .cast_mut()
                .as_mut()
                .expect("couldn't cast pointer to reference")
        }
    }
}

impl<'a, T> RwLockWriteGuard<'a, T>
where
    T: ?Sized,
{
    pub(crate) const unsafe fn new(lock: &'a RwLock<T>) -> Self {
        Self { lock }
    }
    pub fn rwlock(&self) -> &'a RwLock<T> {
        &self.lock
    }

    pub fn unlocked<U, F>(&mut self, f: F) -> U
    where
        F: FnOnce() -> U,
    {
        unsafe {
            self.lock.lock.unlock_exclusive();
        }
        let result = f();
        self.lock.lock.lock_exclusive();
        result
    }

    pub fn unlock_fair(self) {
        unsafe {
            self.lock.lock.unlock_exclusive_fair();
        }
        forget(self);
    }
}
