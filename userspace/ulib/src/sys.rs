//! The raw system-call instruction.
//!
//! This is the library's only assembly: `int 0x80` is the CPU entry into
//! the kernel and has no Rust equivalent. The register convention is
//! documented in [`os_abi::syscall`].

use core::arch::asm;

/// Execute system call `number` with up to four arguments and return the
/// raw `rax` value (decode it with [`os_abi::decode_result`]).
///
/// # Safety
///
/// Arguments that the call interprets as pointers must describe memory this
/// program may let the kernel access that way: readable for inputs, and for
/// outputs writable memory that no live Rust reference aliases. The kernel
/// rejects pointers that are not mapped user memory, but it cannot know
/// Rust's aliasing rules.
pub unsafe fn syscall(number: u64, a: u64, b: u64, c: u64, d: u64) -> u64 {
    let ret: u64;
    // SAFETY: `int 0x80` enters the kernel through its user-callable gate.
    // In ring 3 the CPU switches to the kernel stack, so the user stack is
    // not touched (`nostack`). The kernel restores every register except
    // `rax`, and RFLAGS, on return. Memory effects are covered by the
    // caller's contract (hence no `nomem`/`readonly`).
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") number => ret,
            in("rdi") a,
            in("rsi") b,
            in("rdx") c,
            in("r10") d,
            options(nostack, preserves_flags),
        );
    }
    ret
}
