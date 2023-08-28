#![allow(dead_code)]
use std::cell::UnsafeCell;

pub trait Mutex<T> {
    fn new(v: T) -> Self;
    fn lock<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R;
    fn lock_timed<F, R>(&self, f: F) -> (std::time::Duration, R)
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

    fn lock_timed<F, R>(&self, f: F) -> (std::time::Duration, R)
    where
        F: FnOnce(&mut T) -> R,
    {
        let start = std::time::Instant::now();
        let mut guard = self.lock().unwrap();
        let elapsed = start.elapsed();
        let res = f(&mut *guard);
        (elapsed, res)
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

    fn lock_timed<F, R>(&self, f: F) -> (std::time::Duration, R)
    where
        F: FnOnce(&mut T) -> R,
    {
        let start = std::time::Instant::now();
        let mut guard = self.lock();
        let elapsed = start.elapsed();
        let res = f(&mut *guard);
        (elapsed, res)
    }
}

impl<T> Mutex<T> for colock::mutex::Mutex<T> {
    fn new(v: T) -> Self {
        Self::new(v)
    }
    fn lock<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R,
    {
        f(&mut *self.lock())
    }

    fn lock_timed<F, R>(&self, f: F) -> (std::time::Duration, R)
    where
        F: FnOnce(&mut T) -> R,
    {
        let start = std::time::Instant::now();
        let mut guard = self.lock();
        let elapsed = start.elapsed();
        let res = f(&mut *guard);
        (elapsed, res)
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

    fn lock_timed<F, R>(&self, f: F) -> (std::time::Duration, R)
    where
        F: FnOnce(&mut T) -> R,
    {
        let start = std::time::Instant::now();
        let mut guard = self.lock();
        let elapsed = start.elapsed();
        let res = f(&mut *guard);
        (elapsed, res)
    }
}

#[cfg(unix)]
pub struct PthreadMutex<T>(UnsafeCell<T>, UnsafeCell<libc::pthread_mutex_t>);
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

    fn lock_timed<F, R>(&self, f: F) -> (std::time::Duration, R)
    where
        F: FnOnce(&mut T) -> R,
    {
        unsafe {
            let start = std::time::Instant::now();
            libc::pthread_mutex_lock(self.1.get());
            let elapsed = start.elapsed();
            let res = f(&mut *self.0.get());
            libc::pthread_mutex_unlock(self.1.get());
            (elapsed, res)
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
