//! Thread context switching and the first entry into ring 3.
//!
//! Three small assembly routines, because Rust cannot change the stack
//! pointer or return to a lower privilege level:
//!
//! - [`switch_context`] saves the callee-saved registers (`rbx`, `rbp`,
//!   `r12`-`r15`) on the current stack, stores the stack pointer, loads
//!   another thread's saved stack pointer, restores its callee-saved
//!   registers and returns on *its* stack. Everything else a thread needs is
//!   either caller-saved (the Rust caller already spilled it) or lives on its
//!   own stack (interrupt frames, return addresses). The kernel is
//!   soft-float, so there is no FPU/SIMD state to save.
//! - `rust_os_thread_trampoline` is where a new thread's first switch
//!   "returns" to: it aligns the stack and calls the scheduler's Rust entry.
//! - [`enter_user`] builds an `iretq` frame (user SS:RSP, RFLAGS with IF
//!   set and IOPL 0, user CS:RIP), clears every general-purpose register
//!   except the startup argument and drops to ring 3.
//!
//! All three run with interrupts disabled.

use core::arch::global_asm;

use super::tables::{USER_CODE_SELECTOR, USER_DATA_SELECTOR};

/// RFLAGS for a new user thread: IF (bit 9) set, reserved bit 1 set, IOPL 0.
const USER_RFLAGS: u64 = 0x202;

global_asm!(
    r#"
.section .text.rust_os_switch, "ax"
.p2align 4
.globl rust_os_switch_context
.hidden rust_os_switch_context
rust_os_switch_context:
    push rbp
    push rbx
    push r12
    push r13
    push r14
    push r15
    mov [rdi], rsp
    mov rsp, rsi
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbx
    pop rbp
    ret

.p2align 4
.globl rust_os_thread_trampoline
.hidden rust_os_thread_trampoline
rust_os_thread_trampoline:
    and rsp, -16
    call {start}
    ud2

.p2align 4
.globl rust_os_enter_user
.hidden rust_os_enter_user
rust_os_enter_user:
    push {user_ss}
    push rsi
    push {user_rflags}
    push {user_cs}
    push rdi
    mov rdi, rdx
    xor eax, eax
    xor ebx, ebx
    xor ecx, ecx
    xor edx, edx
    xor esi, esi
    xor ebp, ebp
    xor r8d, r8d
    xor r9d, r9d
    xor r10d, r10d
    xor r11d, r11d
    xor r12d, r12d
    xor r13d, r13d
    xor r14d, r14d
    xor r15d, r15d
    iretq

.section .text
"#,
    start = sym crate::sched::thread_entry,
    user_ss = const USER_DATA_SELECTOR as u64,
    user_cs = const USER_CODE_SELECTOR as u64,
    user_rflags = const USER_RFLAGS,
);

unsafe extern "C" {
    fn rust_os_switch_context(save_rsp: *mut u64, load_rsp: u64);
    fn rust_os_thread_trampoline();
    fn rust_os_enter_user(entry: u64, stack: u64, arg: u64) -> !;
}

/// Save the current thread's context, storing its stack pointer at
/// `save_rsp`, and resume the thread whose saved stack pointer is
/// `load_rsp`. Returns when some thread switches back to this one.
///
/// # Safety
///
/// - Interrupts must be disabled.
/// - `save_rsp` must be valid for a write and stay valid until this thread
///   is resumed; nothing else may access it in between.
/// - `load_rsp` must be a value saved by this function for a suspended
///   thread, or the top of a frame built by [`initial_frame`], on a stack
///   that stays mapped while the thread can run.
pub unsafe fn switch_context(save_rsp: *mut u64, load_rsp: u64) {
    // SAFETY: per the contract above; the routine preserves every register
    // the System V ABI requires and returns normally on the new stack.
    unsafe { rust_os_switch_context(save_rsp, load_rsp) }
}

/// Write the initial [`switch_context`] frame for a new thread just below
/// `stack_top` and return the stack pointer to resume it with. Its first
/// switch-in runs `crate::sched::thread_entry`.
///
/// # Safety
///
/// `stack_top` must be the 16-byte-aligned top of a mapped, writable stack
/// of at least one page that nothing else uses.
pub unsafe fn initial_frame(stack_top: u64) -> u64 {
    let frame = [
        0,                                                      // r15
        0,                                                      // r14
        0,                                                      // r13
        0,                                                      // r12
        0,                                                      // rbx
        0,                                                      // rbp
        rust_os_thread_trampoline as *const () as usize as u64, // return address
        0,                                                      // padding: keeps 16-byte alignment
    ];
    let rsp = stack_top - (frame.len() as u64) * 8;
    let ptr = core::ptr::with_exposed_provenance_mut::<u64>(rsp as usize);
    // SAFETY: the eight words lie just below `stack_top`, inside the
    // caller's exclusive, mapped, writable stack; `rsp` is 8-byte aligned.
    unsafe { ptr.copy_from_nonoverlapping(frame.as_ptr(), frame.len()) };
    rsp
}

/// Leave the kernel and start executing `entry` in ring 3 with stack
/// pointer `stack` and `rdi = arg`. Never returns: the thread re-enters the
/// kernel only through interrupts, exceptions and system calls, on the
/// stack installed with `tables::set_kernel_stack`.
///
/// # Safety
///
/// - Interrupts must be disabled (`iretq` enables them in ring 3).
/// - The active address space must map `entry` user-executable and the
///   stack user-writable (otherwise the process faults immediately, which
///   is contained, but the caller should have checked).
/// - RSP0 must point at this thread's kernel stack.
pub unsafe fn enter_user(entry: u64, stack: u64, arg: u64) -> ! {
    // SAFETY: per the contract above; `iretq` to CPL 3 cannot return here.
    unsafe { rust_os_enter_user(entry, stack, arg) }
}
