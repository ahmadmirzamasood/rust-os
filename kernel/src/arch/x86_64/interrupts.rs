//! External-interrupt dispatch (vectors 32..=255).
//!
//! # Synchronization rules
//!
//! These rules hold for every interrupt handler in the kernel:
//!
//! 1. Handlers run through 64-bit *interrupt gates*, so RFLAGS.IF is clear
//!    for their whole duration: handlers never nest and never interrupt
//!    each other. There is one CPU, so a handler only ever races with the
//!    code it interrupted.
//! 2. Handlers do bounded work only: acknowledge the device, move at most a
//!    few bytes into a lock-free [`input::ring::ByteRing`] or bump an atomic
//!    counter, and send the EOI. Decoding, drawing and logging happen in the
//!    main loop.
//! 3. Handlers never take a lock (not even a [`crate::sync::TryLock`]),
//!    never allocate, and never print. State they share with normal code is
//!    atomics only: rings (single producer = handler, single consumer = main
//!    loop) and counters.
//! 4. Normal code reads shared values one atomic at a time and tolerates
//!    them changing in between (counters, ring indices). Code that ever
//!    needs several values to be mutually consistent must bracket the reads
//!    with [`super::disable_interrupts`] / [`super::enable_interrupts`];
//!    nothing currently does.
//! 5. The main loop sleeps with [`super::enable_interrupts_and_wait`] only
//!    after seeing no pending work with interrupts disabled, so a wake-up
//!    cannot be lost.
//!
//! CPU exceptions follow their own policy (see [`super::exceptions`]); a
//! fatal exception inside a handler is reported with the IRQ it interrupted.

use core::sync::atomic::{AtomicU8, AtomicU64, Ordering};

use super::pic;

pub const TIMER_IRQ: u8 = 0;
pub const KEYBOARD_IRQ: u8 = 1;
pub const MOUSE_IRQ: u8 = 12;

const NO_IRQ: u8 = 0xFF;

static COUNTS: [AtomicU64; pic::LINES as usize] = [const { AtomicU64::new(0) }; 16];
static SPURIOUS: AtomicU64 = AtomicU64::new(0);
static UNEXPECTED: AtomicU64 = AtomicU64::new(0);
static LAST_UNEXPECTED: AtomicU8 = AtomicU8::new(0);
/// IRQ whose handler is running, or [`NO_IRQ`]. Read by exception reports.
static CURRENT: AtomicU8 = AtomicU8::new(NO_IRQ);

/// Called by the exception dispatcher for vectors 32..=255 (interrupts
/// disabled, see the module rules).
pub fn handle(vector: u64) {
    let master = u64::from(pic::MASTER_OFFSET);
    let Some(irq) = vector
        .checked_sub(master)
        .filter(|&irq| irq < u64::from(pic::LINES))
    else {
        // Not a vector the 8259 delivers. Nothing to acknowledge: the
        // controller that sent it (if any) is not ours.
        UNEXPECTED.fetch_add(1, Ordering::Relaxed);
        LAST_UNEXPECTED.store(vector as u8, Ordering::Relaxed);
        return;
    };
    let irq = irq as u8;
    if pic::is_spurious(irq) {
        SPURIOUS.fetch_add(1, Ordering::Relaxed);
        if irq == 15 {
            pic::end_of_interrupt(irq, false);
        }
        return;
    }
    COUNTS[usize::from(irq)].fetch_add(1, Ordering::Relaxed);
    CURRENT.store(irq, Ordering::Relaxed);
    match irq {
        TIMER_IRQ => crate::time::on_timer_interrupt(),
        KEYBOARD_IRQ | MOUSE_IRQ => crate::drivers::ps2::on_interrupt(),
        _ => {}
    }
    CURRENT.store(NO_IRQ, Ordering::Relaxed);
    pic::end_of_interrupt(irq, true);
}

/// The IRQ whose handler is running, if any.
pub fn current_irq() -> Option<u8> {
    match CURRENT.load(Ordering::Relaxed) {
        NO_IRQ => None,
        irq => Some(irq),
    }
}

/// Description of an IRQ handler context, for diagnostics.
pub fn irq_name(irq: u8) -> &'static str {
    match irq {
        TIMER_IRQ => "IRQ 0 handler (PIT timer)",
        KEYBOARD_IRQ => "IRQ 1 handler (PS/2 keyboard)",
        MOUSE_IRQ => "IRQ 12 handler (PS/2 mouse)",
        _ => "handler of an unassigned IRQ",
    }
}

/// Interrupt statistics snapshot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub timer: u64,
    pub keyboard: u64,
    pub mouse: u64,
    /// Interrupts on lines no driver owns (should stay 0: they are masked).
    pub other_lines: u64,
    pub spurious: u64,
    pub unexpected: u64,
    pub last_unexpected_vector: u8,
}

pub fn stats() -> Stats {
    // Read every counter exactly once: handlers may run between reads, so
    // deriving values from two reads of the same counter could underflow.
    let counts: [u64; pic::LINES as usize] =
        core::array::from_fn(|irq| COUNTS[irq].load(Ordering::Relaxed));
    let total: u64 = counts.iter().sum();
    let (timer, keyboard, mouse) = (
        counts[usize::from(TIMER_IRQ)],
        counts[usize::from(KEYBOARD_IRQ)],
        counts[usize::from(MOUSE_IRQ)],
    );
    Stats {
        timer,
        keyboard,
        mouse,
        other_lines: total - timer - keyboard - mouse,
        spurious: SPURIOUS.load(Ordering::Relaxed),
        unexpected: UNEXPECTED.load(Ordering::Relaxed),
        last_unexpected_vector: LAST_UNEXPECTED.load(Ordering::Relaxed),
    }
}
