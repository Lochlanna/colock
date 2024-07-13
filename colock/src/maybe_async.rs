use parking::ThreadWaker;
use std::task::Waker;

//TODO naming here is weird...
#[derive(Debug)]
pub enum MaybeAsync {
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