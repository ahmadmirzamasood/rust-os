//! Timer tick and monotonic clock.
//!
//! Two sources, for two jobs:
//!
//! - **Tick** ([`ticks`]): the PIT interrupts at [`TICK_HZ`] and the IRQ 0
//!   handler increments one 64-bit atomic counter (nothing else happens in
//!   interrupt context). The tick wakes the event loop and counts timer
//!   interrupts. It is *not* a reliable clock under emulation: QEMU drops
//!   PIT periods it cannot deliver on time (measured on a Windows host: about
//!   125 of 1000 per second while the guest idles), so ticks undercount.
//! - **Clock** ([`uptime_ns`]): the time-stamp counter, calibrated once at
//!   boot against a 50 ms PIT channel-2 interval by polling (which the
//!   emulator computes from the host clock, independent of interrupt
//!   delivery). One CPU, so readings never go backwards. If calibration
//!   fails, the clock falls back to the tick count.
//!
//! Assumption: the TSC runs at a constant rate. QEMU's TCG derives it from
//! the host's TSC; real CPUs advertise this as "invariant TSC", which is
//! reported at boot but not required.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::arch::x86_64::{cpuid, pit, read_tsc};

/// Requested tick rate.
pub const TICK_HZ: u32 = 1000;

static TICKS: AtomicU64 = AtomicU64::new(0);
/// PIT divisor in use (0 until [`init`]).
static DIVISOR: AtomicU64 = AtomicU64::new(0);
/// TSC frequency in Hz (0: not calibrated, use ticks).
static TSC_HZ: AtomicU64 = AtomicU64::new(0);
/// TSC value at calibration (the clock's zero, close to timer start).
static TSC_START: AtomicU64 = AtomicU64::new(0);

/// Calibrate the clock and program the PIT. Interrupts must be disabled;
/// IRQ 0 stays masked until the caller unmasks it.
pub fn init() -> Rate {
    if let Some(hz) = pit::calibrate_tsc(read_tsc) {
        TSC_START.store(read_tsc(), Ordering::SeqCst);
        TSC_HZ.store(hz, Ordering::SeqCst);
    }
    let divisor = pit::init(TICK_HZ);
    DIVISOR.store(u64::from(divisor), Ordering::SeqCst);
    rate()
}

/// IRQ 0 handler body.
pub fn on_timer_interrupt() {
    let now = TICKS.fetch_add(1, Ordering::Relaxed) + 1;
    #[cfg(feature = "deliberate-interrupt-fault")]
    deliberate::maybe_fault(now);
    #[cfg(not(feature = "deliberate-interrupt-fault"))]
    let _ = now;
}

/// Timer interrupts handled since the timer started.
pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

/// The programmed tick rate and the clock source.
#[derive(Debug, Clone, Copy)]
pub struct Rate {
    pub divisor: u64,
    /// Requested tick rate in millihertz (exact PIT rate).
    pub millihertz: u64,
    /// Calibrated TSC frequency, if the TSC is the clock.
    pub tsc_hz: Option<u64>,
    /// CPUID reports an invariant TSC.
    pub tsc_invariant: bool,
}

pub fn rate() -> Rate {
    let divisor = DIVISOR.load(Ordering::SeqCst).max(1);
    let tsc_hz = TSC_HZ.load(Ordering::SeqCst);
    Rate {
        divisor,
        millihertz: pit::INPUT_HZ * 1000 / divisor,
        tsc_hz: (tsc_hz != 0).then_some(tsc_hz),
        tsc_invariant: cpuid(0x8000_0007, 0).3 & (1 << 8) != 0,
    }
}

/// Monotonic time since the clock started, in nanoseconds.
pub fn uptime_ns() -> u64 {
    let hz = TSC_HZ.load(Ordering::Relaxed);
    if hz == 0 {
        let divisor = u128::from(DIVISOR.load(Ordering::Relaxed).max(1));
        return (u128::from(ticks()) * divisor * 1_000_000_000 / u128::from(pit::INPUT_HZ)) as u64;
    }
    let elapsed = read_tsc().saturating_sub(TSC_START.load(Ordering::Relaxed));
    (u128::from(elapsed) * 1_000_000_000 / u128::from(hz)) as u64
}

/// Monotonic time since the clock started, in milliseconds.
pub fn uptime_ms() -> u64 {
    uptime_ns() / 1_000_000
}

#[cfg(feature = "deliberate-interrupt-fault")]
pub mod deliberate {
    //! Fault-test build: the timer handler writes into an unmapped guard
    //! page once enough ticks have passed, to check that exceptions raised
    //! inside an interrupt handler are reported with that context.

    use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    /// Ticks after arming at which the handler faults.
    pub const FAULT_DELAY_TICKS: u64 = 300;

    static TARGET: AtomicU64 = AtomicU64::new(0);
    static AT_TICK: AtomicU64 = AtomicU64::new(u64::MAX);
    /// Set by the event loop while it sleeps in `hlt`, so the fault never
    /// interrupts a screen update (which would keep the panic banner from
    /// being drawn and make the smoke test flaky).
    pub static MAIN_LOOP_IDLE: AtomicBool = AtomicBool::new(false);

    /// Arm the fault: `target` must be an address inside an unmapped guard
    /// page. The handler writes to it [`FAULT_DELAY_TICKS`] ticks from now.
    pub fn arm(target: u64) {
        TARGET.store(target, Ordering::SeqCst);
        AT_TICK.store(super::ticks() + FAULT_DELAY_TICKS, Ordering::SeqCst);
    }

    pub(super) fn maybe_fault(now: u64) {
        let target = TARGET.load(Ordering::Relaxed);
        if target != 0
            && now >= AT_TICK.load(Ordering::Relaxed)
            && MAIN_LOOP_IDLE.load(Ordering::Relaxed)
        {
            let ptr = core::ptr::with_exposed_provenance_mut::<u64>(target as usize);
            // SAFETY: deliberately faulting: `arm` requires an unmapped guard
            // page, so the write raises a page fault and the kernel stops
            // with a report; it never completes, so no memory is modified.
            unsafe { ptr.write_volatile(0xdead_beef) };
        }
    }
}
