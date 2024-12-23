use std::ops::Add;
use parking::Waker as ThreadWaker;
use std::task::Waker;
use std::time::{Duration, Instant};


pub(crate) trait Timeout {
    fn to_instant(&self)->Instant;
}

impl Timeout for Instant {
    fn to_instant(&self) -> Instant {
        self.clone()
    }
}

impl Timeout for Duration {
    fn to_instant(&self) -> Instant {
        Instant::now().add(self.clone())
    }
}