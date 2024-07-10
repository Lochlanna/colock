use crate::rw_lock::{IsRWLock, RwLock};
use std::fmt::{Debug, Display, Formatter};
use std::marker::PhantomData;
use std::mem::forget;
use std::ops::Deref;
use std::sync::Arc;

pub struct RwLockReadGuardBase<T: ?Sized, L: IsRWLock<T>> {
    lock: L,
    phantom_data: PhantomData<T>,
}

impl<T, L> Drop for RwLockReadGuardBase<T, L>
where
    T: ?Sized,
    L: IsRWLock<T>,
{
    fn drop(&mut self) {
        unsafe {
            self.lock.get_lock().lock.unlock_shared();
        }
    }
}

impl<T, L> Display for RwLockReadGuardBase<T, L>
where
    T: ?Sized + Display,
    L: IsRWLock<T>,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&self.lock.get_lock().data, f)
    }
}

impl<T, L> Debug for RwLockReadGuardBase<T, L>
where
    T: ?Sized + Debug,
    L: IsRWLock<T>,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Debug::fmt(&self.lock.get_lock().data, f)
    }
}

impl<T, L> Deref for RwLockReadGuardBase<T, L>
where
    T: ?Sized,
    L: IsRWLock<T>,
{
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.lock.get_lock().data
    }
}

impl<T, L> RwLockReadGuardBase<T, L>
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

    ///Returns a reference to the original reader-writer lock object.
    pub fn rwlock(&self) -> &RwLock<T> {
        self.lock.get_lock()
    }

    pub fn unlocked<U, F>(&self, f: F) -> U
    where
        F: FnOnce() -> U,
    {
        unsafe {
            self.lock.get_lock().lock.unlock_shared();
        }
        let result = f();
        self.lock.get_lock().lock.lock_shared();
        result
    }

    pub fn unlock_fair(self) {
        unsafe {
            self.lock.get_lock().lock.unlock_shared_fair();
        }
        forget(self);
    }

    pub fn unlocked_fair<U, F>(&self, f: F) -> U
    where
        F: FnOnce() -> U,
    {
        unsafe {
            self.lock.get_lock().lock.unlock_shared_fair();
        }
        let result = f();
        self.lock.get_lock().lock.lock_shared();
        result
    }

    pub fn bump(&self) {
        self.lock.get_lock().lock.bump_shared();
    }
}

pub type RwLockReadGuard<'a, T: ?Sized> = RwLockReadGuardBase<T, &'a RwLock<T>>;
pub type ArcRwLockReadGuard<T: ?Sized> = RwLockReadGuardBase<T, Arc<RwLock<T>>>;

//unit tests
#[cfg(test)]
mod tests {
    use super::*;
    use crate::rw_lock::RwLock;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_read_guard() {
        let lock = Arc::new(RwLock::new(0));
        let lock_clone = lock.clone();
        let _ = thread::spawn(move || {
            let _guard = lock_clone.read();
        })
        .join();
        let guard = lock.read();
        assert_eq!(*guard, 0);
    }

    #[test]
    fn test_read_guard_deref() {
        let lock = Arc::new(RwLock::new(0));
        let guard = lock.read();
        assert_eq!(*guard, 0);
    }

    #[test]
    fn test_read_guard_display() {
        let lock = Arc::new(RwLock::new(0));
        let guard = lock.read();
        assert_eq!(format!("{}", guard), "0");
    }

    #[test]
    fn test_read_guard_debug() {
        let lock = Arc::new(RwLock::new(0));
        let guard = lock.read();
        assert_eq!(format!("{:?}", guard), "0");
    }

    #[test]
    fn test_read_guard_unlocked() {
        let lock = Arc::new(RwLock::new(0));
        let guard = lock.read();
        let result = guard.unlocked(|| 1);
        assert_eq!(result, 1);
    }

    #[test]
    fn test_read_guard_unlocked_fair() {
        let lock = Arc::new(RwLock::new(0));
        let guard = lock.read();
        let result = guard.unlocked_fair(|| 1);
        assert_eq!(result, 1);
    }

    #[test]
    fn test_read_guard_bump() {
        let lock = Arc::new(RwLock::new(0));
        let guard = lock.read();
        guard.bump();
        assert_eq!(*guard, 1);
    }
}
