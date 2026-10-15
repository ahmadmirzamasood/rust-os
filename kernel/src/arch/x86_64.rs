//! x86_64 instructions the kernel cannot express in plain Rust.
//!
//! This file is the only place in the project-owned kernel with inline
//! assembly. Each wrapper is one instruction:
//!
//! | Wrapper | Instruction | Why Rust cannot do it |
//! | --- | --- | --- |
//! | [`outb`] / [`inb`] | `out dx, al` / `in al, dx` | x86 port I/O has no memory-mapped form; needed for the 16550 UART |
//! | [`disable_interrupts`] | `cli` | Masks maskable interrupts; no IDT is installed yet |
//! | [`halt_forever`] | `cli; hlt` | Stops the CPU until the next interrupt instead of busy-waiting |
//!
//! All of these require CPL 0. The bootloader enters the kernel in ring 0 and
//! the kernel never leaves it in this phase.

use core::arch::asm;

/// Write `value` to I/O port `port`.
///
/// # Safety
///
/// Port writes have device-defined side effects. The caller must own the
/// device behind `port` (no other code drives it concurrently) and `value`
/// must be meaningful for that register.
pub unsafe fn outb(port: u16, value: u8) {
    // SAFETY: `out` accesses no memory and leaves flags unchanged; the device
    // side effects are the caller's responsibility per the contract above.
    unsafe {
        asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags));
    }
}

/// Read one byte from I/O port `port`.
///
/// # Safety
///
/// Some device registers change state when read (for example by popping a
/// FIFO). The caller must own the device behind `port`.
pub unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    // SAFETY: `in` accesses no memory and leaves flags unchanged; read side
    // effects are the caller's responsibility per the contract above.
    unsafe {
        asm!("in al, dx", in("dx") port, out("al") value, options(nomem, nostack, preserves_flags));
    }
    value
}

/// Mask maskable interrupts (clear RFLAGS.IF).
///
/// The kernel has no IDT yet, so an interrupt would triple-fault the machine.
/// Calling this at entry and in the panic handler keeps that from happening
/// regardless of how the bootloader left the flag.
pub fn disable_interrupts() {
    // SAFETY: `cli` is permitted at CPL 0, where the kernel runs. It only
    // changes RFLAGS.IF, which no Rust code relies on, and touches no memory.
    unsafe {
        asm!("cli", options(nomem, nostack));
    }
}

/// Halt the CPU permanently.
///
/// Interrupts are masked first, so only NMI or SMI can resume execution; the
/// loop halts again if that happens.
pub fn halt_forever() -> ! {
    loop {
        // SAFETY: `cli` and `hlt` are permitted at CPL 0. `hlt` only pauses
        // the CPU until an interrupt; neither instruction touches memory.
        unsafe {
            asm!("cli", "hlt", options(nomem, nostack));
        }
    }
}
