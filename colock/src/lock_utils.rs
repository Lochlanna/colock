use std::ops::Add;
use parking::ThreadWaker;
use std::task::Waker;
use std::time::{Duration, Instant};

//TODO naming here is weird...
#[derive(Debug)]
pub(crate) enum MaybeAsync {
    Parker(ThreadWaker),
    Waker(Waker)
}

impl MaybeAsync {
    pub fn wake(self) {
        match self {
            MaybeAsync::Parker(p) => p.wake(),
            MaybeAsync::Waker(w) => w.wake()
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