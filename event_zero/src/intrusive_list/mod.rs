pub mod intrusive_linked_list;

use crate::maybe_ref::MaybeRef;

pub trait IntrusiveToken<T> {
    fn push_if(&self, condition: impl Fn() -> bool) -> bool;
    fn push(&self) {
        self.push_if(|| true);
    }
    fn revoke(&self) -> bool;
    fn inner(&self) -> &T;
}

pub trait Node<T> {}
pub trait IntrusiveList<T> {
    const NEW: Self;
    type Token<'a>: IntrusiveToken<T>
    where
        Self: 'a,
        T: 'a;
    type Node: Node<T>;
    fn pop_if<R>(
        &self,
        condition: impl Fn(&T, usize) -> Option<R>,
        on_empty: impl Fn(usize),
    ) -> Option<R>;

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
        self.pop_if(|v, _| Some(v.clone()), |_| {})
    }
}

impl<O, T> IntrusiveListCloneExt<T> for O
where
    O: IntrusiveList<T>,
    T: Clone,
{
}
