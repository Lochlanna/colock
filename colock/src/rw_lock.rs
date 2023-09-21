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
            self.read_guard()
        }
    }

    pub async fn write_async(&self) -> RwLockWriteGuard<'_, T> {
        if let Some(guard) = self.try_write() {
            return guard;
        }
        unsafe {
            self.raw().lock_exclusive_async().await;
            self.write_guard()
        }
    }
}

pub type RwLockReadGuard<'a, T> = lock_api::RwLockReadGuard<'a, RawRwLock, T>;
pub type RwLockWriteGuard<'a, T> = lock_api::RwLockWriteGuard<'a, RawRwLock, T>;
