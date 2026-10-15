//! Memory-access probes whose faults are recoverable.
//!
//! The self-tests must show that guard pages, unmapped pages, read-only and
//! no-execute pages really fault, without bringing the kernel down. Each
//! probe is a tiny assembly routine with a known faulting instruction. When
//! the exception dispatcher sees a page fault (or #GP) at exactly that
//! instruction, [`try_recover`] records the fault, points RIP at a `ret` and
//! sets RAX to [`FAULTED`], so the probe returns an error. A fault anywhere
//! else is still fatal; nothing else in the kernel can "survive" a fault.
//!
//! The execute probe is different: the fault happens at the *target* (the
//! fetched address), so it is only recovered while that exact target is
//! armed in [`EXEC_TARGET`], and only for an instruction-fetch page fault.

use core::arch::{asm, global_asm};
use core::sync::atomic::{AtomicU64, Ordering};

use super::exceptions::{ExceptionFrame, PAGE_FAULT, PageFaultCode};

/// Value a probe returns in RAX when its access faulted.
const FAULTED: u64 = u64::MAX;

/// Details of a recovered fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fault {
    pub vector: u64,
    /// CR2 (page faults) or 0.
    pub address: u64,
    pub error_code: u64,
}

impl Fault {
    pub fn is_page_fault(&self) -> bool {
        self.vector == PAGE_FAULT as u64
    }

    /// The page was present (so this was a permission violation).
    pub fn page_present(&self) -> bool {
        self.error_code & PageFaultCode::PRESENT != 0
    }

    pub fn was_write(&self) -> bool {
        self.error_code & PageFaultCode::WRITE != 0
    }

    pub fn was_instruction_fetch(&self) -> bool {
        self.error_code & PageFaultCode::INSTRUCTION_FETCH != 0
    }
}

static LAST_VECTOR: AtomicU64 = AtomicU64::new(0);
static LAST_ADDRESS: AtomicU64 = AtomicU64::new(0);
static LAST_ERROR: AtomicU64 = AtomicU64::new(0);
/// Address the execute probe is currently calling (0: none).
static EXEC_TARGET: AtomicU64 = AtomicU64::new(0);

global_asm!(
    r#"
.section .text.rust_os_probe, "ax"
.p2align 4
.globl rust_os_probe_read
.hidden rust_os_probe_read
rust_os_probe_read:
.globl rust_os_probe_read_access
.hidden rust_os_probe_read_access
rust_os_probe_read_access:
    movzx eax, byte ptr [rdi]
    ret

.p2align 4
.globl rust_os_probe_write
.hidden rust_os_probe_write
rust_os_probe_write:
.globl rust_os_probe_write_access
.hidden rust_os_probe_write_access
rust_os_probe_write_access:
    mov byte ptr [rdi], sil
    xor eax, eax
    ret

.p2align 4
.globl rust_os_probe_execute
.hidden rust_os_probe_execute
rust_os_probe_execute:
    call rdi
    xor eax, eax
    ret

.p2align 4
.globl rust_os_probe_fault_return
.hidden rust_os_probe_fault_return
rust_os_probe_fault_return:
    ret
.section .text
"#
);

unsafe extern "C" {
    fn rust_os_probe_read(addr: u64) -> u64;
    fn rust_os_probe_write(addr: u64, value: u8) -> u64;
    fn rust_os_probe_execute(addr: u64) -> u64;
}

macro_rules! label_address {
    ($name:literal) => {{
        let addr: u64;
        // SAFETY: `lea` only computes the address of a label defined in the
        // global assembly above.
        unsafe {
            asm!(
                concat!("lea {}, [rip + ", $name, "]"),
                out(reg) addr,
                options(nomem, nostack, preserves_flags),
            )
        };
        addr
    }};
}

fn last_fault() -> Fault {
    Fault {
        vector: LAST_VECTOR.load(Ordering::SeqCst),
        address: LAST_ADDRESS.load(Ordering::SeqCst),
        error_code: LAST_ERROR.load(Ordering::SeqCst),
    }
}

/// Read one byte at `addr`, reporting a fault instead of dying.
///
/// # Safety
///
/// If `addr` is mapped, it must be ordinary RAM (reading device memory can
/// have side effects).
pub unsafe fn read_u8(addr: u64) -> Result<u8, Fault> {
    // SAFETY: the routine reads one byte at `addr` and returns it; a fault
    // there is recovered by `try_recover`. Device memory is excluded by the
    // caller.
    let value = unsafe { rust_os_probe_read(addr) };
    if value == FAULTED {
        Err(last_fault())
    } else {
        Ok(value as u8)
    }
}

/// Write one byte at `addr`, reporting a fault instead of dying.
///
/// # Safety
///
/// If the write succeeds it modifies memory: `addr` must be unmapped, not
/// writable, or a byte the caller owns.
pub unsafe fn write_u8(addr: u64, value: u8) -> Result<(), Fault> {
    // SAFETY: per the contract above; a fault is recovered by `try_recover`.
    let result = unsafe { rust_os_probe_write(addr, value) };
    if result == FAULTED {
        Err(last_fault())
    } else {
        Ok(())
    }
}

/// Call `addr` as a function, reporting an instruction-fetch page fault
/// instead of dying.
///
/// # Safety
///
/// `addr` must be unmapped, non-executable, or the start of code that
/// returns normally and follows the C calling convention.
pub unsafe fn execute(addr: u64) -> Result<(), Fault> {
    EXEC_TARGET.store(addr, Ordering::SeqCst);
    // SAFETY: per the contract above; an instruction-fetch fault at the armed
    // target is recovered by `try_recover`.
    let result = unsafe { rust_os_probe_execute(addr) };
    EXEC_TARGET.store(0, Ordering::SeqCst);
    if result == FAULTED {
        Err(last_fault())
    } else {
        Ok(())
    }
}

/// Recover from a fault raised by a probe. Called by the exception
/// dispatcher for page faults and #GP; returns whether `frame` was fixed up
/// to resume in the probe's error path.
pub fn try_recover(frame: &mut ExceptionFrame, cr2: u64) -> bool {
    let read_access = label_address!("rust_os_probe_read_access");
    let write_access = label_address!("rust_os_probe_write_access");
    let fault_return = label_address!("rust_os_probe_fault_return");
    let exec_target = EXEC_TARGET.load(Ordering::SeqCst);

    let at_access = frame.rip == read_access || frame.rip == write_access;
    let at_exec_target = frame.vector == PAGE_FAULT as u64
        && frame.error_code & PageFaultCode::INSTRUCTION_FETCH != 0
        && exec_target != 0
        && frame.rip == exec_target;
    if !at_access && !at_exec_target {
        return false;
    }
    LAST_VECTOR.store(frame.vector, Ordering::SeqCst);
    LAST_ADDRESS.store(
        if frame.vector == PAGE_FAULT as u64 {
            cr2
        } else {
            0
        },
        Ordering::SeqCst,
    );
    LAST_ERROR.store(frame.error_code, Ordering::SeqCst);
    if at_exec_target {
        // The `call` pushed a return address into the probe; drop it so the
        // `ret` below returns straight to the probe's caller.
        frame.rsp += 8;
    }
    frame.rip = fault_return;
    frame.rax = FAULTED;
    true
}
