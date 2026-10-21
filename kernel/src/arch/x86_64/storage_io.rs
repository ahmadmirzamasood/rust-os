//! Hardware boundary for PCI configuration and ATA's 16-bit data register.
use core::arch::asm;

/// # Safety
/// Caller owns this I/O port and must supply a valid device register value.
pub unsafe fn outw(port: u16, value: u16) {
    // SAFETY: CPL0, port ownership and value are the caller's contract.
    unsafe {
        asm!("out dx, ax", in("dx") port, in("ax") value, options(nomem, nostack, preserves_flags));
    }
}
/// # Safety
/// Caller exclusively owns the device register at this port.
pub unsafe fn inw(port: u16) -> u16 {
    let value;
    // SAFETY: CPL0; caller owns the register and its read side effects.
    unsafe {
        asm!("in ax, dx", in("dx") port, out("ax") value, options(nomem, nostack, preserves_flags));
    }
    value
}
/// # Safety
/// Caller owns this I/O port and must supply a valid device register value.
pub unsafe fn outl(port: u16, value: u32) {
    // SAFETY: CPL0, port ownership and value are the caller's contract.
    unsafe {
        asm!("out dx, eax", in("dx") port, in("eax") value, options(nomem, nostack, preserves_flags));
    }
}
/// # Safety
/// Caller exclusively owns the device register at this port.
pub unsafe fn inl(port: u16) -> u32 {
    let value;
    // SAFETY: CPL0; caller owns the register and its read side effects.
    unsafe {
        asm!("in eax, dx", in("dx") port, out("eax") value, options(nomem, nostack, preserves_flags));
    }
    value
}
