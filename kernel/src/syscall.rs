//! System-call dispatch (`int 0x80` from ring 3).
//!
//! The entry stub saves every general-purpose register into an
//! [`ExceptionFrame`]; [`dispatch`] reads the call number and arguments from
//! it (convention in [`os_abi::syscall`]) and writes the encoded result to
//! `rax`. All other registers are restored unchanged.
//!
//! Each call validates its own arguments before acting: unknown numbers,
//! out-of-range lengths and flags, handles not in the caller's table, and
//! pointers that are not mapped user memory of the caller fail with the
//! documented [`Error`] and change nothing. The kernel never dereferences a
//! user pointer (see [`crate::memory::user`]).
//!
//! Calls run with interrupts enabled, so they can be preempted like user
//! code; blocking calls give up the CPU until woken.

use os_abi::{Error, ExitStatus, encode_result, limits, syscall as nr};

use crate::arch::x86_64::{self as arch, exceptions::ExceptionFrame};
use crate::{process, sched, time};

/// Handle one system call. Called by the exception dispatcher with
/// interrupts disabled; returns with interrupts disabled.
pub fn dispatch(frame: &mut ExceptionFrame) {
    if frame.cs & 3 != 3 {
        // The gate is user-callable, but the kernel itself never uses it.
        frame.rax = encode_result(Err(Error::InvalidArgument));
        return;
    }
    arch::enable_interrupts();
    let result = call(frame.rax, frame.rdi, frame.rsi, frame.rdx, frame.r10);
    arch::disable_interrupts();
    frame.rax = encode_result(result);
}

fn call(number: u64, a: u64, b: u64, c: u64, d: u64) -> Result<u64, Error> {
    match number {
        nr::ABI_VERSION => Ok(u64::from(os_abi::ABI_VERSION)),
        nr::EXIT => process::exit_current(ExitStatus::Exited(a as u32 as i32)),
        nr::DEBUG_WRITE => process::debug_write(a, b),
        nr::YIELD => {
            sched::yield_now();
            Ok(0)
        }
        nr::SLEEP => {
            sched::sleep_ms(a.min(limits::SLEEP_MAX_MS));
            Ok(0)
        }
        nr::CLOCK => Ok(time::uptime_ns()),
        nr::SPAWN => process::spawn(a, b, c),
        nr::WAIT => process::wait(a),
        nr::CLOSE => process::close(a),
        nr::CHANNEL_CREATE => process::channel_create(a),
        nr::CHANNEL_SEND => process::channel_send(a, b, c),
        nr::CHANNEL_RECV => process::channel_recv(a, b, c, d),
        nr::DISPLAY_INFO..=nr::DISPLAY_INPUT => process::display::call(number, a, b, c, d),
        nr::FS_SYNC_STEP | nr::FILE_OPEN..=nr::FS_TEST_SHUTDOWN => {
            process::files::call(number, a, b, c, d)
        }
        _ => Err(Error::UnknownSyscall),
    }
}
