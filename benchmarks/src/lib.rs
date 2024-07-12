// extern crate core;
// 
// use std::fmt;
// use std::fmt::Display;
// 
// pub mod async_shared;
// pub mod sync_shared;
// 
// #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
// pub struct Run {
//     num_threads: usize,
//     num_inside: usize,
//     num_outside: usize,
// }
// 
// impl From<(usize, usize, usize)> for Run {
//     fn from((num_threads, num_inside, num_outside): (usize, usize, usize)) -> Self {
//         Self {
//             num_threads,
//             num_inside,
//             num_outside,
//         }
//     }
// }
// 
// impl Display for Run {
//     fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
//         write!(
//             f,
//             "{} threads, {} inside, {} outside",
//             self.num_threads, self.num_inside, self.num_outside
//         )
//     }
// }
