#![allow(dead_code)]
use std::cell::UnsafeCell;

pub trait Mutex<T> {
    fn new(v: T) -> Self;
    fn lock<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R;
}

impl<T> Mutex<T> for std::sync::Mutex<T> {
    fn new(v: T) -> Self {
        Self::new(v)
    }
    fn lock<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R,
    {
        f(&mut *self.lock().unwrap())
    }
}

impl<T> Mutex<T> for parking_lot::Mutex<T> {
    fn new(v: T) -> Self {
        Self::new(v)
    }
    fn lock<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R,
    {
        f(&mut *self.lock())
    }
}

impl<T> Mutex<T> for colock::Mutex<T> {
    fn new(v: T) -> Self {
        Self::new(v)
    }
    fn lock<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R,
    {
        f(&mut *self.lock())
    }
}

impl<T> Mutex<T> for usync::Mutex<T> {
    fn new(v: T) -> Self {
        Self::new(v)
    }
    fn lock<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R,
    {
        f(&mut *self.lock())
    }
}

#[cfg(not(windows))]
type SrwLock<T> = std::sync::Mutex<T>;

#[cfg(windows)]
use winapi::um::synchapi;
#[cfg(windows)]
struct SrwLock<T>(UnsafeCell<T>, UnsafeCell<synchapi::SRWLOCK>);
#[cfg(windows)]
unsafe impl<T> Sync for SrwLock<T> {}
#[cfg(windows)]
unsafe impl<T: Send> Send for SrwLock<T> {}
#[cfg(windows)]
impl<T> Mutex<T> for SrwLock<T> {
    fn new(v: T) -> Self {
        let mut h: synchapi::SRWLOCK = synchapi::SRWLOCK {
            Ptr: std::ptr::null_mut(),
        };

        unsafe {
            synchapi::InitializeSRWLock(&mut h);
        }
        SrwLock(UnsafeCell::new(v), UnsafeCell::new(h))
    }
    fn lock<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R,
    {
        unsafe {
            synchapi::AcquireSRWLockExclusive(self.1.get());
            let res = f(&mut *self.0.get());
            synchapi::ReleaseSRWLockExclusive(self.1.get());
            res
        }
    }
}

#[cfg(not(unix))]
type PthreadMutex<T> = std::sync::Mutex<T>;

#[cfg(unix)]
struct PthreadMutex<T>(UnsafeCell<T>, UnsafeCell<libc::pthread_mutex_t>);
#[cfg(unix)]
unsafe impl<T> Sync for PthreadMutex<T> {}
#[cfg(unix)]
impl<T> Mutex<T> for PthreadMutex<T> {
    fn new(v: T) -> Self {
        PthreadMutex(
            UnsafeCell::new(v),
            UnsafeCell::new(libc::PTHREAD_MUTEX_INITIALIZER),
        )
    }
    fn lock<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R,
    {
        unsafe {
            libc::pthread_mutex_lock(self.1.get());
            let res = f(&mut *self.0.get());
            libc::pthread_mutex_unlock(self.1.get());
            res
        }
    }
}
#[cfg(unix)]
impl<T> Drop for PthreadMutex<T> {
    fn drop(&mut self) {
        unsafe {
            libc::pthread_mutex_destroy(self.1.get());
        }
    }
}
