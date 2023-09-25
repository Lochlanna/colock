use parking::{ThreadParker, ThreadParkerT};
use std::cell::Cell;
use std::fmt::{Debug, Formatter};
use std::task::Waker;

pub enum Handle {
    Sync(ThreadParker),
    Async(Cell<Option<Waker>>),
}

impl Debug for Handle {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sync(_) => write!(f, "Handle::Sync"),
            Self::Async(_) => write!(f, "Handle::Async"),
        }
    }
}

pub trait ThreadHandle: Wakeable {
    fn thread_parker(&self) -> Option<&ThreadParker>;
}

pub trait AsyncHandle: Wakeable {
    fn replace_waker(&self, new_waker: Waker);
}

pub trait Wakeable {
    fn unpark_handle(&self) -> UnparkHandle;
}

impl Wakeable for Handle {
    fn unpark_handle(&self) -> UnparkHandle {
        match self {
            Self::Sync(thread_parker) => UnparkHandle::Sync(thread_parker),
            Self::Async(waker) => UnparkHandle::Async(waker.take()),
        }
    }
}

impl ThreadHandle for Handle {
    fn thread_parker(&self) -> Option<&ThreadParker> {
        match self {
            Self::Sync(thread_parker) => Some(thread_parker),
            Self::Async(_) => None,
        }
    }
}

impl AsyncHandle for Handle {
    fn replace_waker(&self, new_waker: Waker) {
        match self {
            Self::Sync(_) => panic!("should not replace waker for sync code"),
            Self::Async(waker) => waker.set(Some(new_waker)),
        };
    }
}

pub enum UnparkHandle {
    Sync(*const ThreadParker),
    Async(Option<Waker>),
}

impl UnparkHandle {
    pub unsafe fn unpark(self) {
        match self {
            Self::Sync(thread_parker) => {
                (*thread_parker).unpark();
            }
            Self::Async(maybe_waker) => maybe_waker
                .expect("should always have a waker. Node was re-used")
                .wake(),
        }
    }
}
