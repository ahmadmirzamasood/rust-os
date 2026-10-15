//! Interrupt and input self-test, run on every boot once the PIC, PIT and
//! PS/2 controller are set up and interrupts are enabled.
//!
//! It checks the real machine (IDT gates, PIC mask, the timer actually
//! ticking, PS/2 devices present) and runs the input core's overflow and
//! resynchronization paths on the target. Prints one marker line that
//! `cargo xtask smoke` checks.

use boot_data::markers;
use input::event::{EventQueue, InputEvent};
use input::keyboard::{Decoder, Key};
use input::mouse::PacketDecoder;
use input::ring::ByteRing;

use crate::arch::x86_64::{self as arch, interrupts, pic, tables};
use crate::drivers::ps2::InitReport;
use crate::{kprintln, time};

/// Ticks the timer check waits for.
const TIMER_CHECK_TICKS: u64 = 20;
/// Spin iterations before the timer check gives up (several seconds even
/// under emulation; at 1 kHz the 20 ticks take 20 ms).
const TIMER_SPIN_LIMIT: u64 = 400_000_000;

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
        kprintln!("{} {}", markers::INPUT_SELFTEST_CHECK_FAILED, name);
    }
}

/// Run all checks. Interrupts must be enabled.
pub fn run(ps2: &InitReport) -> Outcome {
    let mut o = Outcome {
        checks: 0,
        failures: 0,
    };
    check(
        &mut o,
        "interrupts enabled for the test",
        arch::interrupts_enabled(),
    );
    check(
        &mut o,
        "IDT gates for vectors 32..=255",
        tables::interrupt_gates_installed(),
    );
    let expected_unmasked = (1u16 << interrupts::TIMER_IRQ)
        | (1 << 2)
        | if ps2.keyboard.is_ok() {
            1 << interrupts::KEYBOARD_IRQ
        } else {
            0
        }
        | if ps2.mouse.is_ok() {
            1 << interrupts::MOUSE_IRQ
        } else {
            0
        };
    check(
        &mut o,
        "PIC mask enables exactly timer, cascade and working PS/2 lines",
        pic::mask() == !expected_unmasked,
    );
    check(&mut o, "PS/2 controller", ps2.controller.is_ok());
    check(&mut o, "PS/2 keyboard", ps2.keyboard.is_ok());
    check(&mut o, "PS/2 mouse", ps2.mouse.is_ok());

    // The timer really interrupts, and ticks only move forward.
    // IRQ count first: the handler bumps it before the tick, so a tick
    // between these reads can only make the IRQ delta larger.
    let timer_before = interrupts::stats().timer;
    let start = time::ticks();
    let mut spins = 0u64;
    while time::ticks() < start + TIMER_CHECK_TICKS && spins < TIMER_SPIN_LIMIT {
        core::hint::spin_loop();
        spins += 1;
    }
    let advanced = time::ticks() - start;
    check(&mut o, "timer ticks advance", advanced >= TIMER_CHECK_TICKS);
    check(
        &mut o,
        "every tick came from IRQ 0",
        interrupts::stats().timer - timer_before >= advanced,
    );
    let mut previous = time::ticks();
    let mut monotonic = true;
    for _ in 0..10_000 {
        let now = time::ticks();
        monotonic &= now >= previous;
        previous = now;
    }
    check(&mut o, "ticks are monotonic", monotonic);
    let rate = time::rate();
    check(
        &mut o,
        "TSC calibrated to a plausible frequency (50 MHz - 20 GHz)",
        rate.tsc_hz
            .is_some_and(|hz| (50_000_000..=20_000_000_000).contains(&hz)),
    );
    let mut previous = time::uptime_ns();
    let mut clock_monotonic = true;
    for _ in 0..10_000 {
        let now = time::uptime_ns();
        clock_monotonic &= now >= previous;
        previous = now;
    }
    check(
        &mut o,
        "clock is monotonic and advancing",
        clock_monotonic && time::uptime_ns() > 0,
    );
    check(
        &mut o,
        "no handler marked running outside interrupts",
        interrupts::current_irq().is_none(),
    );
    check(
        &mut o,
        "no interrupts on unexpected vectors",
        interrupts::stats().unexpected == 0,
    );

    // Bounded queues behave as documented on the target.
    let ring = ByteRing::<16>::new();
    let accepted = (0..40u8).filter(|&b| ring.push(b)).count();
    let order_kept = (0..16u8).all(|b| ring.pop() == Some(b)) && ring.pop().is_none();
    check(
        &mut o,
        "full ring drops new bytes and counts them",
        accepted == 16 && ring.dropped() == 24 && order_kept,
    );
    let mut queue = EventQueue::<8>::new();
    let mut decoder = Decoder::new();
    for i in 0..10_000u32 {
        if let Some(event) = decoder.feed(if i % 2 == 0 { 0x1E } else { 0x9E }) {
            queue.push(InputEvent::Key(event));
        }
    }
    check(
        &mut o,
        "event queue stays bounded and drops the oldest",
        queue.len() == 8 && queue.dropped() == 10_000 - 8,
    );
    let mut decoder = Decoder::new();
    let shifted_a = [0x2A, 0x1E, 0x9E, 0xAA]
        .into_iter()
        .filter_map(|b| decoder.feed(b))
        .find(|e| e.key == Key::A && e.pressed);
    check(
        &mut o,
        "scancode decoder: Shift+A gives 'A'",
        shifted_a.and_then(|e| e.ch) == Some('A'),
    );
    let mut mouse = PacketDecoder::new();
    mouse.feed(0x08);
    mouse.resync();
    let packet = [0x00, 0x29, 0x02, 0xFE]
        .into_iter()
        .find_map(|b| mouse.feed(b));
    check(
        &mut o,
        "mouse decoder resynchronizes",
        packet.is_some_and(|p| p.dx == 2 && p.dy == 2 && p.buttons.left),
    );

    kprintln!(
        "[rust-os] input self-test: timer advanced {} ticks in {} spins; {} checks",
        advanced,
        spins,
        o.checks
    );
    if o.passed() {
        kprintln!("{} ({} checks)", markers::INPUT_SELFTEST_PASS, o.checks);
    } else {
        kprintln!(
            "{} ({} of {} checks failed)",
            markers::INPUT_SELFTEST_FAIL,
            o.failures,
            o.checks
        );
    }
    o
}
