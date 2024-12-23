use std::ops::Sub;
use std::ptr::null;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct Parker {
    should_wakeup: AtomicBool,
}

impl Default for Parker {
    fn default() -> Self {
        Self::new()
    }
}

impl Parker {
    pub const fn is_cheap_to_construct() -> bool {
        true
    }
    
    pub const fn new() -> Self {
        Self{
            should_wakeup: AtomicBool::new(false),
        }
    }
    
    pub fn should_wakeup(&self) -> bool {
        self.should_wakeup.load(Ordering::Acquire)
    }
    
    pub fn prepare_park(&self) {
        self.should_wakeup.store(false, Ordering::Relaxed);
    }
    
    pub fn waker(&self)->Waker{
        Waker::new(self)
    }

    pub fn async_waker(&self, waker: std::task::Waker) -> Waker {
        Waker::new_task(self, waker)
    }
    
    pub fn park_until(&self, timeout: Instant) -> bool {
        let mut should_wakeup = false;
        while !should_wakeup {
            let diff = timeout.sub(Instant::now());
            if diff.is_zero() {
                break;
            }
            std::thread::park_timeout(diff);
            should_wakeup = self.should_wakeup.load(Ordering::Acquire);
        }
        should_wakeup
    }
    
    pub fn park_for(&self, duration: Duration) -> bool {
        self.park_until(Instant::now() + duration)
    }
    
    pub fn park(&self) {
        while !self.should_wakeup.load(Ordering::Acquire) {
            std::thread::park();
        }
    }
    
}

#[derive(Debug)]
enum MaybeAsync {
    Thread(std::thread::Thread),
    Task(std::task::Waker),
}

#[derive(Debug)]
pub struct Waker {
    parker: *const Parker,
    inner: MaybeAsync
}

impl Waker {

    pub const fn new_task(parker: &Parker, waker: std::task::Waker) -> Waker {
        Self {
            parker,
            inner: MaybeAsync::Task(waker)
        }
    }

    pub const fn new_task_unattached(waker: std::task::Waker) -> Waker {
        Self {
            parker: null(),
            inner: MaybeAsync::Task(waker)
        }
    }
    pub fn new(parker: &Parker)->Self{
        Self {
            parker,
            inner: MaybeAsync::Thread(std::thread::current())
        }
    }

    pub fn wake(self) {
        if !self.parker.is_null() {
            let parker =unsafe { self.parker.as_ref().unwrap() };
            parker.should_wakeup.store(true, Ordering::Release);
        }
        match self.inner {
            MaybeAsync::Thread(handle) => {
                handle.unpark();
            }
            MaybeAsync::Task(waker) => {
                waker.wake();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;


}
