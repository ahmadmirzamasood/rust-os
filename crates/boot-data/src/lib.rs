//! Boot information checks shared by the kernel and host tooling.
//!
//! `no_std` and host-testable. The kernel feeds the bootloader's memory map
//! through [`memory::MemoryMap::load`], which copies it into kernel-owned
//! storage and validates it; the host smoke test matches the serial
//! [`markers`] the kernel prints.

#![no_std]
#![forbid(unsafe_code)]

pub mod memory;

/// Serial-line markers. The kernel prints them; `cargo xtask smoke` matches
/// them. Changing one is a protocol change for the smoke test.
pub mod markers {
    /// Prefix of every kernel diagnostic line.
    pub const PREFIX: &str = "[rust-os]";
    /// First line the kernel prints after its serial port works.
    pub const KERNEL_ENTERED: &str = "[rust-os] kernel entered";
    /// Prefix of the framebuffer line, followed by ` <width>x<height> ...`.
    pub const FRAMEBUFFER: &str = "[rust-os] framebuffer:";
    /// Every stage succeeded and the welcome screen is drawn.
    pub const BOOT_OK: &str = "[rust-os] BOOT OK";
    /// The kernel initialized, but at least one stage (for example graphics)
    /// is unavailable. Serial diagnostics still work.
    pub const BOOT_DEGRADED: &str = "[rust-os] BOOT DEGRADED";
    /// Graphics could not be initialized; followed by the reason.
    pub const GRAPHICS_UNAVAILABLE: &str = "[rust-os] graphics unavailable:";
    /// Start of a kernel panic report.
    pub const PANIC: &str = "[rust-os] KERNEL PANIC";
    /// Panic report line giving the source location.
    pub const PANIC_LOCATION: &str = "[rust-os]   location:";
    /// Panic report line saying whether the on-screen banner was drawn.
    pub const PANIC_BANNER_DRAWN: &str = "[rust-os]   on-screen panic banner: drawn";
    /// Last line of a panic report.
    pub const PANIC_HALTED: &str = "[rust-os]   CPU halted";
    /// Message of the panic triggered by the kernel's `deliberate-panic`
    /// feature.
    pub const DELIBERATE_PANIC_MESSAGE: &str =
        "deliberate panic requested by the deliberate-panic feature";
}
