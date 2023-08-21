pub mod intrusive_linked_list;

use crate::maybe_ref::MaybeRef;

pub trait IntrusiveToken<T> {
    fn push(&self);
    fn revoke(&self) -> bool;
    fn inner(&self) -> &T;
    fn is_on_queue(&self) -> bool;
}

pub trait Node<T> {}
pub trait IntrusiveList<T> {
    const NEW: Self;
    type Token<'a>: IntrusiveToken<T>
    where
        Self: 'a,
        T: 'a;
    type Node: Node<T>;
    fn pop<R>(&self, on_pop: impl Fn(&T) -> R) -> Option<R>;

    fn build_node(data: T) -> Self::Node;
    fn build_token<'a>(&'a self, node: impl Into<MaybeRef<'a, Self::Node>>) -> Self::Token<'a>
    where
        T: 'a;
}
pub trait IntrusiveListCloneExt<T>: IntrusiveList<T>
where
    T: Clone,
{
    fn pop_clone(&self) -> Option<T> {
        self.pop(Clone::clone)
    }
}

impl<O, T> IntrusiveListCloneExt<T> for O
where
    O: IntrusiveList<T>,
    T: Clone,
{
}
