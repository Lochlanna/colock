use parking::{ThreadParker, ThreadParkerT};
use std::cell::Cell;
use std::fmt::{Debug, Formatter};
use std::task::Waker;

pub enum Handle {
    Sync(ThreadParker),
    Async(Waker),
}

impl Debug for Handle {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sync(_) => write!(f, "Handle::Sync"),
            Self::Async(_) => write!(f, "Handle::Async"),
        }
    }
}

impl Handle {
    pub unsafe fn unpark_ref(&self) {
        match self {
            Self::Sync(thread_parker) => {
                thread_parker.unpark();
            }
            Self::Async(maybe_waker) => maybe_waker.wake_by_ref(),
        }
    }

    pub unsafe fn unpark_owned(self) {
        match self {
            Self::Sync(thread_parker) => {
                thread_parker.unpark();
            }
            Self::Async(maybe_waker) => maybe_waker.wake(),
        }
    }
}
