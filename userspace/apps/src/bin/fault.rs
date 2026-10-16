//! `fault`: crashes on purpose, in the way named by the first message on its
//! startup channel. The kernel must terminate it and report the exception
//! while every other process and the kernel keep running.
//!
//! | Mode | What it does | Expected exception |
//! | --- | --- | --- |
//! | `null-read` | reads address 0 | #PF (not present) |
//! | `kernel-read` | reads a mapped kernel page (the IDT, found with `sidt`) | #PF (protection) |
//! | `kernel-write` | writes that kernel page | #PF (protection) |
//! | `kernel-exec` | jumps into that kernel page | #PF (instruction fetch) |
//! | `privileged` | executes `cli` | #GP |
//! | `port-io` | reads I/O port 0x60 | #GP |
//! | `invalid-opcode` | executes `ud2` | #UD |
//! | `sse` | executes an SSE instruction (SIMD is disabled) | #UD |
//! | `stack-overflow` | recurses into the stack guard page | #PF (not present) |
//!
//! The assembly below exists only to produce these faults.
//!
//! Exit codes (when no fault happened): 2 unknown mode, 3 no channel,
//! 4 the fault did not happen.

#![no_std]
#![no_main]

use core::arch::asm;

use ulib::limits;

ulib::entry!(main);

fn main() -> i32 {
    let Some(channel) = ulib::startup_channel() else {
        return 3;
    };
    let mut mode = [0u8; limits::MESSAGE_MAX];
    let Ok(len) = channel.recv(&mut mode) else {
        return 3;
    };
    match &mode[..len] {
        b"null-read" => {
            let null = core::ptr::null::<u64>();
            // SAFETY: deliberately invalid: page 0 is never mapped in a
            // process, so this read faults and the process is terminated.
            let value = unsafe { core::ptr::read_volatile(null) };
            core::hint::black_box(value);
        }
        b"kernel-read" => {
            let target = kernel_address();
            // SAFETY: deliberately invalid: the IDT is mapped supervisor-only,
            // so this user-mode read faults.
            let value = unsafe { core::ptr::read_volatile(target as *const u64) };
            core::hint::black_box(value);
        }
        b"kernel-write" => {
            let target = kernel_address();
            // SAFETY: deliberately invalid, as above; the write never happens.
            unsafe { core::ptr::write_volatile(target as *mut u64, 0) };
        }
        b"kernel-exec" => {
            let target = kernel_address();
            // SAFETY: deliberately invalid: jumping to a supervisor page
            // faults on the instruction fetch, before anything executes.
            unsafe { asm!("jmp {}", in(reg) target, options(noreturn)) };
        }
        b"privileged" => {
            // SAFETY: `cli` raises #GP at CPL 3 (IOPL is 0); nothing changes.
            unsafe { asm!("cli", options(nomem, nostack)) };
        }
        b"port-io" => {
            let value: u8;
            // SAFETY: port I/O raises #GP at CPL 3 (IOPL 0, no I/O bitmap).
            unsafe { asm!("in al, 0x60", out("al") value, options(nomem, nostack)) };
            core::hint::black_box(value);
        }
        b"invalid-opcode" => {
            // SAFETY: `ud2` always raises #UD.
            unsafe { asm!("ud2", options(nomem, nostack, noreturn)) };
        }
        b"sse" => {
            // SAFETY: the kernel disables SSE (CR4.OSFXSR clear, CR0.EM set),
            // so this raises #UD before touching any register.
            unsafe { asm!("xorps xmm0, xmm0", options(nomem, nostack)) };
        }
        b"stack-overflow" => {
            core::hint::black_box(recurse(0));
        }
        _ => return 2,
    }
    4
}

/// Address of the interrupt descriptor table: a page that is certainly
/// mapped in every address space, but only for the kernel.
fn kernel_address() -> u64 {
    let mut idtr = [0u8; 10];
    // SAFETY: `sidt` stores the 10-byte IDTR into the buffer; it is
    // permitted at CPL 3 because the kernel does not enable UMIP.
    unsafe { asm!("sidt [{}]", in(reg) idtr.as_mut_ptr(), options(nostack)) };
    let mut base = [0u8; 8];
    base.copy_from_slice(&idtr[2..]);
    u64::from_le_bytes(base)
}

#[allow(unconditional_recursion)]
fn recurse(depth: u64) -> u64 {
    let frame = core::hint::black_box([depth; 32]);
    recurse(depth + 1).wrapping_add(core::hint::black_box(frame[0]))
}
