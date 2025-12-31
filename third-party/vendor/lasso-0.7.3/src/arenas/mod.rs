mod bucket;
mod single_threaded;

#[cfg(feature = "multi-threaded")]
mod atomic_bucket;
#[cfg(feature = "multi-threaded")]
mod lockfree;

#[cfg(feature = "multi-threaded")]
pub(crate) use lockfree::LockfreeArena;
pub(crate) use single_threaded::Arena;

use core::fmt::{self, Debug};

/// A wrapper type to abstract over all arena types
///
/// Used for readers & resolvers to allow them to be created from
/// any arena type without using dynamic dispatch or allocation
pub(crate) enum AnyArena {
    Arena(Arena),
    #[cfg(feature = "multi-threaded")]
    Lockfree(LockfreeArena),
}

impl Debug for AnyArena {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Arena(arena) => arena.fmt(f),
            #[cfg(feature = "multi-threaded")]
            Self::Lockfree(arena) => arena.fmt(f),
        }
    }
}
