use std::ops::Deref;

#[derive(Debug)]
pub enum MaybeRef<'a, T> {
    Ref(&'a T),
    Owned(T),
    Boxed(Box<T>),
}

/// SAFETY: This is safe because `MaybeRef` is `Send` if `T` is `Send`.
unsafe impl<T> Send for MaybeRef<'_, T> where T: Send {}

impl<'a, T> MaybeRef<'a, T> {
    #[must_use]
    pub const fn new_owned(inner: T) -> Self {
        Self::Owned(inner)
    }

    #[must_use]
    pub const fn new_ref(inner: &'a T) -> Self {
        Self::Ref(inner)
    }

    #[must_use]
    pub const fn new_boxed(inner: Box<T>) -> Self {
        Self::Boxed(inner)
    }
}

impl<T> From<T> for MaybeRef<'_, T> {
    fn from(value: T) -> Self {
        Self::Owned(value)
    }
}

impl<'a, T> From<&'a T> for MaybeRef<'a, T> {
    fn from(value: &'a T) -> Self {
        Self::Ref(value)
    }
}

impl<'a, T> From<Box<T>> for MaybeRef<'a, T> {
    fn from(value: Box<T>) -> Self {
        Self::Boxed(value)
    }
}

impl<'a, T> Deref for MaybeRef<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        match self {
            MaybeRef::Ref(value) => value,
            MaybeRef::Owned(value) => value,
            MaybeRef::Boxed(value) => value,
        }
    }
}
