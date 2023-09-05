#![allow(dead_code)]

use async_trait::async_trait;

#[async_trait]
pub trait Mutex<T>: Sync {
    fn new(v: T) -> Self;
    async fn lock<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R + Send;
    async fn lock_timed<F, R>(&self, f: F) -> (std::time::Duration, R)
    where
        F: FnOnce(&mut T) -> R + Send;
}

#[async_trait]
impl<T> Mutex<T> for tokio::sync::Mutex<T>
where
    T: Send,
{
    fn new(v: T) -> Self {
        Self::new(v)
    }
    async fn lock<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R + Send,
    {
        f(&mut *self.lock().await)
    }

    async fn lock_timed<F, R>(&self, f: F) -> (std::time::Duration, R)
    where
        F: FnOnce(&mut T) -> R + Send,
    {
        let start = std::time::Instant::now();
        let mut guard = self.lock().await;
        let elapsed = start.elapsed();
        let res = f(&mut *guard);
        (elapsed, res)
    }
}

#[async_trait]
impl<T> Mutex<T> for maitake_sync::Mutex<T>
where
    T: Send,
{
    fn new(v: T) -> Self {
        Self::new(v)
    }
    async fn lock<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R + Send,
    {
        f(&mut *self.lock().await)
    }

    async fn lock_timed<F, R>(&self, f: F) -> (std::time::Duration, R)
    where
        F: FnOnce(&mut T) -> R + Send,
    {
        let start = std::time::Instant::now();
        let mut guard = self.lock().await;
        let elapsed = start.elapsed();
        let res = f(&mut *guard);
        (elapsed, res)
    }
}

#[async_trait]
impl<T> Mutex<T> for colock::mutex::Mutex<T>
where
    T: Send,
{
    fn new(v: T) -> Self {
        Self::new(v)
    }
    async fn lock<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R + Send,
    {
        f(&mut *self.lock_async().await)
    }

    async fn lock_timed<F, R>(&self, f: F) -> (std::time::Duration, R)
    where
        F: FnOnce(&mut T) -> R + Send,
    {
        let start = std::time::Instant::now();
        let mut guard = self.lock_async().await;
        let elapsed = start.elapsed();
        let res = f(&mut *guard);
        (elapsed, res)
    }
}
