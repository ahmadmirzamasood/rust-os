//! Bounded QEMU Q35 platform backend; no AML interpreter or suspend support.
//! Register contracts: QEMU hw/acpi/{ich9,core}.c and hw/isa/lpc_ich9.c.
use crate::arch::x86_64::{self as arch, storage_io};
use core::sync::atomic::{AtomicU16, Ordering};
use os_abi::Error;

static CONTROL: AtomicU16 = AtomicU16::new(0);

fn config(address: u32) -> u32 {
    // SAFETY: boot-time exclusive PCI mechanism-1 access, aligned dword.
    unsafe { storage_io::outl(0xcf8, address) };
    // SAFETY: reads the configuration register selected immediately above.
    unsafe { storage_io::inl(0xcfc) }
}

fn qemu_signature() -> bool {
    // SAFETY: after firmware exit, no other guest code uses fw_cfg. Select
    // its read-only signature item at the documented x86 selector port.
    unsafe { storage_io::outw(0x510, 0) };
    let mut signature = [0; 4];
    for byte in &mut signature {
        // SAFETY: fw_cfg's byte data port advances the selected item cursor;
        // four bounded reads, no DMA, guest memory or firmware tables accessed.
        *byte = unsafe { arch::inb(0x511) };
    }
    signature == *b"QEMU"
}

pub fn init() {
    // Explicit supported virtual platform, not a generic Intel hardware claim.
    let lpc = 0x8000_f800;
    if config(0x8000_0000) == 0x29c0_8086 && config(lpc) == 0x2918_8086 && qemu_signature() {
        let base = config(lpc | 0x40);
        let enabled = config(lpc | 0x44) & 0x80 != 0;
        if enabled && base & 1 != 0 && base & 0xffff_0000 == 0 && base & 0xff80 >= 0x400 {
            CONTROL.store(((base & 0xff80) + 4) as u16, Ordering::Release);
        }
    }
    crate::kprintln!(
        "[rust-os] power: Q35 PM1 control={:#x}; sleep unsupported",
        CONTROL.load(Ordering::Acquire)
    );
}

pub fn capabilities() -> u64 {
    if CONTROL.load(Ordering::Acquire) != 0 {
        3
    } else {
        0
    }
}

/// Called with preemption inhibited after successful final volume flush.
/// Returns an error if hardware does not complete the transition in two seconds.
pub fn transition(action: u64) -> Result<u64, Error> {
    if !(1..=2).contains(&action) {
        return Err(Error::InvalidArgument);
    }
    let port = CONTROL.load(Ordering::Acquire);
    if port == 0 {
        return Err(Error::NotFound);
    }
    crate::kprintln!("[rust-os] POWER CLEAN action={}", action);
    if action == 1 {
        // SAFETY: detected Q35 owns PM1a_CNT at decoded PMBASE+4. QEMU S5
        // has SLP_TYP=0; set SLP_EN, preserve SCI_EN and unrelated bits.
        let old = unsafe { storage_io::inw(port) };
        // SAFETY: same exclusively owned PM1 control register, S5 request.
        unsafe { storage_io::outw(port, (old & !0x3c00) | 0x2000) };
    } else {
        // SAFETY: detected ICH9 reset-control port; SYS_RST then RST_CPU
        // edge requests a full platform reset. No triple-fault fallback.
        unsafe { arch::outb(0xcf9, 2) };
        // SAFETY: same detected reset register, rising RST_CPU edge.
        unsafe { arch::outb(0xcf9, 6) };
    }
    let deadline = crate::time::uptime_ns().saturating_add(1_000_000_000);
    while crate::time::uptime_ns() < deadline {
        core::hint::spin_loop();
    }
    if action == 2 {
        for _ in 0..100_000 {
            // SAFETY: PS/2 driver runs only in the inhibited UI thread;
            // IRQ handlers only read the data port, not this command port.
            if unsafe { arch::inb(0x64) } & 2 == 0 {
                // SAFETY: controller input buffer empty; pulse CPU reset.
                unsafe { arch::outb(0x64, 0xfe) };
                break;
            }
        }
    }
    let deadline = crate::time::uptime_ns().saturating_add(1_000_000_000);
    while crate::time::uptime_ns() < deadline {
        core::hint::spin_loop();
    }
    crate::kprintln!("[rust-os] POWER FAILED action={}", action);
    Err(Error::Io)
}
