#[macro_export]
macro_rules! loom_const_fn {
    (
        $(#[$meta:meta])*
        $vis:vis unsafe fn $name:ident($($arg:ident: $T:ty),*) -> $Ret:ty $body:block
    ) => {
        $(#[$meta])*
        #[cfg(not(loom))]
        $vis const unsafe fn $name($($arg: $T),*) -> $Ret $body

        $(#[$meta])*
        #[cfg(loom)]
        $vis unsafe fn $name($($arg: $T),*) -> $Ret $body
    };
    (
        $(#[$meta:meta])*
        $vis:vis fn $name:ident($($arg:ident: $T:ty),*) -> $Ret:ty $body:block
    ) => {
        $(#[$meta])*
        #[cfg(not(loom))]
        $vis const fn $name($($arg: $T),*) -> $Ret $body

        $(#[$meta])*
        #[cfg(loom)]
        $vis fn $name($($arg: $T),*) -> $Ret $body
    }
}
