//! Lock helpers.

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Lock a mutex, recovering the guard if another thread panicked while holding it.
///
/// Every mutex in this crate guards plain data (counters, queues, a cache map) that a panic
/// elsewhere cannot leave logically corrupt, so poisoning carries no information worth
/// propagating; without recovery, one unrelated panic would make a long-lived `Pacer` or
/// `Cache` panic on every later use.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
