//! x86_64 instructions the kernel cannot express in plain Rust.
//!
//! All inline and global assembly in the project-owned kernel lives in this
//! directory:
//!
//! - this file: one-instruction wrappers (below);
//! - [`exceptions`]: the CPU exception entry stubs (save registers, call
//!   Rust, restore, `iretq`);
//! - [`probe`]: memory-access probes whose faults the exception handler can
//!   recover from, used to test guard pages and page permissions;
//! - [`switch`]: thread context switch, new-thread trampoline and the
//!   `iretq` into ring 3.
//!
//! [`pic`], [`pit`] and [`interrupts`] use only the port I/O wrappers below.
//!
//! | Wrapper | Instruction | Why Rust cannot do it |
//! | --- | --- | --- |
//! | [`outb`] / [`inb`] | `out dx, al` / `in al, dx` | x86 port I/O has no memory-mapped form; needed for the 16550 UART |
//! | [`disable_interrupts`] / [`enable_interrupts`] | `cli` / `sti` | Mask or unmask maskable (external) interrupts |
//! | [`interrupts_enabled`] | `pushfq; pop` | Read RFLAGS.IF |
//! | [`enable_interrupts_and_wait`] | `sti; hlt` | Sleep until the next interrupt without losing a wake-up |
//! | [`halt_forever`] | `cli; hlt` | Stops the CPU for good |
//! | [`read_cr0`] / [`write_cr0`], [`read_cr4`] / [`write_cr4`] | `mov` to/from CR0, CR4 | Control registers (write protection, SMEP/SMAP) |
//! | [`read_cr2`] | `mov rax, cr2` | Faulting address of the last page fault |
//! | [`read_cr3`] / [`write_cr3`] | `mov` to/from CR3 | Physical address of the active PML4 (switching address spaces) |
//! | [`read_msr`] / [`write_msr`] | `rdmsr` / `wrmsr` | Model-specific registers (EFER.NXE) |
//! | [`invalidate_page`] | `invlpg` | Drops a stale TLB entry after a page-table edit |
//! | [`read_tsc`] | `rdtsc` | Time-stamp counter, the monotonic clock source |
//! | [`load_gdt`], [`load_idt`], [`load_task_register`], [`reload_segments`] | `lgdt`, `lidt`, `ltr`, far return + `mov` to segment registers | Install the kernel's descriptor tables |
//!
//! All of these require CPL 0. The bootloader enters the kernel in ring 0;
//! only user processes run in ring 3, entered through [`switch::enter_user`]
//! and leaving it only through interrupts, exceptions and the system-call
//! gate.

pub mod exceptions;
pub mod interrupts;
pub mod pic;
pub mod pit;
pub mod probe;
pub mod storage_io;
pub mod switch;
pub mod tables;

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
/// Called at entry (before the interrupt controller is programmed, whatever
/// the bootloader left in the flag) and in the panic handler. CPU exceptions
/// are not maskable and are handled by [`exceptions`].
///
/// Not `nomem`: this is a compiler barrier, so memory accesses meant to be
/// inside an interrupt-free section are not moved out of it.
pub fn disable_interrupts() {
    // SAFETY: `cli` is permitted at CPL 0, where the kernel runs. It only
    // changes RFLAGS.IF and touches no memory.
    unsafe {
        asm!("cli", options(nostack));
    }
}

/// Unmask maskable interrupts (set RFLAGS.IF).
///
/// Only call this once the IDT has a gate for every vector the programmed
/// interrupt controller can deliver ([`tables::init`] covers all 256) and
/// the controller is set up ([`pic::init`]). Not `nomem`, for the same
/// reason as [`disable_interrupts`].
pub fn enable_interrupts() {
    // SAFETY: `sti` is permitted at CPL 0. Every vector has a gate whose
    // stub saves and restores all registers it uses (see `exceptions`), so
    // interrupted code observes no change except time passing.
    unsafe {
        asm!("sti", options(nostack));
    }
}

/// Whether maskable interrupts are currently enabled (RFLAGS.IF).
pub fn interrupts_enabled() -> bool {
    let flags: u64;
    // SAFETY: `pushfq; pop` reads RFLAGS through the stack slot it pushes
    // and pops again, leaving RSP unchanged. The kernel target has no red
    // zone, so the push cannot clobber live data below RSP.
    unsafe {
        asm!("pushfq", "pop {}", out(reg) flags, options(nomem, preserves_flags));
    }
    flags & (1 << 9) != 0
}

/// Enable interrupts and halt until one arrives; returns with interrupts
/// enabled.
///
/// Call with interrupts *disabled* after checking that no work is pending.
/// `sti` takes effect only after the following instruction, so an interrupt
/// that became pending after the check is delivered after `hlt` starts and
/// wakes it: no wake-up is lost between the check and the halt.
pub fn enable_interrupts_and_wait() {
    // SAFETY: as in `enable_interrupts`; `hlt` only pauses the CPU until the
    // next interrupt. Not `nomem`: handlers run in between and may change
    // memory the caller reads afterwards.
    unsafe {
        asm!("sti", "hlt", options(nostack));
    }
}

/// Wait roughly a microsecond by writing to the unused POST-code port 0x80; used
/// between commands to slow legacy devices (8259 PIC).
pub fn io_wait() {
    // SAFETY: port 0x80 is the POST diagnostic port; nothing in the kernel
    // or on the q35 machine depends on its value.
    unsafe { outb(0x80, 0) };
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

/// Read CR0.
pub fn read_cr0() -> u64 {
    let value: u64;
    // SAFETY: reading CR0 is permitted at CPL 0 and has no side effects.
    unsafe { asm!("mov {}, cr0", out(reg) value, options(nomem, nostack, preserves_flags)) };
    value
}

/// Write CR0.
///
/// # Safety
///
/// CR0 controls paging, protection and caching. The caller may only change
/// bits whose effect it has checked (this kernel only ever *sets* WP).
pub unsafe fn write_cr0(value: u64) {
    // SAFETY: per the contract above. Not `nomem`: changing WP alters how
    // later memory accesses behave, so the compiler must not move them.
    unsafe { asm!("mov cr0, {}", in(reg) value, options(nostack, preserves_flags)) };
}

/// Read CR2: the linear address that caused the most recent page fault.
pub fn read_cr2() -> u64 {
    let value: u64;
    // SAFETY: reading CR2 is permitted at CPL 0 and has no side effects.
    unsafe { asm!("mov {}, cr2", out(reg) value, options(nomem, nostack, preserves_flags)) };
    value
}

/// Read CR3: physical address of the active PML4 plus flag bits.
pub fn read_cr3() -> u64 {
    let value: u64;
    // SAFETY: reading CR3 is permitted at CPL 0 and has no side effects.
    unsafe { asm!("mov {}, cr3", out(reg) value, options(nomem, nostack, preserves_flags)) };
    value
}

/// Load CR3 (switch address space). Flushes all non-global TLB entries.
///
/// # Safety
///
/// `value` must be the physical address of a valid PML4 that maps the
/// running kernel (code, stack, data) identically to the current one, and
/// that stays alive while loaded.
pub unsafe fn write_cr3(value: u64) {
    // SAFETY: per the contract above. Not `nomem`: every later memory access
    // goes through the new tables.
    unsafe { asm!("mov cr3, {}", in(reg) value, options(nostack, preserves_flags)) };
}

/// Read CR4.
pub fn read_cr4() -> u64 {
    let value: u64;
    // SAFETY: reading CR4 is permitted at CPL 0 and has no side effects.
    unsafe { asm!("mov {}, cr4", out(reg) value, options(nomem, nostack, preserves_flags)) };
    value
}

/// Write CR4.
///
/// # Safety
///
/// Setting a CR4 bit the CPU does not support raises #GP. The caller must
/// have checked support (CPUID) and that the new mode is compatible with
/// every existing mapping (for SMEP/SMAP: no user-accessible kernel pages).
pub unsafe fn write_cr4(value: u64) {
    // SAFETY: per the contract above; not `nomem` for the same reason as
    // `write_cr0`.
    unsafe { asm!("mov cr4, {}", in(reg) value, options(nostack, preserves_flags)) };
}

/// Read model-specific register `msr`.
///
/// # Safety
///
/// Reading an MSR the CPU does not implement raises #GP. The caller must
/// know `msr` exists (architectural MSRs such as EFER always do in long
/// mode).
pub unsafe fn read_msr(msr: u32) -> u64 {
    let (low, high): (u32, u32);
    // SAFETY: per the contract above; `rdmsr` touches no memory.
    unsafe {
        asm!("rdmsr", in("ecx") msr, out("eax") low, out("edx") high, options(nomem, nostack, preserves_flags));
    }
    (u64::from(high) << 32) | u64::from(low)
}

/// Write model-specific register `msr`.
///
/// # Safety
///
/// As [`read_msr`], and the new value must be valid for the register and
/// compatible with the running system (this kernel only sets EFER.NXE).
pub unsafe fn write_msr(msr: u32, value: u64) {
    let (low, high) = (value as u32, (value >> 32) as u32);
    // SAFETY: per the contract above. Not `nomem`: EFER.NXE changes how
    // page-table entries are interpreted.
    unsafe {
        asm!("wrmsr", in("ecx") msr, in("eax") low, in("edx") high, options(nostack, preserves_flags))
    };
}

/// Invalidate the TLB entry for the page containing `addr` on this CPU.
///
/// Always safe: dropping a cached translation only makes the CPU re-read the
/// page tables. (Single CPU: no other TLBs exist that would need a
/// shootdown.)
pub fn invalidate_page(addr: u64) {
    // SAFETY: `invlpg` is permitted at CPL 0 and only discards cached
    // translations. Not `nomem`: memory accesses after it must see the new
    // mapping, so the compiler must not reorder them across it.
    unsafe { asm!("invlpg [{}]", in(reg) addr, options(nostack, preserves_flags)) };
}

/// Read the time-stamp counter.
pub fn read_tsc() -> u64 {
    let (low, high): (u32, u32);
    // SAFETY: `rdtsc` is available on every x86_64 CPU and permitted at
    // CPL 0 (CR4.TSD only restricts ring 3); it reads a counter and touches
    // no memory.
    unsafe {
        asm!("rdtsc", out("eax") low, out("edx") high, options(nomem, nostack, preserves_flags));
    }
    (u64::from(high) << 32) | u64::from(low)
}

/// Operand of `lgdt` / `lidt`.
#[repr(C, packed(2))]
#[derive(Debug)]
pub struct DescriptorTablePointer {
    /// Size of the table in bytes, minus one.
    pub limit: u16,
    /// Linear address of the table.
    pub base: u64,
}

/// Load the global descriptor table register.
///
/// # Safety
///
/// `pointer` must describe a valid GDT that lives (unmoved, unchanged in the
/// entries in use) for as long as it is loaded, i.e. forever. Segment
/// registers keep their cached descriptors until reloaded.
pub unsafe fn load_gdt(pointer: &DescriptorTablePointer) {
    // SAFETY: per the contract above; `lgdt` reads the 10-byte operand.
    unsafe { asm!("lgdt [{}]", in(reg) pointer, options(readonly, nostack, preserves_flags)) };
}

/// Load the interrupt descriptor table register.
///
/// # Safety
///
/// `pointer` must describe a valid IDT, every present gate must point at a
/// correct handler, and the table must live forever.
pub unsafe fn load_idt(pointer: &DescriptorTablePointer) {
    // SAFETY: per the contract above; `lidt` reads the 10-byte operand.
    unsafe { asm!("lidt [{}]", in(reg) pointer, options(readonly, nostack, preserves_flags)) };
}

/// Load the task register with TSS selector `selector`.
///
/// # Safety
///
/// `selector` must index an *available* 64-bit TSS descriptor in the loaded
/// GDT whose TSS lives forever. `ltr` marks the descriptor busy, so it can
/// only be loaded once.
pub unsafe fn load_task_register(selector: u16) {
    // SAFETY: per the contract above. `ltr` writes the busy bit in the GDT.
    unsafe { asm!("ltr {:x}", in(reg) selector, options(nostack, preserves_flags)) };
}

/// Reload CS with `code` (via a far return) and DS, ES, SS with `data`.
///
/// # Safety
///
/// The loaded GDT must contain a 64-bit ring-0 code segment at `code` and a
/// writable data segment at `data`.
pub unsafe fn reload_segments(code: u16, data: u16) {
    // SAFETY: per the contract above. The far return pops the pushed RIP and
    // CS, leaving the stack as it was; FS and GS (and their bases) are left
    // untouched.
    unsafe {
        asm!(
            "push {code}",
            "lea {tmp}, [rip + 2f]",
            "push {tmp}",
            "retfq",
            "2:",
            "mov ds, {data:x}",
            "mov es, {data:x}",
            "mov ss, {data:x}",
            code = in(reg) u64::from(code),
            data = in(reg) data,
            // `out`, not `lateout`: `tmp` is written while `data` is still
            // needed, so they must not share a register.
            tmp = out(reg) _,
            options(preserves_flags),
        );
    }
}

/// Bits of interest in CPUID leaf 7 / EBX and in control registers.
pub mod bits {
    pub const CR0_MP: u64 = 1 << 1;
    pub const CR0_EM: u64 = 1 << 2;
    pub const CR0_WP: u64 = 1 << 16;
    pub const CR4_OSFXSR: u64 = 1 << 9;
    pub const CR4_OSXMMEXCPT: u64 = 1 << 10;
    pub const CR4_UMIP: u64 = 1 << 11;
    pub const CR4_OSXSAVE: u64 = 1 << 18;
    pub const CR4_SMEP: u64 = 1 << 20;
    pub const CR4_SMAP: u64 = 1 << 21;
    pub const EFER: u32 = 0xC000_0080;
    pub const EFER_NXE: u64 = 1 << 11;
    pub const CPUID7_EBX_SMEP: u32 = 1 << 7;
    pub const CPUID7_EBX_SMAP: u32 = 1 << 20;
    pub const CPUID_EXT1_EDX_NX: u32 = 1 << 20;
}

/// `(eax, ebx, ecx, edx)` of CPUID `leaf`/`sub_leaf`, or zeros if `leaf` is
/// above the CPU's maximum for its range.
pub fn cpuid(leaf: u32, sub_leaf: u32) -> (u32, u32, u32, u32) {
    use core::arch::x86_64::__cpuid_count;
    let max = __cpuid_count(leaf & 0x8000_0000, 0).eax;
    if leaf > max {
        return (0, 0, 0, 0);
    }
    let r = __cpuid_count(leaf, sub_leaf);
    (r.eax, r.ebx, r.ecx, r.edx)
}

/// What [`disable_simd`] found and changed.
#[derive(Debug, Clone, Copy)]
pub struct SimdState {
    /// CR4.OSFXSR was set (by firmware) before.
    pub sse_was_enabled: bool,
    /// CR4.OSXSAVE was set before.
    pub xsave_was_enabled: bool,
    /// CR4.UMIP is set (ring 3 cannot use `sidt`/`sgdt`).
    pub umip: bool,
}

/// Turn off x87, SSE and AVX for all code: set CR0.EM (x87 instructions
/// raise #NM), clear CR0.MP, and clear CR4.OSFXSR, OSXMMEXCPT and OSXSAVE
/// (SSE/AVX instructions raise #UD).
///
/// The kernel and user programs are compiled soft-float, so nothing needs
/// these units, and the scheduler does not save their registers. Leaving
/// them enabled would let one process read register values another left
/// behind; disabled, a program that uses them is terminated instead.
pub fn disable_simd() -> SimdState {
    let cr4 = read_cr4();
    let state = SimdState {
        sse_was_enabled: cr4 & bits::CR4_OSFXSR != 0,
        xsave_was_enabled: cr4 & bits::CR4_OSXSAVE != 0,
        umip: cr4 & bits::CR4_UMIP != 0,
    };
    let cr0 = (read_cr0() | bits::CR0_EM) & !bits::CR0_MP;
    // SAFETY: EM/MP only change how x87/SSE instructions trap; the kernel is
    // compiled soft-float and executes none of them.
    unsafe { write_cr0(cr0) };
    let cr4 = cr4 & !(bits::CR4_OSFXSR | bits::CR4_OSXMMEXCPT | bits::CR4_OSXSAVE);
    // SAFETY: clearing these bits only makes SSE/AVX/XSAVE instructions
    // fault; no kernel code uses them (soft-float target).
    unsafe { write_cr4(cr4) };
    state
}
