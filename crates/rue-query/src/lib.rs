//! Parallel, demand-driven query execution primitives.
//!
//! This crate owns execution mechanics only. Compiler query families keep
//! their typed keys, results, equality, and algorithms outside the runtime.

use std::sync::{Condvar, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

mod context;
mod hash;
mod metrics;
mod node;
mod outcome;
mod retention;
mod revision;
mod task;
mod validation;

pub use context::*;
pub use hash::*;
pub use metrics::*;
pub use node::*;
pub use outcome::*;
pub use retention::*;
pub use revision::*;
pub use task::*;
pub use validation::*;

#[cfg(test)]
mod registered_batch_tests;
#[cfg(test)]
mod tests;

fn duration_ns(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn write<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn wait<'a, T>(condvar: &Condvar, guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
    condvar
        .wait(guard)
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
