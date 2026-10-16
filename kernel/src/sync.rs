//! Synchronization for a single-CPU, preemptively scheduled kernel.
//!
//! Three tools, each with one job:
//!
//! - [`TryLock`]: exclusive access to shared kernel state (heap, memory
//!   manager, display, ...). Holding one also disables *preemption* (see
//!   [`PreemptGuard`]), so a thread is never switched out while it holds a
//!   lock. With one CPU, the only way to find a `TryLock` held is therefore
//!   re-entry from an exception or the panic handler; callers then skip the
//!   work instead of spinning (which would deadlock).
//! - [`PreemptGuard`]: keeps the current thread on the CPU (interrupts still
//!   run, the scheduler just does not switch at their end).
//! - [`IrqCell`]: state shared between threads *and* interrupt handlers
//!   (scheduler, process table). Accessed only with interrupts disabled, so
//!   on one CPU nothing else can touch it meanwhile; re-entry is a bug and
//!   panics.
//!
//! Rule: never block, yield or sleep while holding a `TryLock` or
//! `PreemptGuard` (the scheduler asserts this).

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::arch::x86_64 as arch;

/// Nesting depth of preemption-disabled sections on the (only) CPU.
static PREEMPT_DISABLED: AtomicU32 = AtomicU32::new(0);

/// Whether preemption is currently disabled.
pub fn preemption_disabled() -> bool {
    PREEMPT_DISABLED.load(Ordering::Relaxed) != 0
}

/// Disables preemption until dropped. Nests.
#[derive(Debug)]
pub struct PreemptGuard(());

impl PreemptGuard {
    pub fn new() -> Self {
        PREEMPT_DISABLED.fetch_add(1, Ordering::Acquire);
        Self(())
    }
}

impl Default for PreemptGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for PreemptGuard {
    fn drop(&mut self) {
        PREEMPT_DISABLED.fetch_sub(1, Ordering::Release);
    }
}

/// A lock that can only be *tried*.
///
/// With one CPU, a lock that is already held can never be released by
/// someone else while we wait, so spinning would deadlock. Interrupt
/// handlers never take these locks and holders cannot be preempted, so the
/// only way to find one held is re-entry from the panic handler (or a fatal
/// exception) while the holder was mid-operation; callers then skip the
/// work instead.
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

    /// Acquire the lock if it is free. Preemption stays disabled while the
    /// guard lives.
    pub fn try_lock(&self) -> Option<TryLockGuard<'_, T>> {
        let preempt = PreemptGuard::new();
        self.locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| TryLockGuard {
                lock: self,
                _preempt: preempt,
            })
    }
}

/// Exclusive access to the value of a [`TryLock`]; released on drop.
#[derive(Debug)]
pub struct TryLockGuard<'a, T> {
    lock: &'a TryLock<T>,
    _preempt: PreemptGuard,
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
        // Field `_preempt` is dropped after this body: the lock is released
        // before preemption is allowed again.
        self.lock.locked.store(false, Ordering::Release);
    }
}

/// State shared with interrupt handlers, accessed with interrupts disabled.
#[derive(Debug)]
pub struct IrqCell<T> {
    busy: AtomicBool,
    value: UnsafeCell<T>,
}

// SAFETY: `value` is only accessed inside `with`, which runs with
// interrupts disabled on the only CPU and refuses re-entry via `busy`, so
// at most one `&mut T` exists at a time.
unsafe impl<T: Send> Sync for IrqCell<T> {}

impl<T> IrqCell<T> {
    pub const fn new(value: T) -> Self {
        Self {
            busy: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }

    /// Run `f` with exclusive access, interrupts disabled for its duration
    /// (and restored to their previous state afterwards).
    ///
    /// # Panics
    ///
    /// On re-entry (calling `with` on the same cell from inside `f`, or from
    /// an exception that interrupted `f`): that is a kernel bug.
    pub fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        let enabled = arch::interrupts_enabled();
        arch::disable_interrupts();
        assert!(
            !self.busy.swap(true, Ordering::Acquire),
            "IrqCell re-entered"
        );
        // SAFETY: interrupts are off and `busy` was false, so this is the
        // only access (see the `Sync` impl).
        let result = f(unsafe { &mut *self.value.get() });
        self.busy.store(false, Ordering::Release);
        if enabled {
            arch::enable_interrupts();
        }
        result
    }

    /// Raw pointer to the value, for fields the CPU or assembly writes
    /// (saved stack pointers). Never turned into a reference outside `with`.
    pub fn as_ptr(&self) -> *mut T {
        self.value.get()
    }
}
