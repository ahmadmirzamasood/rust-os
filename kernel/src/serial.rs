//! Polled 16550 UART driver for COM1, used for boot diagnostics.
//!
//! Output only. Every wait is bounded, so a missing or stuck UART drops
//! bytes instead of hanging the kernel. The kernel runs on one CPU and
//! interrupt handlers never print, so only the main loop writes, except a
//! fatal exception report, whose lines could interleave with a line the
//! main loop was printing. Nothing here holds state across bytes, so that
//! never corrupts memory.

use core::fmt::{self, Write};
use core::sync::atomic::{AtomicBool, Ordering};

use crate::arch::x86_64::{inb, outb};

/// Base I/O port of COM1 on PC-compatible machines (QEMU q35 included).
const COM1: u16 = 0x3F8;

// Register offsets from the base port.
const DATA: u16 = 0; // DLAB=0: transmit/receive. DLAB=1: divisor low byte.
const INTERRUPT_ENABLE: u16 = 1; // DLAB=1: divisor high byte.
const FIFO_CONTROL: u16 = 2;
const LINE_CONTROL: u16 = 3;
const MODEM_CONTROL: u16 = 4;
const LINE_STATUS: u16 = 5;

const LINE_STATUS_TX_EMPTY: u8 = 1 << 5;
/// Polls of the line-status register before a byte is dropped. At 115200
/// baud one byte takes ~87 µs; this is far longer on any plausible CPU.
const TX_SPIN_LIMIT: u32 = 100_000;

static READY: AtomicBool = AtomicBool::new(false);

fn write_reg(offset: u16, value: u8) {
    // SAFETY: COM1 (0x3F8..=0x3FF) is owned by this driver; no other kernel
    // code touches these ports, and only one CPU runs with interrupts masked.
    unsafe { outb(COM1 + offset, value) }
}

fn read_reg(offset: u16) -> u8 {
    // SAFETY: as in `write_reg`. The registers read here (data in loopback
    // mode and line status) have no side effects that other code relies on.
    unsafe { inb(COM1 + offset) }
}

/// Program COM1 for 115200 baud, 8N1, FIFOs on, interrupts off, and check
/// with a loopback byte that a UART is present. Returns whether it is.
/// Safe to call again (the panic handler does if init never ran).
pub fn init() -> bool {
    write_reg(INTERRUPT_ENABLE, 0x00); // no UART interrupts
    write_reg(LINE_CONTROL, 0x80); // DLAB on: next two writes set the divisor
    write_reg(DATA, 0x01); // divisor 1 = 115200 baud (low byte)
    write_reg(INTERRUPT_ENABLE, 0x00); // divisor high byte
    write_reg(LINE_CONTROL, 0x03); // DLAB off, 8 data bits, no parity, 1 stop bit
    write_reg(FIFO_CONTROL, 0xC7); // enable and clear FIFOs, 14-byte threshold
    write_reg(MODEM_CONTROL, 0x1E); // loopback mode, RTS, OUT1, OUT2
    write_reg(DATA, 0xAE);
    let present = read_reg(DATA) == 0xAE;
    write_reg(MODEM_CONTROL, 0x0F); // normal mode: DTR, RTS, OUT1, OUT2
    READY.store(present, Ordering::SeqCst);
    present
}

/// Whether [`init`] found a working UART.
pub fn is_ready() -> bool {
    READY.load(Ordering::SeqCst)
}

fn write_byte(byte: u8) {
    for _ in 0..TX_SPIN_LIMIT {
        if read_reg(LINE_STATUS) & LINE_STATUS_TX_EMPTY != 0 {
            write_reg(DATA, byte);
            return;
        }
        core::hint::spin_loop();
    }
    // Transmitter stuck: drop the byte rather than hang.
}

/// Zero-sized handle implementing [`fmt::Write`] over COM1. Writes are
/// silently discarded if no UART was found.
#[derive(Debug, Clone, Copy)]
pub struct Serial;

impl Write for Serial {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        if !is_ready() {
            return Ok(());
        }
        for byte in s.bytes() {
            if byte == b'\n' {
                write_byte(b'\r');
            }
            write_byte(byte);
        }
        Ok(())
    }
}

#[doc(hidden)]
pub fn print(args: fmt::Arguments<'_>) {
    // `Serial::write_str` never fails; formatting errors from user Display
    // impls are ignored because there is nowhere else to report them.
    let _ = Serial.write_fmt(args);
}

/// Print a line to the serial console.
#[macro_export]
macro_rules! kprintln {
    () => { $crate::serial::print(format_args!("\n")) };
    ($($arg:tt)*) => {{
        $crate::serial::print(format_args!($($arg)*));
        $crate::serial::print(format_args!("\n"));
    }};
}
