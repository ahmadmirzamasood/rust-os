//! The kernel's own GDT, TSS and IDT.
//!
//! The bootloader's GDT has no TSS, so it cannot give the double-fault
//! handler a known-good stack. A kernel stack overflow would then fault while
//! pushing the page-fault frame, escalate to a double fault, fault again on
//! the same exhausted stack, and reset the machine without a word. This
//! module installs:
//!
//! - a GDT with a 64-bit ring-0 code segment, a data segment and a TSS;
//! - a TSS whose IST slot 1 holds the double-fault stack. It starts as a
//!   static emergency stack and is later replaced by a heap-independent stack
//!   with an unmapped guard page ([`set_double_fault_stack`]);
//! - an IDT with a gate for each of the 32 CPU exception vectors. Vectors 32
//!   and above (external interrupts) are outside the IDT limit; they stay
//!   masked (`cli`, no interrupt controller programmed).
//!
//! The tables are statics that the CPU reads for the rest of the kernel's
//! life; they are written only by [`init`] (once) and
//! [`set_double_fault_stack`] (one aligned field, single CPU).

use core::cell::UnsafeCell;
use core::mem::size_of;
use core::sync::atomic::{AtomicBool, Ordering};

use super::exceptions;
use super::{DescriptorTablePointer, load_gdt, load_idt, load_task_register, reload_segments};

pub const KERNEL_CODE_SELECTOR: u16 = 0x08;
pub const KERNEL_DATA_SELECTOR: u16 = 0x10;
pub const TSS_SELECTOR: u16 = 0x18;

/// IST slot (1-based, as encoded in gates) used for double faults.
pub const DOUBLE_FAULT_IST: u8 = 1;

/// Size of the static emergency stack used until the guarded one exists.
const EMERGENCY_STACK_SIZE: usize = 16 * 1024;

/// Number of IDT gates: the CPU exception vectors.
pub const EXCEPTION_VECTORS: usize = 32;

/// 64-bit task-state segment (Intel SDM vol. 3, 8.7).
#[repr(C, packed(4))]
struct TaskStateSegment {
    reserved0: u32,
    /// Stacks for privilege changes; unused (no ring 3 yet).
    privilege_stacks: [u64; 3],
    reserved1: u64,
    /// Interrupt stack table, slots 1..=7.
    interrupt_stacks: [u64; 7],
    reserved2: u64,
    reserved3: u16,
    io_map_base: u16,
}

const _: () = assert!(size_of::<TaskStateSegment>() == 104);

/// One 16-byte IDT gate.
#[repr(C)]
#[derive(Clone, Copy)]
struct Gate {
    offset_low: u16,
    selector: u16,
    /// Bits 0..=2: IST slot; rest reserved.
    ist: u8,
    /// Present, DPL 0, type 0xE (64-bit interrupt gate: clears IF on entry).
    attributes: u8,
    offset_mid: u16,
    offset_high: u32,
    reserved: u32,
}

const _: () = assert!(size_of::<Gate>() == 16);

impl Gate {
    const MISSING: Self = Self {
        offset_low: 0,
        selector: 0,
        ist: 0,
        attributes: 0,
        offset_mid: 0,
        offset_high: 0,
        reserved: 0,
    };

    fn interrupt(handler: u64, ist: u8) -> Self {
        Self {
            offset_low: handler as u16,
            selector: KERNEL_CODE_SELECTOR,
            ist: ist & 0x7,
            attributes: 0x8E,
            offset_mid: (handler >> 16) as u16,
            offset_high: (handler >> 32) as u32,
            reserved: 0,
        }
    }
}

#[repr(C, align(16))]
struct Tables {
    gdt: [u64; 5],
    tss: TaskStateSegment,
    idt: [Gate; EXCEPTION_VECTORS],
}

#[repr(C, align(16))]
struct EmergencyStack([u8; EMERGENCY_STACK_SIZE]);

/// Interior-mutable holder for data the CPU reads by address.
struct CpuData<T>(UnsafeCell<T>);

// SAFETY: written only by `init` (guarded by INITIALIZED, before the tables
// are loaded) and by `set_double_fault_stack` (single aligned-by-field store
// on the only CPU). Rust code never holds references into them afterwards;
// the CPU reads them by address.
unsafe impl<T> Sync for CpuData<T> {}

static TABLES: CpuData<Tables> = CpuData(UnsafeCell::new(Tables {
    gdt: [0; 5],
    tss: TaskStateSegment {
        reserved0: 0,
        privilege_stacks: [0; 3],
        reserved1: 0,
        interrupt_stacks: [0; 7],
        reserved2: 0,
        reserved3: 0,
        io_map_base: 0,
    },
    idt: [Gate::MISSING; EXCEPTION_VECTORS],
}));

static EMERGENCY_STACK: CpuData<EmergencyStack> =
    CpuData(UnsafeCell::new(EmergencyStack([0; EMERGENCY_STACK_SIZE])));

static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Flat 64-bit ring-0 code segment: present, DPL 0, code, readable, L=1.
const KERNEL_CODE: u64 = 0x00AF_9A00_0000_FFFF;
/// Flat ring-0 data segment: present, DPL 0, data, writable.
const KERNEL_DATA: u64 = 0x00CF_9200_0000_FFFF;

/// The two GDT words of an available 64-bit TSS descriptor for `base`.
fn tss_descriptor(base: u64) -> (u64, u64) {
    let limit = (size_of::<TaskStateSegment>() - 1) as u64;
    let low = (limit & 0xFFFF)
        | ((base & 0x00FF_FFFF) << 16)
        | (0x89 << 40) // present, DPL 0, type 9 = available 64-bit TSS
        | (((limit >> 16) & 0xF) << 48)
        | (((base >> 24) & 0xFF) << 56);
    (low, base >> 32)
}

/// Install the GDT, TSS and IDT. Returns `false` (and does nothing) if
/// called again.
pub fn init() -> bool {
    if INITIALIZED.swap(true, Ordering::SeqCst) {
        return false;
    }
    let tables = TABLES.0.get();
    let emergency_top = EMERGENCY_STACK.0.get() as u64 + EMERGENCY_STACK_SIZE as u64;
    let handlers = exceptions::entry_points();

    // SAFETY: INITIALIZED guarantees this runs once, before the CPU uses the
    // tables, so this is the only access; `tables` points to a static.
    let tables = unsafe { &mut *tables };
    tables.tss.io_map_base = size_of::<TaskStateSegment>() as u16;
    tables.tss.interrupt_stacks[usize::from(DOUBLE_FAULT_IST - 1)] = emergency_top;
    let (tss_low, tss_high) = tss_descriptor(&raw const tables.tss as u64);
    tables.gdt = [0, KERNEL_CODE, KERNEL_DATA, tss_low, tss_high];
    for (vector, gate) in tables.idt.iter_mut().enumerate() {
        let ist = if vector == exceptions::DOUBLE_FAULT {
            DOUBLE_FAULT_IST
        } else {
            0
        };
        *gate = Gate::interrupt(handlers[vector], ist);
    }

    let gdt = DescriptorTablePointer {
        limit: (size_of::<[u64; 5]>() - 1) as u16,
        base: &raw const tables.gdt as u64,
    };
    let idt = DescriptorTablePointer {
        limit: (size_of::<[Gate; EXCEPTION_VECTORS]>() - 1) as u16,
        base: &raw const tables.idt as u64,
    };
    // SAFETY: the GDT is a static with valid code/data/TSS descriptors at the
    // selectors used below; the TSS is a static; it is loaded exactly once
    // (INITIALIZED).
    unsafe { load_gdt(&gdt) };
    // SAFETY: the GDT just loaded has 64-bit code at 0x08, data at 0x10.
    unsafe { reload_segments(KERNEL_CODE_SELECTOR, KERNEL_DATA_SELECTOR) };
    // SAFETY: 0x18 is the available TSS descriptor built above.
    unsafe { load_task_register(TSS_SELECTOR) };
    // SAFETY: the IDT is a static whose 32 gates point at the entry stubs in
    // `exceptions`, which follow the interrupt calling convention.
    unsafe { load_idt(&idt) };
    true
}

/// Point IST slot 1 (double faults) at the stack whose top is `top`.
///
/// # Safety
///
/// `top` must be the 16-byte-aligned top of a mapped, writable stack that
/// nothing else uses and that stays mapped for as long as it is installed.
pub unsafe fn set_double_fault_stack(top: u64) {
    let tables = TABLES.0.get();
    let slot = usize::from(DOUBLE_FAULT_IST - 1);
    // SAFETY: `tables` points to the static; this only computes the field's
    // address (no reference is created to the packed TSS).
    let field = unsafe { &raw mut (*tables).tss.interrupt_stacks[slot] };
    // SAFETY: the field lies in the static TSS and is only 4-byte aligned
    // (packed), hence the unaligned write. Single CPU: the only concurrent
    // reader is the CPU itself on a double fault, which sees either the old
    // (still valid) or the new value.
    unsafe { field.write_unaligned(top) };
}

/// Address range `[bottom, top)` of the static emergency stack.
pub fn emergency_stack() -> (u64, u64) {
    let bottom = EMERGENCY_STACK.0.get() as u64;
    (bottom, bottom + EMERGENCY_STACK_SIZE as u64)
}
