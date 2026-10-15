//! Kernel entry point.
//!
//! Phase 0 scaffolding: this compiles for `x86_64-unknown-none` and links
//! against `bootloader_api`, but it has NOT been booted. It produces no serial
//! or framebuffer output and must not be described as a bootable kernel.
//! Diagnostics, a panic report path and a controlled `hlt` halt arrive with the
//! graphical-boot milestone.

#![no_std]
#![no_main]

use bootloader_api::{BootInfo, entry_point};

entry_point!(kernel_main);

fn kernel_main(_boot_info: &'static mut BootInfo) -> ! {
    // Reference the shared ABI so the kernel/user-space boundary is exercised
    // by the build from the start.
    let _ = os_abi::ABI_VERSION;
    idle()
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    idle()
}

/// Busy-wait forever. Deliberately avoids inline assembly until the boot
/// milestone introduces a documented `hlt` wrapper.
fn idle() -> ! {
    loop {
        core::hint::spin_loop();
    }
}
