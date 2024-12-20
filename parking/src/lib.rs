use std::ops::Sub;
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
    
    pub fn prepare_park(&self) {
        
    }
    
    pub fn waker(&self)->Waker{
        Waker::new(self)
    }
    
    pub fn park_until(&self, timeout: Instant) -> bool {
        let mut should_wakeup = false;
        while !should_wakeup {
            let diff = Instant::now().sub(timeout);
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
pub struct Waker {
    parker: *const Parker,
    inner: std::thread::Thread
}

impl Waker {
    pub fn new(parker: &Parker)->Self{
        Self {
            parker,
            inner: std::thread::current()
        }
    }
    
    pub fn wake(&self) {
        let parker =unsafe { self.parker.as_ref().unwrap() };
        parker.should_wakeup.store(true, Ordering::Release);
        self.inner.unpark();
    }
}

#[cfg(test)]
mod tests {
    use super::*;


}
