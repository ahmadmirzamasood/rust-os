//! i8042 PS/2 controller with a keyboard on port 1 and a mouse on port 2.
//!
//! These are the input devices QEMU's q35 machine provides by default (the
//! "QEMU PS/2 Keyboard" and "QEMU PS/2 Mouse"). There is no USB stack, so a
//! USB keyboard, mouse or tablet (`-device usb-tablet`, VirtualBox/VMware
//! USB input) is not supported.
//!
//! # Initialization (polled, interrupts off)
//!
//! [`init`] disables both ports, flushes the output buffer, runs the
//! controller and port self-tests, resets both devices, selects scancode
//! set 2 on the keyboard with controller translation on (so the kernel sees
//! set 1), sets mouse defaults (3-byte packets, 100 samples/s, 4
//! counts/mm), enables data reporting, and only then enables the
//! controller's IRQ 1 / IRQ 12 outputs and unmasks them at the PIC. Every
//! wait is a bounded poll; a missing or silent device is reported and the
//! kernel continues without it.
//!
//! # Interrupt path
//!
//! [`on_interrupt`] (IRQ 1 and IRQ 12) reads at most a few bytes, routes each
//! by the status register's AUX bit into [`KEYBOARD_BYTES`] or
//! [`MOUSE_BYTES`], and returns. Decoding happens in the main loop
//! ([`crate::input_events`]). A full ring drops the new byte and counts it;
//! the decoders resynchronize when they see the drop counter change.

use input::ring::ByteRing;

use crate::arch::x86_64::interrupts::{KEYBOARD_IRQ, MOUSE_IRQ};
use crate::arch::x86_64::{inb, outb, pic};

const DATA: u16 = 0x60;
const STATUS: u16 = 0x64;
const COMMAND: u16 = 0x64;

const STATUS_OUTPUT_FULL: u8 = 1 << 0;
const STATUS_INPUT_FULL: u8 = 1 << 1;
const STATUS_AUX: u8 = 1 << 5;

const CONFIG_IRQ1: u8 = 1 << 0;
const CONFIG_IRQ12: u8 = 1 << 1;
const CONFIG_CLOCK1_OFF: u8 = 1 << 4;
const CONFIG_CLOCK2_OFF: u8 = 1 << 5;
const CONFIG_TRANSLATE: u8 = 1 << 6;

const ACK: u8 = 0xFA;
const RESEND: u8 = 0xFE;
const SELF_TEST_OK: u8 = 0xAA;

/// Status polls before a controller wait gives up. Each poll is a port read
/// (microseconds on real hardware), so this bounds a wait to well under a
/// second even on slow machines.
const POLL_LIMIT: u32 = 200_000;
/// Longer bound for device reset, which runs a self-test in the device.
const RESET_POLL_LIMIT: u32 = 2_000_000;
/// Bytes read per interrupt at most (the controller holds one at a time;
/// this only bounds the handler if the status register misbehaves).
const BYTES_PER_INTERRUPT: usize = 4;

/// Raw keyboard bytes (producer: IRQ handler, consumer: main loop).
pub static KEYBOARD_BYTES: ByteRing<256> = ByteRing::new();
/// Raw mouse bytes (producer: IRQ handler, consumer: main loop). Larger
/// than the keyboard ring because motion arrives in 3-byte packets at up to
/// the sample rate.
pub static MOUSE_BYTES: ByteRing<512> = ByteRing::new();

fn read_status() -> u8 {
    // SAFETY: port 0x64 is the i8042 status register, owned by this driver;
    // reading it has no side effects.
    unsafe { inb(STATUS) }
}

fn read_data() -> u8 {
    // SAFETY: port 0x60 is the i8042 data register, owned by this driver.
    // Reading pops the output buffer; only this driver consumes it (polled
    // during `init`, from the IRQ handler afterwards, never both: `init`
    // runs with the controller's IRQ outputs disabled).
    unsafe { inb(DATA) }
}

/// IRQ 1 / IRQ 12 handler body. Runs with interrupts disabled.
pub fn on_interrupt() {
    for _ in 0..BYTES_PER_INTERRUPT {
        let status = read_status();
        if status & STATUS_OUTPUT_FULL == 0 {
            break;
        }
        let byte = read_data();
        if status & STATUS_AUX != 0 {
            MOUSE_BYTES.push(byte);
        } else {
            KEYBOARD_BYTES.push(byte);
        }
    }
}

/// Which device a step talks to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Port {
    Keyboard,
    Mouse,
}

/// Why initialization of the controller or a device failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ps2Error {
    /// The controller never became ready during `stage`.
    Timeout { stage: &'static str },
    /// `stage` got `byte` where something else was expected.
    Unexpected { stage: &'static str, byte: u8 },
    /// The controller has no second port, so no mouse.
    SingleChannel,
    /// Skipped because the controller itself failed.
    ControllerUnavailable,
}

/// Result of [`init`].
#[derive(Debug, Clone, Copy)]
pub struct InitReport {
    pub controller: Result<(), Ps2Error>,
    pub keyboard: Result<(), Ps2Error>,
    pub mouse: Result<(), Ps2Error>,
    /// Bytes discarded while flushing stale controller output.
    pub flushed: u32,
}

fn wait_input_empty(stage: &'static str) -> Result<(), Ps2Error> {
    for _ in 0..POLL_LIMIT {
        if read_status() & STATUS_INPUT_FULL == 0 {
            return Ok(());
        }
        core::hint::spin_loop();
    }
    Err(Ps2Error::Timeout { stage })
}

fn controller_command(command: u8, stage: &'static str) -> Result<(), Ps2Error> {
    wait_input_empty(stage)?;
    // SAFETY: port 0x64 (command register) belongs to this driver; `init`
    // only sends documented i8042 commands.
    unsafe { outb(COMMAND, command) };
    Ok(())
}

fn write_data(byte: u8, stage: &'static str) -> Result<(), Ps2Error> {
    wait_input_empty(stage)?;
    // SAFETY: port 0x60 (data register) belongs to this driver; the byte is
    // a documented command or argument for the addressed device.
    unsafe { outb(DATA, byte) };
    Ok(())
}

/// Next output byte from `from` (or from the controller itself when
/// `None`), skipping bytes from the other port.
fn read_from(from: Option<Port>, limit: u32, stage: &'static str) -> Result<u8, Ps2Error> {
    for _ in 0..limit {
        let status = read_status();
        if status & STATUS_OUTPUT_FULL != 0 {
            let byte = read_data();
            let source = if status & STATUS_AUX != 0 {
                Port::Mouse
            } else {
                Port::Keyboard
            };
            match from {
                // Controller replies arrive on the keyboard side.
                None if source == Port::Keyboard => return Ok(byte),
                Some(port) if port == source => return Ok(byte),
                _ => continue,
            }
        }
        core::hint::spin_loop();
    }
    Err(Ps2Error::Timeout { stage })
}

fn controller_query(command: u8, stage: &'static str) -> Result<u8, Ps2Error> {
    controller_command(command, stage)?;
    read_from(None, POLL_LIMIT, stage)
}

fn flush() -> u32 {
    let mut count = 0;
    while count < 64 && read_status() & STATUS_OUTPUT_FULL != 0 {
        let _ = read_data();
        count += 1;
    }
    count
}

/// Send `byte` to a device and wait for its acknowledgement, resending up
/// to twice on request.
fn device_command(port: Port, byte: u8, stage: &'static str) -> Result<(), Ps2Error> {
    for _ in 0..3 {
        if port == Port::Mouse {
            controller_command(0xD4, stage)?; // next data byte goes to port 2
        }
        write_data(byte, stage)?;
        match read_from(Some(port), POLL_LIMIT, stage)? {
            ACK => return Ok(()),
            RESEND => continue,
            other => return Err(Ps2Error::Unexpected { stage, byte: other }),
        }
    }
    Err(Ps2Error::Unexpected {
        stage,
        byte: RESEND,
    })
}

fn reset_device(port: Port, stage: &'static str) -> Result<(), Ps2Error> {
    device_command(port, 0xFF, stage)?;
    match read_from(Some(port), RESET_POLL_LIMIT, stage)? {
        SELF_TEST_OK => {}
        other => return Err(Ps2Error::Unexpected { stage, byte: other }),
    }
    if port == Port::Mouse {
        // The mouse follows with its device ID (0x00 for a standard mouse).
        let id = read_from(Some(port), POLL_LIMIT, stage)?;
        if id != 0x00 {
            return Err(Ps2Error::Unexpected { stage, byte: id });
        }
    }
    Ok(())
}

fn init_controller() -> Result<(u8, bool), Ps2Error> {
    controller_command(0xAD, "disable port 1")?;
    controller_command(0xA7, "disable port 2")?;
    flush();
    let mut config = controller_query(0x20, "read configuration")?;
    config &= !(CONFIG_IRQ1 | CONFIG_IRQ12);
    config |= CONFIG_TRANSLATE;
    controller_command(0x60, "write configuration")?;
    write_data(config, "write configuration")?;

    match controller_query(0xAA, "controller self-test")? {
        0x55 => {}
        byte => {
            return Err(Ps2Error::Unexpected {
                stage: "controller self-test",
                byte,
            });
        }
    }
    // Some controllers reset their configuration during the self-test.
    controller_command(0x60, "restore configuration")?;
    write_data(config, "restore configuration")?;

    // Port 2 exists if enabling it clears its clock-disable bit.
    controller_command(0xA8, "probe port 2")?;
    let probed = controller_query(0x20, "probe port 2")?;
    let dual = probed & CONFIG_CLOCK2_OFF == 0;
    controller_command(0xA7, "disable port 2")?;

    match controller_query(0xAB, "port 1 test")? {
        0x00 => {}
        byte => {
            return Err(Ps2Error::Unexpected {
                stage: "port 1 test",
                byte,
            });
        }
    }
    Ok((config, dual))
}

fn init_keyboard() -> Result<(), Ps2Error> {
    controller_command(0xAE, "enable port 1")?;
    reset_device(Port::Keyboard, "keyboard reset")?;
    // Scancode set 2; the controller translates it to set 1.
    device_command(Port::Keyboard, 0xF0, "keyboard scancode set")?;
    device_command(Port::Keyboard, 0x02, "keyboard scancode set")?;
    device_command(Port::Keyboard, 0xF4, "keyboard enable scanning")
}

fn init_mouse(dual: bool) -> Result<(), Ps2Error> {
    if !dual {
        return Err(Ps2Error::SingleChannel);
    }
    match controller_query(0xA9, "port 2 test")? {
        0x00 => {}
        byte => {
            return Err(Ps2Error::Unexpected {
                stage: "port 2 test",
                byte,
            });
        }
    }
    controller_command(0xA8, "enable port 2")?;
    reset_device(Port::Mouse, "mouse reset")?;
    device_command(Port::Mouse, 0xF6, "mouse defaults")?;
    device_command(Port::Mouse, 0xF4, "mouse enable reporting")
}

/// Set up the controller and both devices, then enable their interrupts at
/// the controller and the PIC. Call once, with interrupts disabled, after
/// [`pic::init`].
pub fn init() -> InitReport {
    let (config, dual) = match init_controller() {
        Ok(found) => found,
        Err(err) => {
            return InitReport {
                controller: Err(err),
                keyboard: Err(Ps2Error::ControllerUnavailable),
                mouse: Err(Ps2Error::ControllerUnavailable),
                flushed: 0,
            };
        }
    };
    let keyboard = init_keyboard();
    let mouse = init_mouse(dual);
    let flushed = flush();

    let mut config = config & !(CONFIG_CLOCK1_OFF | CONFIG_CLOCK2_OFF);
    if keyboard.is_ok() {
        config |= CONFIG_IRQ1;
    } else {
        config |= CONFIG_CLOCK1_OFF;
    }
    if mouse.is_ok() {
        config |= CONFIG_IRQ12;
    } else {
        config |= CONFIG_CLOCK2_OFF;
    }
    let controller = controller_command(0x60, "enable interrupts")
        .and_then(|()| write_data(config, "enable interrupts"));
    if controller.is_ok() {
        if keyboard.is_ok() {
            pic::unmask(KEYBOARD_IRQ);
        }
        if mouse.is_ok() {
            pic::unmask(MOUSE_IRQ);
        }
    }
    InitReport {
        controller,
        keyboard,
        mouse,
        flushed,
    }
}
