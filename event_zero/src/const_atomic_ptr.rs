use core::sync::atomic::{AtomicPtr, Ordering};
use std::ptr::NonNull;

#[repr(transparent)]
#[derive(Debug, Default)]
pub struct ConstAtomicPtr<T> {
    inner: AtomicPtr<T>,
}

trait Pointer<T> {
    fn to_ptr(&self) -> *mut T;
}

impl<T> Pointer<T> for Option<NonNull<T>> {
    fn to_ptr(&self) -> *mut T {
        if let Some(not_null) = self {
            return not_null.as_ptr();
        }
        core::ptr::null_mut()
    }
}

impl<T> ConstAtomicPtr<T> {
    pub const fn new(value: Option<NonNull<T>>) -> Self {
        if let Some(ptr) = value {
            return Self {
                inner: AtomicPtr::new(ptr.as_ptr()),
            };
        }
        Self {
            inner: AtomicPtr::new(core::ptr::null_mut()),
        }
    }
    pub fn load(&self, order: Ordering) -> Option<NonNull<T>> {
        NonNull::new(self.inner.load(order))
    }

    pub fn store(&self, value: Option<NonNull<T>>, order: Ordering) {
        self.inner.store(value.to_ptr(), order)
    }

    pub fn swap(&self, value: Option<NonNull<T>>, order: Ordering) -> Option<NonNull<T>> {
        NonNull::new(self.inner.swap(value.to_ptr(), order))
    }

    pub fn compare_exchange(
        &self,
        current: Option<NonNull<T>>,
        new: Option<NonNull<T>>,
        success: Ordering,
        failure: Ordering,
    ) -> Result<Option<NonNull<T>>, Option<NonNull<T>>> {
        self.inner
            .compare_exchange(current.to_ptr(), new.to_ptr(), success, failure)
            .map(|v| NonNull::new(v))
            .map_err(|e| NonNull::new(e))
    }

    pub fn compare_exchange_weak(
        &self,
        current: Option<NonNull<T>>,
        new: Option<NonNull<T>>,
        success: Ordering,
        failure: Ordering,
    ) -> Result<Option<NonNull<T>>, Option<NonNull<T>>> {
        self.inner
            .compare_exchange_weak(current.to_ptr(), new.to_ptr(), success, failure)
            .map(|v| NonNull::new(v))
            .map_err(|e| NonNull::new(e))
    }
}
