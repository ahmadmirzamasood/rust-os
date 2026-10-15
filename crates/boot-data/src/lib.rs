//! Boot information checks shared by the kernel and host tooling.
//!
//! `no_std` and host-testable. The kernel feeds the bootloader's memory map
//! through [`memory::MemoryMap::load`], which copies it into kernel-owned
//! storage and validates it; the host smoke test matches the serial
//! [`markers`] the kernel prints.

#![no_std]
#![forbid(unsafe_code)]

pub mod memory;

/// Serial-line markers. The kernel prints them; `cargo xtask smoke` matches
/// them. Changing one is a protocol change for the smoke test.
pub mod markers {
    /// Prefix of every kernel diagnostic line.
    pub const PREFIX: &str = "[rust-os]";
    /// First line the kernel prints after its serial port works.
    pub const KERNEL_ENTERED: &str = "[rust-os] kernel entered";
    /// Prefix of the framebuffer line, followed by ` <width>x<height> ...`.
    pub const FRAMEBUFFER: &str = "[rust-os] framebuffer:";
    /// Every stage succeeded and the welcome screen is drawn.
    pub const BOOT_OK: &str = "[rust-os] BOOT OK";
    /// The kernel initialized, but at least one stage (for example graphics)
    /// is unavailable. Serial diagnostics still work.
    pub const BOOT_DEGRADED: &str = "[rust-os] BOOT DEGRADED";
    /// Graphics could not be initialized; followed by the reason.
    pub const GRAPHICS_UNAVAILABLE: &str = "[rust-os] graphics unavailable:";
    /// Start of a kernel panic report.
    pub const PANIC: &str = "[rust-os] KERNEL PANIC";
    /// Panic report line giving the source location.
    pub const PANIC_LOCATION: &str = "[rust-os]   location:";
    /// Panic report line saying whether the on-screen banner was drawn.
    pub const PANIC_BANNER_DRAWN: &str = "[rust-os]   on-screen panic banner: drawn";
    /// Last line of a panic report.
    pub const PANIC_HALTED: &str = "[rust-os]   CPU halted";
    /// Message of the panic triggered by the kernel's `deliberate-panic`
    /// feature.
    pub const DELIBERATE_PANIC_MESSAGE: &str =
        "deliberate panic requested by the deliberate-panic feature";

    /// Memory management is set up; followed by the free-frame count.
    pub const MEMORY_READY: &str = "[rust-os] memory ready:";
    /// Memory management could not be set up; followed by the error.
    pub const MEMORY_INIT_FAILED: &str = "[rust-os] memory init FAILED:";
    /// Every memory self-test check passed.
    pub const MEMORY_SELFTEST_PASS: &str = "[rust-os] MEMORY SELF-TEST PASS";
    /// At least one memory self-test check failed.
    pub const MEMORY_SELFTEST_FAIL: &str = "[rust-os] MEMORY SELF-TEST FAIL";
    /// Prefix of one failed self-test check, followed by its name.
    pub const MEMORY_SELFTEST_CHECK_FAILED: &str = "[rust-os]   memory self-test check failed:";

    /// Start of a fatal CPU exception report, followed by the exception.
    pub const CPU_EXCEPTION: &str = "[rust-os] CPU EXCEPTION:";
    /// Exception report line with the faulting address (CR2).
    pub const FAULT_ADDRESS: &str = "[rust-os]   address (CR2):";
    /// Exception report line naming the memory involved.
    pub const FAULT_DIAGNOSIS: &str = "[rust-os]   diagnosis:";
    /// Printed just before a deliberate fault in fault-test builds.
    pub const DELIBERATE_FAULT: &str = "[rust-os] deliberate fault:";
    /// Diagnosis text for the boot stack's guard page.
    pub const BOOT_STACK_GUARD: &str = "guard page below the boot kernel stack";
    /// Diagnosis text for the double-fault stack's guard page.
    pub const DOUBLE_FAULT_STACK_GUARD: &str = "guard page below the double-fault stack";
    /// Exception report line naming the interrupted context (an IRQ handler
    /// or normal code), the interrupt flag and the uptime.
    pub const FAULT_CONTEXT: &str = "[rust-os]   context:";
    /// Context text for an exception inside the timer interrupt handler.
    pub const TIMER_IRQ_CONTEXT: &str = "IRQ 0 handler (PIT timer)";

    /// Interrupt controller, timer and PS/2 setup finished; followed by a
    /// summary.
    pub const INTERRUPTS_READY: &str = "[rust-os] interrupts ready:";
    /// Interrupt or input setup failed; followed by the reason.
    pub const INTERRUPTS_FAILED: &str = "[rust-os] interrupts FAILED:";
    /// Every input/interrupt self-test check passed.
    pub const INPUT_SELFTEST_PASS: &str = "[rust-os] INPUT SELF-TEST PASS";
    /// At least one input/interrupt self-test check failed.
    pub const INPUT_SELFTEST_FAIL: &str = "[rust-os] INPUT SELF-TEST FAIL";
    /// Prefix of one failed input self-test check, followed by its name.
    pub const INPUT_SELFTEST_CHECK_FAILED: &str = "[rust-os]   input self-test check failed:";
    /// Periodic timer line: `uptime <s> s (<ticks> ticks); ...`.
    pub const TIMER_HEARTBEAT: &str = "[rust-os] timer: uptime";
    /// The interactive screen is up: `button at <x>,<y> <w>x<h>; pointer at
    /// <x>,<y>`.
    pub const UI_READY: &str = "[rust-os] ui: ready;";
    /// A key event: ` press|release <name> (scancode 0x..)[ char 'c']`.
    pub const INPUT_KEY: &str = "[rust-os] input: key";
    /// A mouse button event: ` <left|right|middle> down|up at <x>,<y>`.
    pub const INPUT_MOUSE_BUTTON: &str = "[rust-os] input: mouse";
    /// The test button was clicked: ` (<count>) at <x>,<y>`.
    pub const UI_CLICK: &str = "[rust-os] ui: test button clicked";
    /// F9 pressed: the event loop stops consuming input for a while so the
    /// interrupt-side rings overflow.
    pub const STALL_BEGIN: &str = "[rust-os] input: overflow test: event loop stalled";
    /// The stall ended; followed by the ring counters.
    pub const STALL_END: &str = "[rust-os] input: overflow test: resumed;";
    /// After a stall, everything queued during it has been decoded and the
    /// rings are empty again; input sent from now on is not dropped.
    pub const STALL_DRAINED: &str = "[rust-os] input: overflow test: backlog drained;";

    /// Diagnosis text for the guard page below a thread's kernel stack.
    pub const THREAD_STACK_GUARD: &str = "guard page below a thread's kernel stack";
    /// The scheduler runs: followed by its policy.
    pub const SCHED_READY: &str = "[rust-os] scheduler ready:";
    /// The scheduler could not start; followed by the reason.
    pub const SCHED_FAILED: &str = "[rust-os] scheduler FAILED:";
    /// Every scheduler self-test check passed.
    pub const SCHED_SELFTEST_PASS: &str = "[rust-os] SCHED SELF-TEST PASS";
    /// At least one scheduler self-test check failed.
    pub const SCHED_SELFTEST_FAIL: &str = "[rust-os] SCHED SELF-TEST FAIL";
    /// Prefix of one failed scheduler self-test check.
    pub const SCHED_SELFTEST_CHECK_FAILED: &str = "[rust-os]   scheduler self-test check failed:";
    /// The boot program bundle was found and validated; followed by the
    /// program names.
    pub const PROGRAMS_READY: &str = "[rust-os] programs:";
    /// No usable program bundle; followed by the reason.
    pub const PROGRAMS_UNAVAILABLE: &str = "[rust-os] programs unavailable:";
    /// A process started: ` <name> (pid <n>) ...`.
    pub const PROCESS_STARTED: &str = "[rust-os] process started:";
    /// A process ended: ` <name> (pid <n>): exit code <c> | fault vector <v> ...`.
    pub const PROCESS_EXITED: &str = "[rust-os] process exited:";
    /// A process was terminated after a CPU exception in ring 3; followed by
    /// the process, the exception and the cause. Never fatal for the kernel.
    pub const USER_FAULT: &str = "[rust-os] USER FAULT:";
    /// Prefix of text a process wrote with `DEBUG_WRITE`
    /// (`[user <name>/<pid>] <text>`). User text never starts a line.
    pub const USER_OUTPUT: &str = "[user ";
    /// `init` (the process test driver) exited with code 0.
    pub const PROCESS_TESTS_PASS: &str = "[rust-os] PROCESS TESTS PASS";
    /// `init` exited with a non-zero code or faulted, or could not start.
    pub const PROCESS_TESTS_FAIL: &str = "[rust-os] PROCESS TESTS FAIL";
}
