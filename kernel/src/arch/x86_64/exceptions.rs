//! CPU exception entry and reporting.
//!
//! # Entry stubs (global assembly)
//!
//! For each vector 0..32 a stub makes the stack uniform (pushes a zero error
//! code if the CPU did not push one, then the vector number) and jumps to a
//! common path that saves all fifteen general-purpose registers, calls
//! [`rust_os_exception_dispatch`] with a pointer to the resulting
//! [`ExceptionFrame`], restores the registers, drops vector and error code
//! and returns with `iretq`. The dispatcher may edit the frame (the probe
//! fixups in [`super::probe`] change RIP/RAX/RSP) before it is restored.
//!
//! Flags: the common path loads RFLAGS with 0 before calling Rust. Interrupt
//! gates clear only IF, TF and NT; this also clears DF (the ABI requires it)
//! and AC, which ring 3 may set to suspend SMAP for the kernel while it
//! handles that process's interrupts and system calls. The interrupted
//! RFLAGS is in the frame and restored by `iretq`.
//!
//! Stack alignment: in 64-bit mode the CPU aligns RSP to 16 bytes before
//! pushing its 5-word frame; the stub adds error code + vector (2 words) and
//! 15 registers, 22 words in total, so RSP is 16-byte aligned again at the
//! `call`, as the System V ABI requires. The kernel target disables the red
//! zone, so interrupted code keeps nothing below RSP.
//!
//! Vectors 32..=255 (external interrupts) have stubs of their own, generated
//! with `.irp` into one block at a fixed 16-byte stride, so the IDT can be
//! built from the block's address ([`interrupt_entry_points`]). They push a
//! zero error code and the vector and share the same common path; the
//! dispatcher hands them to [`super::interrupts::handle`] (or, for vector
//! 0x80, to the system-call dispatcher) and returns. The
//! kernel target is soft-float (no SSE/x87 state is touched by Rust code), so
//! the fifteen general-purpose registers are the whole interrupted state.
//!
//! # Policy
//!
//! - An exception raised in ring 3 (the saved CS has RPL 3) is the user
//!   process's fault: it is reported and the process is terminated; the
//!   kernel and every other process continue.
//! - A page fault or #GP raised by a probe instruction is recovered: the
//!   probe returns an error and the kernel continues (used by self-tests).
//! - Every other exception (one raised by kernel code) is fatal: a report
//!   goes to the serial port (no allocation, no lock waits), then the kernel
//!   panics, which draws the on-screen banner and halts.
//! - The double fault runs on its own IST stack, so a kernel stack overflow
//!   (which faults again while delivering the page fault) is still reported.

use core::arch::global_asm;
use core::fmt;

use boot_data::markers;

use super::{probe, read_cr2};
use crate::kprintln;

pub const PAGE_FAULT: usize = 14;
pub const DOUBLE_FAULT: usize = 8;
pub const GENERAL_PROTECTION: usize = 13;

/// Registers saved by the entry stub, in memory order (lowest address
/// first), followed by the vector, the error code and the CPU's frame.
#[repr(C)]
#[derive(Debug)]
pub struct ExceptionFrame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rbp: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,
    pub vector: u64,
    pub error_code: u64,
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

const _: () = assert!(core::mem::size_of::<ExceptionFrame>() == 22 * 8);

// Vectors for which the CPU pushes an error code: #DF 8, #TS 10, #NP 11,
// #SS 12, #GP 13, #PF 14, #AC 17, #CP 21, #VC 29, #SX 30.
global_asm!(
    r#"
.macro RUST_OS_EXC_NOERR vector
    .p2align 4
    .globl rust_os_exc_\vector
    .hidden rust_os_exc_\vector
    rust_os_exc_\vector:
        push 0
        push \vector
        jmp rust_os_exc_common
.endm
.macro RUST_OS_EXC_ERR vector
    .p2align 4
    .globl rust_os_exc_\vector
    .hidden rust_os_exc_\vector
    rust_os_exc_\vector:
        push \vector
        jmp rust_os_exc_common
.endm

.section .text.rust_os_exceptions, "ax"
RUST_OS_EXC_NOERR 0
RUST_OS_EXC_NOERR 1
RUST_OS_EXC_NOERR 2
RUST_OS_EXC_NOERR 3
RUST_OS_EXC_NOERR 4
RUST_OS_EXC_NOERR 5
RUST_OS_EXC_NOERR 6
RUST_OS_EXC_NOERR 7
RUST_OS_EXC_ERR 8
RUST_OS_EXC_NOERR 9
RUST_OS_EXC_ERR 10
RUST_OS_EXC_ERR 11
RUST_OS_EXC_ERR 12
RUST_OS_EXC_ERR 13
RUST_OS_EXC_ERR 14
RUST_OS_EXC_NOERR 15
RUST_OS_EXC_NOERR 16
RUST_OS_EXC_ERR 17
RUST_OS_EXC_NOERR 18
RUST_OS_EXC_NOERR 19
RUST_OS_EXC_NOERR 20
RUST_OS_EXC_ERR 21
RUST_OS_EXC_NOERR 22
RUST_OS_EXC_NOERR 23
RUST_OS_EXC_NOERR 24
RUST_OS_EXC_NOERR 25
RUST_OS_EXC_NOERR 26
RUST_OS_EXC_NOERR 27
RUST_OS_EXC_NOERR 28
RUST_OS_EXC_ERR 29
RUST_OS_EXC_ERR 30
RUST_OS_EXC_NOERR 31

// External-interrupt stubs: one per vector 32..=255, each padded to exactly
// 16 bytes (push imm8/imm32 + push imm + jmp rel32 <= 12 bytes), so stub v is
// at rust_os_irq_stubs + (v - 32) * 16. `interrupt_entry_points` checks the
// block's total size before the IDT uses it.
.p2align 4
.globl rust_os_irq_stubs
.hidden rust_os_irq_stubs
rust_os_irq_stubs:
// (LLVM does not accept line continuations, hence one long line.)
.irp vector, 32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56,57,58,59,60,61,62,63,64,65,66,67,68,69,70,71,72,73,74,75,76,77,78,79,80,81,82,83,84,85,86,87,88,89,90,91,92,93,94,95,96,97,98,99,100,101,102,103,104,105,106,107,108,109,110,111,112,113,114,115,116,117,118,119,120,121,122,123,124,125,126,127,128,129,130,131,132,133,134,135,136,137,138,139,140,141,142,143,144,145,146,147,148,149,150,151,152,153,154,155,156,157,158,159,160,161,162,163,164,165,166,167,168,169,170,171,172,173,174,175,176,177,178,179,180,181,182,183,184,185,186,187,188,189,190,191,192,193,194,195,196,197,198,199,200,201,202,203,204,205,206,207,208,209,210,211,212,213,214,215,216,217,218,219,220,221,222,223,224,225,226,227,228,229,230,231,232,233,234,235,236,237,238,239,240,241,242,243,244,245,246,247,248,249,250,251,252,253,254,255
    .p2align 4
    push 0
    push \vector
    jmp rust_os_exc_common
.endr
.p2align 4
.globl rust_os_irq_stubs_end
.hidden rust_os_irq_stubs_end
rust_os_irq_stubs_end:

.p2align 4
rust_os_exc_common:
    push rax
    push rbx
    push rcx
    push rdx
    push rsi
    push rdi
    push rbp
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15
    push 0
    popfq
    mov rdi, rsp
    call {dispatch}
    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rbp
    pop rdi
    pop rsi
    pop rdx
    pop rcx
    pop rbx
    pop rax
    add rsp, 16
    iretq

.section .text
"#,
    dispatch = sym rust_os_exception_dispatch,
);

/// Address of the entry stub for `vector`, resolved by a RIP-relative `lea`
/// so it is correct wherever the bootloader placed the kernel.
macro_rules! entry_address {
    ($n:literal) => {{
        let addr: u64;
        // SAFETY: `lea` only computes an address; the symbol is defined in
        // the global assembly above.
        unsafe {
            core::arch::asm!(
                concat!("lea {}, [rip + rust_os_exc_", stringify!($n), "]"),
                out(reg) addr,
                options(nomem, nostack, preserves_flags),
            )
        };
        addr
    }};
}

/// Entry-stub addresses for vectors 0..32, for building the IDT.
pub fn entry_points() -> [u64; super::tables::EXCEPTION_VECTORS] {
    [
        entry_address!(0),
        entry_address!(1),
        entry_address!(2),
        entry_address!(3),
        entry_address!(4),
        entry_address!(5),
        entry_address!(6),
        entry_address!(7),
        entry_address!(8),
        entry_address!(9),
        entry_address!(10),
        entry_address!(11),
        entry_address!(12),
        entry_address!(13),
        entry_address!(14),
        entry_address!(15),
        entry_address!(16),
        entry_address!(17),
        entry_address!(18),
        entry_address!(19),
        entry_address!(20),
        entry_address!(21),
        entry_address!(22),
        entry_address!(23),
        entry_address!(24),
        entry_address!(25),
        entry_address!(26),
        entry_address!(27),
        entry_address!(28),
        entry_address!(29),
        entry_address!(30),
        entry_address!(31),
    ]
}

/// Size of one external-interrupt stub in the generated block.
const INTERRUPT_STUB_STRIDE: u64 = 16;

/// Entry-stub addresses for vectors 32..=255, or `None` if the assembled
/// block does not have the expected fixed stride (then no gate may use it).
pub fn interrupt_entry_points() -> Option<[u64; 256 - super::tables::EXCEPTION_VECTORS]> {
    let start: u64;
    let end: u64;
    // SAFETY: `lea` only computes addresses of symbols defined in the global
    // assembly above.
    unsafe {
        core::arch::asm!(
            "lea {start}, [rip + rust_os_irq_stubs]",
            "lea {end}, [rip + rust_os_irq_stubs_end]",
            start = out(reg) start,
            end = out(reg) end,
            options(nomem, nostack, preserves_flags),
        );
    }
    const COUNT: usize = 256 - super::tables::EXCEPTION_VECTORS;
    if end.checked_sub(start) != Some(COUNT as u64 * INTERRUPT_STUB_STRIDE) {
        return None;
    }
    let mut entries = [0u64; COUNT];
    for (i, entry) in entries.iter_mut().enumerate() {
        *entry = start + i as u64 * INTERRUPT_STUB_STRIDE;
    }
    Some(entries)
}

/// Mnemonic and name of exception `vector`.
pub fn name(vector: u64) -> (&'static str, &'static str) {
    match vector {
        0 => ("#DE", "divide error"),
        1 => ("#DB", "debug"),
        2 => ("NMI", "non-maskable interrupt"),
        3 => ("#BP", "breakpoint"),
        4 => ("#OF", "overflow"),
        5 => ("#BR", "bound range exceeded"),
        6 => ("#UD", "invalid opcode"),
        7 => ("#NM", "device not available"),
        8 => ("#DF", "double fault"),
        9 => ("--", "coprocessor segment overrun"),
        10 => ("#TS", "invalid TSS"),
        11 => ("#NP", "segment not present"),
        12 => ("#SS", "stack-segment fault"),
        13 => ("#GP", "general protection fault"),
        14 => ("#PF", "page fault"),
        16 => ("#MF", "x87 floating-point error"),
        17 => ("#AC", "alignment check"),
        18 => ("#MC", "machine check"),
        19 => ("#XM", "SIMD floating-point exception"),
        20 => ("#VE", "virtualization exception"),
        21 => ("#CP", "control protection exception"),
        28 => ("#HV", "hypervisor injection exception"),
        29 => ("#VC", "VMM communication exception"),
        30 => ("#SX", "security exception"),
        _ => ("--", "reserved vector"),
    }
}

/// Exceptions whose error code is a segment-selector index: #TS, #NP, #SS,
/// #GP (and #DF, whose code is always zero).
fn has_selector_error_code(vector: u64) -> bool {
    matches!(vector, 10..=13)
}

/// Human-readable selector error code (Intel SDM vol. 3, 6.13).
#[derive(Debug, Clone, Copy)]
pub struct SelectorErrorCode(pub u64);

impl fmt::Display for SelectorErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let c = self.0;
        let index = (c >> 3) & 0x1FFF;
        match (c >> 1) & 0b11 {
            0b00 => write!(f, "GDT entry {index}")?,
            0b10 => write!(f, "LDT entry {index}")?,
            _ => write!(f, "IDT vector {index}")?,
        }
        if c & 1 != 0 {
            f.write_str(", during delivery of an external event")?;
        }
        Ok(())
    }
}

/// Human-readable page-fault error code.
#[derive(Debug, Clone, Copy)]
pub struct PageFaultCode(pub u64);

impl PageFaultCode {
    pub const PRESENT: u64 = 1 << 0;
    pub const WRITE: u64 = 1 << 1;
    pub const USER: u64 = 1 << 2;
    pub const RESERVED_BIT: u64 = 1 << 3;
    pub const INSTRUCTION_FETCH: u64 = 1 << 4;
}

impl fmt::Display for PageFaultCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let c = self.0;
        f.write_str(if c & Self::PRESENT != 0 {
            "protection violation"
        } else {
            "page not present"
        })?;
        f.write_str(if c & Self::INSTRUCTION_FETCH != 0 {
            ", instruction fetch"
        } else if c & Self::WRITE != 0 {
            ", write"
        } else {
            ", read"
        })?;
        f.write_str(if c & Self::USER != 0 {
            ", user mode"
        } else {
            ", kernel mode"
        })?;
        if c & Self::RESERVED_BIT != 0 {
            f.write_str(", reserved bit set in a page-table entry")?;
        }
        Ok(())
    }
}

/// Called by the entry stubs with interrupts disabled (interrupt gates).
///
/// - The system-call vector goes to [`crate::syscall::dispatch`].
/// - External interrupts (other vectors from 32 up) go to
///   [`super::interrupts::handle`].
/// - Both may then switch threads ([`crate::sched::preempt_point`]): the
///   frame stays on this thread's kernel stack until it resumes.
/// - An exception raised in ring 3 terminates only the current process
///   ([`crate::process::user_fault`]).
/// - A page fault or #GP from a probe instruction is recovered.
/// - Every other exception is a kernel bug and ends in a panic.
///
/// The interrupt and fatal paths must not allocate or wait on locks: they
/// can interrupt any kernel code, including the allocator.
#[unsafe(no_mangle)]
extern "C" fn rust_os_exception_dispatch(frame: &mut ExceptionFrame) {
    let vector = frame.vector as usize;
    if vector == usize::from(os_abi::syscall::VECTOR) {
        crate::syscall::dispatch(frame);
        crate::sched::preempt_point();
        return;
    }
    if vector >= super::tables::EXCEPTION_VECTORS {
        super::interrupts::handle(frame.vector);
        crate::sched::preempt_point();
        return;
    }
    let cr2 = read_cr2();
    if frame.cs & 3 == 3 {
        // Ring 3 caused it: only that process ends; the kernel continues.
        crate::process::user_fault(frame, cr2)
    }
    if (vector == PAGE_FAULT || vector == GENERAL_PROTECTION) && probe::try_recover(frame, cr2) {
        return;
    }
    report_fatal(frame, cr2)
}

fn report_fatal(frame: &ExceptionFrame, cr2: u64) -> ! {
    let (mnemonic, name) = name(frame.vector);
    kprintln!(
        "{} {} ({}), vector {}",
        markers::CPU_EXCEPTION,
        name,
        mnemonic,
        frame.vector
    );
    kprintln!("[rust-os]   error code: {:#x}", frame.error_code);
    if frame.vector as usize == PAGE_FAULT {
        kprintln!("[rust-os]   cause: {}", PageFaultCode(frame.error_code));
    } else if has_selector_error_code(frame.vector) && frame.error_code != 0 {
        kprintln!(
            "[rust-os]   selector: {}",
            SelectorErrorCode(frame.error_code)
        );
    }
    let context = super::interrupts::current_irq();
    kprintln!(
        "{} {}; interrupts were {} (RFLAGS.IF); uptime {} ticks",
        markers::FAULT_CONTEXT,
        match context {
            Some(irq) => super::interrupts::irq_name(irq),
            None => "normal kernel code (not an interrupt handler)",
        },
        if frame.rflags & (1 << 9) != 0 {
            "enabled"
        } else {
            "disabled"
        },
        crate::time::ticks()
    );
    if matches!(frame.vector as usize, PAGE_FAULT | DOUBLE_FAULT) {
        kprintln!("{} {:#018x}", markers::FAULT_ADDRESS, cr2);
    }
    kprintln!(
        "[rust-os]   rip {:#018x}  cs {:#06x}  rflags {:#010x}",
        frame.rip,
        frame.cs,
        frame.rflags
    );
    kprintln!("[rust-os]   rsp {:#018x}  ss {:#06x}", frame.rsp, frame.ss);
    kprintln!(
        "[rust-os]   rax {:#018x} rbx {:#018x} rcx {:#018x} rdx {:#018x}",
        frame.rax,
        frame.rbx,
        frame.rcx,
        frame.rdx
    );
    kprintln!(
        "[rust-os]   rsi {:#018x} rdi {:#018x} rbp {:#018x} r8  {:#018x}",
        frame.rsi,
        frame.rdi,
        frame.rbp,
        frame.r8
    );
    // Name the memory involved if it is a known guard page. For a double
    // fault CR2 still holds the address of the page fault that escalated.
    let mut diagnosis = None;
    if matches!(frame.vector as usize, PAGE_FAULT | DOUBLE_FAULT) {
        diagnosis = crate::memory::describe_guard(cr2);
    }
    if diagnosis.is_none() {
        diagnosis = crate::memory::describe_guard(frame.rsp);
    }
    match diagnosis {
        Some(guard) => kprintln!("{} {}", markers::FAULT_DIAGNOSIS, guard),
        None => kprintln!("{} no known guard page involved", markers::FAULT_DIAGNOSIS),
    }
    panic!("unrecoverable CPU exception: {} ({})", name, mnemonic)
}
