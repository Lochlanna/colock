use crate::rw_lock::RwLock;
use std::fmt::{Debug, Display, Formatter};
use std::mem::forget;
use std::ops::Deref;
use std::sync::Arc;

pub struct RwLockReadGuard<'a, T: ?Sized> {
    lock: &'a RwLock<T>,
}

impl<'a, T> Drop for RwLockReadGuard<'a, T>
where
    T: ?Sized,
{
    fn drop(&mut self) {
        unsafe {
            self.lock.lock.unlock_shared();
        }
    }
}

impl<'a, T> Display for RwLockReadGuard<'a, T>
where
    T: ?Sized + Display,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&self.lock.data, f)
    }
}

impl<'a, T> Debug for RwLockReadGuard<'a, T>
where
    T: ?Sized + Debug,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Debug::fmt(&self.lock.data, f)
    }
}

impl<'a, T> Deref for RwLockReadGuard<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.lock.data
    }
}

impl<'a, T> RwLockReadGuard<'a, T>
where
    T: ?Sized,
{
    pub(crate) const unsafe fn new(lock: &'a RwLock<T>) -> Self {
        Self { lock }
    }

    ///Returns a reference to the original reader-writer lock object.
    pub fn rwlock(&self) -> &'a RwLock<T> {
        &self.lock
    }

    pub fn unlocked<U, F>(&self, f: F) -> U
    where
        F: FnOnce() -> U,
    {
        unsafe {
            self.lock.lock.unlock_shared();
        }
        let result = f();
        self.lock.lock.lock_shared();
        result
    }

    pub fn unlock_fair(self) {
        unsafe {
            self.lock.lock.unlock_shared_fair();
        }
        forget(self);
    }

    pub fn unlocked_fair<U, F>(&self, f: F) -> U
    where
        F: FnOnce() -> U,
    {
        unsafe {
            self.lock.lock.unlock_shared_fair();
        }
        let result = f();
        self.lock.lock.lock_shared();
        result
    }

    pub fn bump(&mut self) {
        self.lock.lock.bump_shared();
    }
}

pub struct ArcRwLockReadGuard<T: ?Sized> {
    lock: Arc<RwLock<T>>,
}

impl<T> Drop for ArcRwLockReadGuard<T>
where
    T: ?Sized,
{
    fn drop(&mut self) {
        unsafe {
            self.lock.lock.unlock_shared();
        }
    }
}

impl<T> Display for ArcRwLockReadGuard<T>
where
    T: ?Sized + Display,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&self.lock.data, f)
    }
}

impl<T> Debug for ArcRwLockReadGuard<T>
where
    T: ?Sized + Debug,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Debug::fmt(&self.lock.data, f)
    }
}

impl<T> Deref for ArcRwLockReadGuard<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.lock.data
    }
}

impl<T> ArcRwLockReadGuard<T>
where
    T: ?Sized,
{
    pub(crate) const unsafe fn new(lock: Arc<RwLock<T>>) -> Self {
        Self { lock }
    }

    /// Returns a reference to the rwlock, contained in its Arc.
    pub fn rwlock(&self) -> Arc<RwLock<T>> {
        self.lock.clone()
    }

    pub fn unlocked<U, F>(&self, f: F) -> U
    where
        F: FnOnce() -> U,
    {
        unsafe {
            self.lock.lock.unlock_shared();
        }
        let result = f();
        self.lock.lock.lock_shared();
        result
    }

    pub fn unlock_fair(self) {
        unsafe {
            self.lock.lock.unlock_shared_fair();
        }
        forget(self);
    }

    pub fn unlocked_fair<U, F>(&self, f: F) -> U
    where
        F: FnOnce() -> U,
    {
        unsafe {
            self.lock.lock.unlock_shared_fair();
        }
        let result = f();
        self.lock.lock.lock_shared();
        result
    }

    pub fn bump(&mut self) {
        self.lock.lock.bump_shared();
    }
}

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
        assert_eq(*guard, 0);
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
    fn test_read_guard_unlock_fair() {
        let lock = Arc::new(RwLock::new(0));
        let guard = lock.read();
        guard.unlock_fair();
        assert_eq!(*guard, 0);
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
