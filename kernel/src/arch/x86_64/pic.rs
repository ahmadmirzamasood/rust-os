//! Legacy 8259A programmable interrupt controller pair.
//!
//! # Why the 8259 and not the APIC
//!
//! The kernel runs on one CPU in a VM. QEMU's q35 machine (ICH9) has a
//! master/slave 8259 pair, and after the firmware exits boot services the
//! local APIC is left in "virtual wire" mode, which passes 8259 interrupts to
//! the CPU through LINT0. That makes the 8259 the smallest working path: no
//! ACPI MADT parsing, no MMIO mapping of the local APIC or I/O APIC. The
//! APIC path (needed for SMP and MSI) is future work; this module is the only
//! place that knows which controller is in use.
//!
//! # Programming
//!
//! [`init`] remaps the master to vectors 32..40 and the slave to 40..48 (the
//! power-on mapping overlaps CPU exception vectors) and masks every line.
//! Drivers unmask the lines they own with [`unmask`]; IRQ 2 (the cascade)
//! is unmasked automatically when a slave line is.
//!
//! # Spurious interrupts
//!
//! A line that drops before the CPU acknowledges it makes the 8259 deliver
//! its lowest-priority vector (IRQ 7, or IRQ 15 on the slave) without
//! setting the in-service bit. [`is_spurious`] checks that bit; a spurious
//! IRQ 7 gets no EOI, a spurious IRQ 15 gets an EOI to the master only (the
//! master did see the cascade).

use core::sync::atomic::{AtomicU16, Ordering};

use super::{inb, io_wait, outb};

const MASTER_COMMAND: u16 = 0x20;
const MASTER_DATA: u16 = 0x21;
const SLAVE_COMMAND: u16 = 0xA0;
const SLAVE_DATA: u16 = 0xA1;

/// First vector of the master (IRQ 0).
pub const MASTER_OFFSET: u8 = 32;
/// First vector of the slave (IRQ 8).
pub const SLAVE_OFFSET: u8 = 40;
/// Number of IRQ lines.
pub const LINES: u8 = 16;
const CASCADE_IRQ: u8 = 2;

const ICW1_INIT: u8 = 0x10;
const ICW1_ICW4: u8 = 0x01;
const ICW4_8086: u8 = 0x01;
const OCW2_EOI: u8 = 0x20;
const OCW3_READ_ISR: u8 = 0x0B;

/// Current mask (bit set = line masked): master in the low byte. Kept in
/// memory so [`unmask`] never has to read the hardware register.
static MASK: AtomicU16 = AtomicU16::new(0xFFFF);

fn write(port: u16, value: u8) {
    // SAFETY: ports 0x20/0x21/0xA0/0xA1 belong to the 8259 pair, which only
    // this module programs. Callers keep interrupts disabled or only send
    // EOIs, which cannot disturb an initialization sequence (that runs once,
    // with interrupts disabled, before any line is unmasked).
    unsafe { outb(port, value) }
}

fn read(port: u16) -> u8 {
    // SAFETY: as in `write`; reading the mask or ISR has no side effects.
    unsafe { inb(port) }
}

/// Remap both controllers and mask every line. Call once, with interrupts
/// disabled, before [`unmask`].
pub fn init() {
    write(MASTER_COMMAND, ICW1_INIT | ICW1_ICW4);
    io_wait();
    write(SLAVE_COMMAND, ICW1_INIT | ICW1_ICW4);
    io_wait();
    write(MASTER_DATA, MASTER_OFFSET); // ICW2: vector base
    io_wait();
    write(SLAVE_DATA, SLAVE_OFFSET);
    io_wait();
    write(MASTER_DATA, 1 << CASCADE_IRQ); // ICW3: slave on IRQ 2
    io_wait();
    write(SLAVE_DATA, CASCADE_IRQ); // ICW3: cascade identity
    io_wait();
    write(MASTER_DATA, ICW4_8086);
    io_wait();
    write(SLAVE_DATA, ICW4_8086);
    io_wait();
    MASK.store(0xFFFF, Ordering::SeqCst);
    write_mask(0xFFFF);
}

fn write_mask(mask: u16) {
    write(MASTER_DATA, mask as u8);
    write(SLAVE_DATA, (mask >> 8) as u8);
}

/// Allow `irq` (0..16) to interrupt. Unmasking a slave line also unmasks the
/// cascade.
pub fn unmask(irq: u8) {
    if irq >= LINES {
        return;
    }
    let mut clear = 1u16 << irq;
    if irq >= 8 {
        clear |= 1 << CASCADE_IRQ;
    }
    let mask = MASK.fetch_and(!clear, Ordering::SeqCst) & !clear;
    write_mask(mask);
}

/// Current mask (bit set = masked), as last programmed.
pub fn mask() -> u16 {
    MASK.load(Ordering::SeqCst)
}

/// In-service register of both controllers (master in the low byte).
fn in_service() -> u16 {
    write(MASTER_COMMAND, OCW3_READ_ISR);
    write(SLAVE_COMMAND, OCW3_READ_ISR);
    u16::from(read(MASTER_COMMAND)) | (u16::from(read(SLAVE_COMMAND)) << 8)
}

/// Whether a delivery of IRQ 7 or 15 was spurious (its in-service bit is
/// clear). Always `false` for other lines.
pub fn is_spurious(irq: u8) -> bool {
    matches!(irq, 7 | 15) && in_service() & (1 << irq) == 0
}

/// Signal end of interrupt for `irq`. A spurious IRQ 15 still needs the
/// master's EOI; pass `slave_too = false` for it.
pub fn end_of_interrupt(irq: u8, slave_too: bool) {
    if irq >= 8 && slave_too {
        write(SLAVE_COMMAND, OCW2_EOI);
    }
    write(MASTER_COMMAND, OCW2_EOI);
}
