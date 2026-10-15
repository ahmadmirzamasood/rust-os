//! Scheduler self-test, run on every boot once the scheduler is up.
//!
//! - **Preemption:** three kernel threads spin for [`SPIN_MS`] without ever
//!   yielding, each counting iterations and watching the others' counters.
//!   On one CPU a thread can only see another counter move between two of
//!   its own reads if it was preempted in between. All three must make
//!   progress, each must observe interleaving, and the timer must have
//!   preempted at least that many times.
//! - **Blocking:** the boot thread joins them (blocks until they exit),
//!   sleeps for [`SLEEP_MS`] (and must wake no earlier), and two threads
//!   ping-pong with `yield_now`.
//! - **Cleanup:** after the idle thread reaps them, the thread count
//!   returns to its starting value and every stack frame is free again
//!   (up to a few page-table frames: stack address space is never reused).
//!
//! Prints one marker line that `cargo xtask smoke` checks.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use boot_data::markers;

use super::{ThreadId, counters, join, sleep_ms, spawn_kernel, yield_now};
use crate::{kprintln, memory, time};

const SPINNERS: usize = 3;
const SPIN_MS: u64 = 150;
const SLEEP_MS: u64 = 30;
const PING_PONG_ROUNDS: u64 = 50;
/// Page-table frames the test's stacks may legitimately leave behind.
const MAX_TABLE_FRAMES: u64 = 4;

static COUNTS: [AtomicU64; SPINNERS] = [const { AtomicU64::new(0) }; SPINNERS];
static INTERLEAVED: [AtomicBool; SPINNERS] = [const { AtomicBool::new(false) }; SPINNERS];
static DEADLINE_NS: AtomicU64 = AtomicU64::new(0);
static TURN: AtomicU64 = AtomicU64::new(0);
static PING_PONG_OK: AtomicBool = AtomicBool::new(true);

#[derive(Debug, Clone, Copy)]
pub struct Outcome {
    pub checks: u32,
    pub failures: u32,
}

impl Outcome {
    pub fn passed(&self) -> bool {
        self.failures == 0
    }
}

fn check(o: &mut Outcome, name: &str, ok: bool) {
    o.checks += 1;
    if !ok {
        o.failures += 1;
        kprintln!("{} {}", markers::SCHED_SELFTEST_CHECK_FAILED, name);
    }
}

fn others(me: usize) -> u64 {
    (0..SPINNERS)
        .filter(|&i| i != me)
        .map(|i| COUNTS[i].load(Ordering::Relaxed))
        .sum()
}

/// A CPU-bound thread that never yields.
fn spinner(index: u64) {
    let me = index as usize;
    let deadline = DEADLINE_NS.load(Ordering::Relaxed);
    let mut seen = others(me);
    while time::uptime_ns() < deadline {
        COUNTS[me].fetch_add(1, Ordering::Relaxed);
        let now = others(me);
        if now != seen {
            INTERLEAVED[me].store(true, Ordering::Relaxed);
            seen = now;
        }
        core::hint::spin_loop();
    }
}

/// Two of these alternate strictly with `yield_now`.
fn ping_pong(side: u64) {
    let mut waited = 0u64;
    for round in 0..PING_PONG_ROUNDS {
        let mine = round * 2 + side;
        while TURN.load(Ordering::Acquire) != mine {
            yield_now();
            waited += 1;
            if waited > 1_000_000 {
                PING_PONG_OK.store(false, Ordering::Relaxed);
                return;
            }
        }
        TURN.store(mine + 1, Ordering::Release);
    }
}

/// Run all checks. Interrupts must be enabled, the scheduler started.
pub fn run() -> Outcome {
    let mut o = Outcome {
        checks: 0,
        failures: 0,
    };
    let started_at = time::uptime_ns();
    let before = counters();
    let frames_before = memory::free_frames();

    // Preemption.
    DEADLINE_NS.store(started_at + SPIN_MS * 1_000_000, Ordering::Relaxed);
    let mut threads: [Option<ThreadId>; SPINNERS] = [None; SPINNERS];
    for (i, slot) in threads.iter_mut().enumerate() {
        *slot = spawn_kernel("selftest-spin", spinner, i as u64).ok();
    }
    check(
        &mut o,
        "spawn three kernel threads",
        threads.iter().all(Option::is_some),
    );
    for id in threads.iter().flatten() {
        join(*id);
    }
    let elapsed_ms = (time::uptime_ns() - started_at) / 1_000_000;
    let after = counters();
    let counts: [u64; SPINNERS] = core::array::from_fn(|i| COUNTS[i].load(Ordering::Relaxed));
    check(
        &mut o,
        "every spinner made progress",
        counts.iter().all(|&c| c > 0),
    );
    check(
        &mut o,
        "every spinner saw the others run while it was not finished (preempted)",
        INTERLEAVED.iter().all(|f| f.load(Ordering::Relaxed)),
    );
    let preemptions = after.preemptions - before.preemptions;
    check(
        &mut o,
        "timer preempted the spinners",
        preemptions >= SPINNERS as u64,
    );
    check(
        &mut o,
        "join returned after the spin time",
        elapsed_ms >= SPIN_MS,
    );

    // Sleep.
    let t0 = time::uptime_ns();
    sleep_ms(SLEEP_MS);
    let slept_ms = (time::uptime_ns() - t0) / 1_000_000;
    check(
        &mut o,
        "sleep lasts at least the requested time",
        slept_ms >= SLEEP_MS,
    );

    // Voluntary switching.
    TURN.store(0, Ordering::Relaxed);
    let a = spawn_kernel("selftest-ping", ping_pong, 0);
    let b = spawn_kernel("selftest-pong", ping_pong, 1);
    check(&mut o, "spawn ping-pong threads", a.is_ok() && b.is_ok());
    for id in [a, b].into_iter().flatten() {
        join(id);
    }
    check(
        &mut o,
        "two threads alternate strictly with yield",
        PING_PONG_OK.load(Ordering::Relaxed)
            && TURN.load(Ordering::Relaxed) == 2 * PING_PONG_ROUNDS,
    );

    // Cleanup: give the idle thread a chance to reap, then compare.
    let mut cleaned = false;
    for _ in 0..50 {
        sleep_ms(2);
        let now = counters();
        // Stacks never reuse address space, so a new page table may remain.
        let leaked = frames_before.saturating_sub(memory::free_frames());
        if now.threads == before.threads && leaked <= MAX_TABLE_FRAMES {
            cleaned = true;
            break;
        }
    }
    check(
        &mut o,
        "exited threads reaped: thread count and free frames restored",
        cleaned,
    );

    let end = counters();
    let line = if o.passed() {
        markers::SCHED_SELFTEST_PASS
    } else {
        markers::SCHED_SELFTEST_FAIL
    };
    kprintln!(
        "{} ({} of {} checks): spinners {} / {} / {} iterations in {} ms, {} preemptions; \
         slept {} of {} ms; {} context switches in total",
        line,
        o.checks - o.failures,
        o.checks,
        counts[0],
        counts[1],
        counts[2],
        elapsed_ms,
        preemptions,
        slept_ms,
        SLEEP_MS,
        end.switches
    );
    o
}
