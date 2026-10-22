//! The kernel's UI event loop (the boot thread, once boot is done): decode
//! input, update the screen, log, and sleep until the next interrupt.
//!
//! Each iteration drains the PS/2 rings through [`Pipeline`], hands events
//! to the [`Ui`] (if the screen is available) and logs key, button and
//! click events on serial. It then checks, with interrupts disabled, that no
//! input is pending before blocking until the next interrupt, so a byte
//! that arrives between the check and the block still wakes the loop. Other
//! threads and processes run while it is blocked. The timer interrupt wakes
//! it at least once per tick.

use boot_data::markers;
use input::event::InputEvent;
use input::keyboard::Key;

use crate::arch::x86_64::{self as arch, interrupts};
use crate::input_events::Pipeline;
use crate::ui::{Status, Ui};
use crate::{kprintln, sched, time};

/// How often the panel's uptime and counters are refreshed.
const PANEL_REFRESH_MS: u64 = 250;
/// How long F9 stops the loop from consuming input (overflow test).
const STALL_MS: u64 = 8000;

/// Run forever. Interrupts must be enabled by the caller.
pub fn run(mut pipeline: Pipeline, mut ui: Option<Ui>) -> ! {
    let mut next_heartbeat_s = 1u64;
    let mut next_panel_ms = 0u64;
    let mut draining_after_stall = false;
    loop {
        if draining_after_stall && !pipeline.has_pending() {
            draining_after_stall = false;
            let s = pipeline.stats();
            kprintln!(
                "{} events lost {}, decoder resyncs {}",
                markers::STALL_DRAINED,
                s.events_dropped,
                s.resyncs
            );
        }
        pipeline.poll();
        while let Some(event) = pipeline.next_event() {
            log_event(&event);
            crate::display_device::push(&event);
            // A user-space desktop owns these events. Diagnostic actions must
            // not also consume them (in particular F9's eight-second stall).
            let desktop_active = crate::display_device::active();
            if let InputEvent::Key(key) = event
                && key.key == Key::F9
                && key.pressed
                && !key.repeat
                && !desktop_active
            {
                stall(&pipeline);
                draining_after_stall = true;
            }
            if !desktop_active
                && let Some(ui) = ui.as_mut()
                && let Some(clicks) = ui.handle(&event)
            {
                let (x, y) = pipeline.pointer().position();
                kprintln!("{} ({}) at {},{}", markers::UI_CLICK, clicks, x, y);
            }
        }

        let uptime_ms = time::uptime_ms();
        if uptime_ms >= next_heartbeat_s * 1000 {
            heartbeat(next_heartbeat_s, &pipeline);
            // Every second for the first five, then every ten seconds.
            next_heartbeat_s = if next_heartbeat_s < 5 {
                next_heartbeat_s + 1
            } else {
                (next_heartbeat_s / 10 + 1) * 10
            };
        }

        if let Some(ui) = ui.as_mut() {
            if uptime_ms >= next_panel_ms {
                ui.invalidate_panel();
                next_panel_ms = uptime_ms + PANEL_REFRESH_MS;
            }
            let active = crate::display_device::active();
            if crate::display_device::take_restore() {
                let (x, y) = pipeline.pointer().position();
                let _ = ui.handle(&InputEvent::PointerMoved { x, y });
                ui.invalidate_screen();
            }
            if !active {
                ui.render(&status(&pipeline));
            }
        }

        arch::disable_interrupts();
        if !pipeline.has_pending() {
            #[cfg(feature = "deliberate-interrupt-fault")]
            time::deliberate::MAIN_LOOP_IDLE.store(true, core::sync::atomic::Ordering::Relaxed);
            wait_for_interrupt();
            #[cfg(feature = "deliberate-interrupt-fault")]
            time::deliberate::MAIN_LOOP_IDLE.store(false, core::sync::atomic::Ordering::Relaxed);
        }
        arch::enable_interrupts();
    }
}

/// Sleep until the next interrupt (interrupts disabled on entry and exit).
/// Other threads run meanwhile; without a scheduler, halt the CPU instead.
fn wait_for_interrupt() {
    if sched::started() {
        sched::block(sched::Wait::Interrupt);
    } else {
        arch::enable_interrupts_and_wait();
        arch::disable_interrupts();
    }
}

/// Current values for the input panel.
pub fn status(pipeline: &Pipeline) -> Status {
    Status {
        uptime_ms: time::uptime_ms(),
        ticks: time::ticks(),
        tick_millihertz: time::rate().millihertz,
        tsc_clock: time::rate().tsc_hz.is_some(),
        irq: interrupts::stats(),
        input: pipeline.stats(),
    }
}

/// Debug aid (F9): stop consuming input for [`STALL_MS`] while interrupts
/// stay enabled, so the handlers keep filling the byte rings until they
/// overflow. Afterwards the loop resumes normally: the decoders see the drop
/// counters change and resynchronize. Used by the `input` smoke scenario.
fn stall(pipeline: &Pipeline) {
    kprintln!(
        "{} for {} ms (interrupts stay enabled)",
        markers::STALL_BEGIN,
        STALL_MS
    );
    let until = time::uptime_ms() + STALL_MS;
    while time::uptime_ms() < until {
        core::hint::spin_loop();
    }
    let s = pipeline.stats();
    kprintln!(
        "{} kbd ring {}/{} lost {}, mouse ring {}/{} lost {}",
        markers::STALL_END,
        s.keyboard_pending,
        s.keyboard_capacity,
        s.keyboard_dropped,
        s.mouse_pending,
        s.mouse_capacity,
        s.mouse_dropped
    );
}

fn log_event(event: &InputEvent) {
    match *event {
        InputEvent::Key(key) => {
            let action = if key.pressed { "press" } else { "release" };
            let repeat = if key.repeat { " (repeat)" } else { "" };
            match key.ch.filter(char::is_ascii_graphic) {
                Some(c) => kprintln!(
                    "{} {} {} (scancode {:#x}) char '{}'{}",
                    markers::INPUT_KEY,
                    action,
                    key.key.name(),
                    key.scancode,
                    c,
                    repeat
                ),
                None => kprintln!(
                    "{} {} {} (scancode {:#x}){}",
                    markers::INPUT_KEY,
                    action,
                    key.key.name(),
                    key.scancode,
                    repeat
                ),
            }
        }
        InputEvent::PointerButton {
            button,
            pressed,
            x,
            y,
        } => kprintln!(
            "{} {} {} at {},{}",
            markers::INPUT_MOUSE_BUTTON,
            button.name(),
            if pressed { "down" } else { "up" },
            x,
            y
        ),
        // Motion is shown on screen only; logging every packet would make
        // the serial port the bottleneck.
        InputEvent::PointerMoved { .. } => {}
    }
}

fn heartbeat(seconds: u64, pipeline: &Pipeline) {
    let irq = interrupts::stats();
    let input = pipeline.stats();
    let (x, y) = pipeline.pointer().position();
    let uptime_ms = time::uptime_ms().max(1);
    kprintln!(
        "{} {} s ({}); timer IRQs {} (avg {}/s of {} Hz requested); IRQs keyboard {}, mouse {}, \
         spurious {}, stray {}; \
         rings kbd {}/{} mouse {}/{}; input lost kbd {}, mouse {}, events {}; \
         decoder resyncs {}, motion merged {}, kbd bytes ignored {}, mouse bytes discarded {}; \
         pointer {},{}",
        markers::TIMER_HEARTBEAT,
        seconds,
        if time::rate().tsc_hz.is_some() {
            "TSC clock"
        } else {
            "tick clock"
        },
        irq.timer,
        irq.timer * 1000 / uptime_ms,
        time::TICK_HZ,
        irq.keyboard,
        irq.mouse,
        irq.spurious,
        irq.unexpected,
        input.keyboard_pending,
        input.keyboard_capacity,
        input.mouse_pending,
        input.mouse_capacity,
        input.keyboard_dropped,
        input.mouse_dropped,
        input.events_dropped,
        input.resyncs,
        input.motion_coalesced,
        input.keyboard_ignored,
        input.mouse_discarded,
        x,
        y
    );
    if irq.unexpected > 0 {
        kprintln!(
            "[rust-os]   {} interrupts on vectors outside the 8259 range; last vector {}",
            irq.unexpected,
            irq.last_unexpected_vector
        );
    }
}
