/// [`AtomicConstPtr<T>`] is a wrapper around a standard atomic pointer where all methods use and return
/// const versions of the pointer rather than mutable versions. The API is otherwise the same as
/// [`AtomicPtr<T>`]
use core::sync::atomic::AtomicPtr;
use core::sync::atomic::Ordering;

trait ToConst {
    type ConstSelf;
    fn to_const(&self) -> Self::ConstSelf;
}

impl<T> ToConst for Result<*mut T, *mut T> {
    type ConstSelf = Result<*const T, *const T>;

    fn to_const(&self) -> Self::ConstSelf {
        self.map(<*mut T>::cast_const).map_err(<*mut T>::cast_const)
    }
}

/// `AtomicConstPtr<T>` is a wrapper around a standard atomic pointer where all methods use and return
/// const versions of the pointer rather than mutable versions. The API is otherwise the same as
/// [`AtomicPtr<T>`]
#[derive(Debug)]
pub struct AtomicConstPtr<T> {
    inner_ptr: AtomicPtr<T>,
}

impl<T> AtomicConstPtr<T> {
    pub const fn new(ptr: *const T) -> Self {
        Self {
            inner_ptr: AtomicPtr::new(ptr.cast_mut()),
        }
    }

    pub fn store(&self, ptr: *const T, order: Ordering) {
        self.inner_ptr.store(ptr.cast_mut(), order);
    }

    pub fn load(&self, order: Ordering) -> *const T {
        self.inner_ptr.load(order).cast_const()
    }

    pub fn compare_exchange(
        &self,
        current: *const T,
        new: *const T,
        success: Ordering,
        failure: Ordering,
    ) -> Result<*const T, *const T> {
        self.inner_ptr
            .compare_exchange(current.cast_mut(), new.cast_mut(), success, failure)
            .to_const()
    }

    pub fn compare_exchange_weak(
        &self,
        current: *const T,
        new: *const T,
        success: Ordering,
        failure: Ordering,
    ) -> Result<*const T, *const T> {
        self.inner_ptr
            .compare_exchange_weak(current.cast_mut(), new.cast_mut(), success, failure)
            .to_const()
    }

    pub fn swap(&self, ptr: *const T, order: Ordering) -> *const T {
        self.inner_ptr.swap(ptr.cast_mut(), order).cast_const()
    }
}
