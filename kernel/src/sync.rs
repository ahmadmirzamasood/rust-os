//! Minimal synchronization for a single-CPU kernel without interrupts.

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

/// A lock that can only be *tried*.
///
/// With one CPU and interrupts masked, a lock that is already held can never
/// be released by someone else while we wait, so spinning would deadlock.
/// The only way to find it held is re-entry from the panic handler while the
/// holder was mid-operation; callers then skip the work instead.
#[derive(Debug)]
pub struct TryLock<T> {
    locked: AtomicBool,
    value: UnsafeCell<T>,
}

// SAFETY: `value` is only reachable through a `TryLockGuard`, and a guard is
// only created after atomically flipping `locked` from false to true, so at
// most one guard (and thus one `&mut T`) exists at a time. `T: Send` because
// the value may be accessed from whichever context holds the guard.
unsafe impl<T: Send> Sync for TryLock<T> {}

impl<T> TryLock<T> {
    pub const fn new(value: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }

    /// Acquire the lock if it is free.
    pub fn try_lock(&self) -> Option<TryLockGuard<'_, T>> {
        self.locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| TryLockGuard { lock: self })
    }
}

/// Exclusive access to the value of a [`TryLock`]; released on drop.
#[derive(Debug)]
pub struct TryLockGuard<'a, T> {
    lock: &'a TryLock<T>,
}

impl<T> Deref for TryLockGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: this guard is the unique holder of the lock (see the `Sync`
        // impl), so no `&mut T` exists elsewhere while this borrow lives.
        unsafe { &*self.lock.value.get() }
    }
}

impl<T> DerefMut for TryLockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: as in `deref`; `&mut self` makes this borrow unique too.
        unsafe { &mut *self.lock.value.get() }
    }
}

impl<T> Drop for TryLockGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
    }
}
