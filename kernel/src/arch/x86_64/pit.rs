//! 8254 programmable interval timer.
//!
//! - Channel 0 drives the periodic tick (IRQ 0).
//! - Channel 2, gated through port 0x61, is used once at boot as a
//!   reference interval to measure the TSC frequency by polling its output
//!   ([`calibrate_tsc`]). Polling does not depend on interrupt delivery,
//!   which an emulator may delay or drop.

use super::{inb, outb};

const CHANNEL0: u16 = 0x40;
const CHANNEL2: u16 = 0x42;
const MODE_COMMAND: u16 = 0x43;
/// NMI status and control: bit 0 gates channel 2, bit 1 enables the
/// speaker, bit 5 reads channel 2's output.
const PORT_61: u16 = 0x61;

/// Input clock of the PIT in Hz (fixed by the PC architecture).
pub const INPUT_HZ: u64 = 1_193_182;

/// Program channel 0 as a rate generator (mode 2) firing about `hz` times
/// per second. Returns the divisor actually used (1..=65535 after rounding
/// and clamping); the exact rate is `INPUT_HZ / divisor`.
pub fn init(hz: u32) -> u16 {
    let divisor = (INPUT_HZ + u64::from(hz.max(1)) / 2) / u64::from(hz.max(1));
    let divisor = divisor.clamp(1, 0xFFFF) as u16;
    // Command 0x34: channel 0, access low then high byte, mode 2, binary.
    write(MODE_COMMAND, 0x34);
    write(CHANNEL0, divisor as u8);
    write(CHANNEL0, (divisor >> 8) as u8);
    divisor
}

fn write(port: u16, value: u8) {
    // SAFETY: ports 0x40, 0x42, 0x43 and 0x61 belong to the PIT (and its
    // gate/speaker control), which only this module programs, at boot, with
    // interrupts disabled. Channel 0 and channel 2 are independent.
    unsafe { outb(port, value) }
}

fn read(port: u16) -> u8 {
    // SAFETY: as in `write`; reading port 0x61 has no side effects.
    unsafe { inb(port) }
}

/// Reference interval for TSC calibration: 59659 PIT counts = 50.0 ms.
const CALIBRATION_COUNT: u16 = 59_659;
/// Polls of port 0x61 before calibration gives up (far longer than 50 ms
/// on any machine that has a PIT at all).
const CALIBRATION_POLL_LIMIT: u32 = 50_000_000;

/// Measure the TSC frequency in Hz against a 50 ms channel-2 interval, or
/// `None` if channel 2's output never went high. Interrupts must be
/// disabled. The speaker stays off.
pub fn calibrate_tsc(read_tsc: impl Fn() -> u64) -> Option<u64> {
    let control = read(PORT_61);
    // Gate low (stopped), speaker off.
    write(PORT_61, control & !0b11);
    // Channel 2, low then high byte, mode 0 (output goes high at terminal
    // count), binary.
    write(MODE_COMMAND, 0xB0);
    write(CHANNEL2, CALIBRATION_COUNT as u8);
    write(CHANNEL2, (CALIBRATION_COUNT >> 8) as u8);
    // Raise the gate: counting starts.
    write(PORT_61, (control & !0b10) | 0b01);
    let start = read_tsc();
    let mut done = false;
    for _ in 0..CALIBRATION_POLL_LIMIT {
        if read(PORT_61) & 0x20 != 0 {
            done = true;
            break;
        }
        core::hint::spin_loop();
    }
    let end = read_tsc();
    write(PORT_61, control & !0b11);
    let cycles = end.checked_sub(start)?;
    if !done || cycles == 0 {
        return None;
    }
    Some((u128::from(cycles) * u128::from(INPUT_HZ) / u128::from(CALIBRATION_COUNT)) as u64)
}
