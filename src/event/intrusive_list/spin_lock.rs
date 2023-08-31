//! This is a custom spin lock implementation which will use a hybrid of spin and yield to alleviate issues with priority inversion.
//! It also gives the opportunity to early out of a lock call if some condition is met

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug)]
pub struct SpinLock<T> {
    locked: AtomicBool,
    data: UnsafeCell<T>,
}

unsafe impl<T> Sync for SpinLock<T> {}
unsafe impl<T> Send for SpinLock<T> where T: Send {}

impl<T> SpinLock<T> {
    pub const fn new(data: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            data: UnsafeCell::new(data),
        }
    }
    pub fn lock(&self) -> SpinGuard<T> {
        loop {
            let Err(mut is_locked) = self.locked.compare_exchange_weak(
                false,
                true,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) else {
                return SpinGuard { inner: self };
            };
            while is_locked {
                //TODO add some yield on supported systems..?
                core::hint::spin_loop();
                is_locked = self.locked.load(Ordering::Relaxed);
            }
        }
    }

    pub fn try_lock(&self) -> Option<SpinGuard<T>> {
        if self
            .locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            return Some(SpinGuard { inner: self });
        }
        None
    }

    pub fn try_lock_while(&self, condition: impl Fn() -> bool) -> Option<SpinGuard<T>> {
        loop {
            let Err(mut is_locked) = self.locked.compare_exchange_weak(
                false,
                true,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) else {
                return Some(SpinGuard { inner: self });
            };

            while is_locked {
                //TODO add some yield on supported systems..?
                if !condition() {
                    return None;
                }
                core::hint::spin_loop();
                is_locked = self.locked.load(Ordering::Relaxed);
            }
        }
    }
    fn unlock(&self) {
        debug_assert!(self.locked.load(Ordering::Relaxed));
        self.locked.store(false, Ordering::Release);
    }
}

pub struct SpinGuard<'lock, T> {
    inner: &'lock SpinLock<T>,
}

impl<T> Drop for SpinGuard<'_, T> {
    fn drop(&mut self) {
        self.inner.unlock();
    }
}

impl<T> Deref for SpinGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.inner.data.get() }
    }
}

impl<T> DerefMut for SpinGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.inner.data.get() }
    }
}
