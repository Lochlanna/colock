use crate::mutex::{IsMutex, Mutex};
use std::fmt::{Debug, Display, Formatter};
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};
use std::ptr;
use std::sync::Arc;

pub struct MutexGuardBase<T, M>
where
    T: ?Sized,
    M: IsMutex<T>,
{
    mutex: M,
    phantom: PhantomData<T>,
}

impl<T, M> Drop for MutexGuardBase<T, M>
where
    T: ?Sized,
    M: IsMutex<T>,
{
    fn drop(&mut self) {
        unsafe { self.mutex().raw().unlock() }
    }
}

impl<T, M> Deref for MutexGuardBase<T, M>
where
    T: ?Sized,
    M: IsMutex<T>,
{
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { self.mutex().data() }
    }
}

impl<T, M> DerefMut for MutexGuardBase<T, M>
where
    T: ?Sized,
    M: IsMutex<T>,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe {
            self.mutex().mut_data()
        }
    }
}

impl<T, M> Debug for MutexGuardBase<T, M>
where
    T: Debug + ?Sized,
    M: IsMutex<T>,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MutexGuard")
            .field("mutex", &self.mutex())
            .finish()
    }
}

impl<T, M> Display for MutexGuardBase<T, M>
where
    T: Display + ?Sized,
    M: IsMutex<T>,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        unsafe {
            write!(f, "{}", self.mutex().data())
        }
    }
}

unsafe impl<T, M> Sync for MutexGuardBase<T, M>
where
    T: Sync + ?Sized,
    M: IsMutex<T>,
{
}

impl<'l, T, M> MutexGuardBase<T, M>
where
    T: ?Sized,
    M: IsMutex<T> + 'l,
{
    pub fn leak(self) -> &'l mut T {
        let mut_data_ref = unsafe {
            ptr::from_ref(self.mutex().data())
                .cast_mut()
                .as_mut()
                .unwrap()
        };
        core::mem::forget(self);
        mut_data_ref
    }
}

impl<T, M> MutexGuardBase<T, M>
where
    T: ?Sized,
    M: IsMutex<T>,
{
    pub(crate) const unsafe fn new(mutex: M) -> Self {
        Self {
            mutex,
            phantom: PhantomData,
        }
    }
    pub fn mutex(&self) -> &Mutex<T> {
        self.mutex.get_mutex()
    }

    //TODO test me...
    pub fn bump(&mut self) {
        unsafe { self.mutex().raw_mutex.bump() }
    }

    pub fn unlock_fair(self) {
        unsafe {
            self.mutex().raw_mutex.unlock_fair();
        }
        core::mem::forget(self)
    }

    pub fn unlocked<F, U>(&mut self, f: F) -> U
    where
        F: FnOnce() -> U,
    {
        unsafe { self.mutex().raw_mutex.unlock() }
        let result = f();
        self.mutex().raw_mutex.lock();
        result
    }

    pub fn unlocked_fair<F, U>(&mut self, f: F) -> U
    where
        F: FnOnce() -> U,
    {
        unsafe { self.mutex().raw_mutex.unlock_fair() }
        let result = f();
        self.mutex().raw_mutex.lock();
        result
    }
}

pub type MutexGuard<'a, T> = MutexGuardBase<T, &'a Mutex<T>>;
pub type ArcMutexGuard<T> = MutexGuardBase<T, Arc<Mutex<T>>>;
