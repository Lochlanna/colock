use std::ops::Deref;
use std::sync::Arc;

#[derive(Debug)]
pub enum MaybeRef<'a, T> {
    Ref(&'a T),
    Owned(T),
    Boxed(Box<T>),
    Arc(Arc<T>),
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

impl<'a, T> From<Arc<T>> for MaybeRef<'a, T> {
    fn from(value: Arc<T>) -> Self {
        Self::Arc(value)
    }
}

impl<'a, T> Deref for MaybeRef<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        match self {
            MaybeRef::Ref(value) => value,
            MaybeRef::Owned(value) => value,
            MaybeRef::Boxed(value) => value,
            MaybeRef::Arc(value) => value,
        }
    }
}
