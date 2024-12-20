use std::ops::Add;
use parking::Waker as ThreadWaker;
use std::task::Waker;
use std::time::{Duration, Instant};

//TODO naming here is weird...
#[derive(Debug)]
pub(crate) enum MaybeAsyncWaker {
    Parker(ThreadWaker),
    Waker(Waker)
}

impl MaybeAsyncWaker {
    pub fn wake(self) {
        match self {
            MaybeAsyncWaker::Parker(p) => p.wake(),
            MaybeAsyncWaker::Waker(w) => w.wake()
        }
    }

    pub fn wake_by_ref(&self) {
        match self {
            MaybeAsyncWaker::Parker(p) => p.wake(),
            MaybeAsyncWaker::Waker(w) => w.wake_by_ref()
        }
    }
}

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