use crate::raw_mutex::RawMutex;
use lock_api::RawMutexFair;
use std::cell::UnsafeCell;

pub struct FairMutex<T> {
    inner: RawMutex,
    data: UnsafeCell<T>,
}
