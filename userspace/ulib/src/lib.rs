//! User-space support library for rust-os programs.
//!
//! A program is a `#![no_std]`, `#![no_main]` binary for
//! `x86_64-unknown-none` that names its main function with [`entry!`]:
//!
//! ```ignore
//! #![no_std]
//! #![no_main]
//!
//! ulib::entry!(main);
//!
//! fn main() -> i32 {
//!     ulib::println!("hello from ring 3");
//!     0
//! }
//! ```
//!
//! The build tool links it statically (non-PIE) inside the user image
//! range of [`os_abi::layout`] and packs it into the boot bundle; the
//! kernel loads it with `spawn` by name.
//!
//! What this library provides: the entry point (which checks the kernel's
//! ABI version), a panic handler (prints the message, exits with code
//! [`PANIC_EXIT_CODE`]), safe wrappers for every system call, owned
//! [`Handle`]s that close themselves, [`Channel`]s, [`Process`]es, and
//! [`print!`]/[`println!`] to the kernel log. There is no heap, no
//! threads, no files and no floating point.

#![no_std]

pub mod sys;

use core::fmt::{self, Write};
use core::num::NonZeroU32;
use core::sync::atomic::{AtomicU32, Ordering};

use os_abi::syscall as nr;
pub use os_abi::{Error, ExitStatus, limits};

pub type Result<T> = core::result::Result<T, Error>;

/// Exit code of a program that panicked.
pub const PANIC_EXIT_CODE: i32 = 101;
/// Exit code of a program started by a kernel with another ABI version.
pub const ABI_MISMATCH_EXIT_CODE: i32 = 126;

fn call(number: u64, a: u64, b: u64, c: u64, d: u64) -> Result<u64> {
    // SAFETY: every call made through this helper passes either no pointer
    // or a pointer/length pair derived from a live Rust reference of the
    // right mutability (see each wrapper), so the kernel only reads or
    // writes memory this program owns as Rust expects.
    os_abi::decode_result(unsafe { sys::syscall(number, a, b, c, d) })
}

/// End the program with `code`.
pub fn exit(code: i32) -> ! {
    let _ = call(nr::EXIT, code as u32 as u64, 0, 0, 0);
    // The kernel never returns from EXIT; if it somehow did, stop here.
    loop {
        core::hint::spin_loop();
    }
}

/// The kernel's ABI version.
pub fn abi_version() -> u32 {
    call(nr::ABI_VERSION, 0, 0, 0, 0).map_or(0, |v| v as u32)
}

/// Write bytes to the kernel log (at most [`limits::DEBUG_WRITE_MAX`]).
pub fn debug_write(bytes: &[u8]) -> Result<usize> {
    call(
        nr::DEBUG_WRITE,
        bytes.as_ptr() as u64,
        bytes.len() as u64,
        0,
        0,
    )
    .map(|n| n as usize)
}

/// Give the CPU to another ready thread.
pub fn yield_now() {
    let _ = call(nr::YIELD, 0, 0, 0, 0);
}

/// Sleep for at least `ms` milliseconds (at most [`limits::SLEEP_MAX_MS`]
/// per call).
pub fn sleep_ms(ms: u64) {
    let _ = call(nr::SLEEP, ms, 0, 0, 0);
}

/// Monotonic nanoseconds since boot.
pub fn clock_ns() -> u64 {
    call(nr::CLOCK, 0, 0, 0, 0).unwrap_or(0)
}

/// An open handle; closed when dropped.
#[derive(Debug)]
pub struct Handle(NonZeroU32);

impl Handle {
    /// Take ownership of a raw handle value (0 is never a handle).
    pub fn from_raw(raw: u32) -> Option<Self> {
        NonZeroU32::new(raw).map(Self)
    }

    pub fn raw(&self) -> u32 {
        self.0.get()
    }

    /// Give up ownership without closing.
    pub fn into_raw(self) -> u32 {
        let raw = self.raw();
        core::mem::forget(self);
        raw
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        let _ = close(self.raw());
    }
}

/// Close a raw handle value.
pub fn close(raw: u32) -> Result<()> {
    call(nr::CLOSE, u64::from(raw), 0, 0, 0).map(drop)
}

/// One endpoint of a bidirectional message channel.
#[derive(Debug)]
pub struct Channel(Handle);

impl Channel {
    /// Create a channel and return both endpoints.
    pub fn pair() -> Result<(Channel, Channel)> {
        let mut out = [0u32; 2];
        call(nr::CHANNEL_CREATE, out.as_mut_ptr() as u64, 0, 0, 0)?;
        match (Handle::from_raw(out[0]), Handle::from_raw(out[1])) {
            (Some(a), Some(b)) => Ok((Channel(a), Channel(b))),
            _ => Err(Error::InvalidArgument),
        }
    }

    pub fn handle(&self) -> &Handle {
        &self.0
    }

    /// Queue one message (at most [`limits::MESSAGE_MAX`] bytes).
    pub fn send(&self, message: &[u8]) -> Result<()> {
        call(
            nr::CHANNEL_SEND,
            u64::from(self.0.raw()),
            message.as_ptr() as u64,
            message.len() as u64,
            0,
        )
        .map(drop)
    }

    /// Wait for the next message; returns its length.
    pub fn recv(&self, buffer: &mut [u8]) -> Result<usize> {
        self.receive(buffer, 0)
    }

    /// Take the next message if one is queued.
    pub fn try_recv(&self, buffer: &mut [u8]) -> Result<usize> {
        self.receive(buffer, nr::RECV_NONBLOCK)
    }

    fn receive(&self, buffer: &mut [u8], flags: u64) -> Result<usize> {
        call(
            nr::CHANNEL_RECV,
            u64::from(self.0.raw()),
            buffer.as_mut_ptr() as u64,
            buffer.len() as u64,
            flags,
        )
        .map(|n| n as usize)
    }
}

/// A started program.
#[derive(Debug)]
pub struct Process(Handle);

impl Process {
    /// Start the bundled program `name`, optionally handing it a channel
    /// endpoint as its startup handle.
    pub fn spawn(name: &str, startup: Option<Channel>) -> Result<Process> {
        let raw = startup.as_ref().map_or(0, |c| c.0.raw());
        let handle = call(
            nr::SPAWN,
            name.as_ptr() as u64,
            name.len() as u64,
            u64::from(raw),
            0,
        )?;
        // The kernel moved the channel handle to the child.
        if let Some(channel) = startup {
            let _ = channel.0.into_raw();
        }
        Handle::from_raw(handle as u32)
            .map(Process)
            .ok_or(Error::InvalidArgument)
    }

    /// Wait until the process has exited.
    pub fn wait(&self) -> Result<ExitStatus> {
        let raw = call(nr::WAIT, u64::from(self.0.raw()), 0, 0, 0)?;
        ExitStatus::decode(raw).ok_or(Error::InvalidArgument)
    }

    pub fn handle(&self) -> &Handle {
        &self.0
    }
}

static STARTUP: AtomicU32 = AtomicU32::new(0);

/// The channel the parent passed at spawn, if any (only the first call
/// returns it).
pub fn startup_channel() -> Option<Channel> {
    Handle::from_raw(STARTUP.swap(0, Ordering::Relaxed)).map(Channel)
}

/// Runtime support used by [`entry!`].
pub mod rt {
    /// Program entry: record the startup handle, check the ABI, run
    /// `main`, exit with its result.
    pub fn start(startup: u64, main: fn() -> i32) -> ! {
        super::STARTUP.store(startup as u32, core::sync::atomic::Ordering::Relaxed);
        if super::abi_version() != os_abi::ABI_VERSION {
            let _ = super::debug_write(b"ulib: kernel ABI version mismatch");
            super::exit(super::ABI_MISMATCH_EXIT_CODE);
        }
        super::exit(main())
    }
}

/// Define the program's entry point (`_start`) calling `$main: fn() -> i32`.
#[macro_export]
macro_rules! entry {
    ($main:path) => {
        /// Program entry point; the kernel passes the startup handle in
        /// `rdi`.
        #[unsafe(no_mangle)]
        pub extern "C" fn _start(startup: u64) -> ! {
            $crate::rt::start(startup, $main)
        }
    };
}

/// Fixed-capacity text buffer for formatting without a heap. Output beyond
/// its capacity is cut off.
#[derive(Debug)]
pub struct LineBuf {
    bytes: [u8; limits::DEBUG_WRITE_MAX],
    len: usize,
}

impl LineBuf {
    pub const fn new() -> Self {
        Self {
            bytes: [0; limits::DEBUG_WRITE_MAX],
            len: 0,
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

impl Default for LineBuf {
    fn default() -> Self {
        Self::new()
    }
}

impl Write for LineBuf {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let room = self.bytes.len() - self.len;
        let n = s.len().min(room);
        self.bytes[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
        Ok(())
    }
}

#[doc(hidden)]
pub fn print_fmt(args: fmt::Arguments<'_>) {
    let mut line = LineBuf::new();
    let _ = line.write_fmt(args);
    let _ = debug_write(line.as_bytes());
}

/// Write one formatted line to the kernel log (one `DEBUG_WRITE` call, cut
/// at [`limits::DEBUG_WRITE_MAX`] bytes).
#[macro_export]
macro_rules! println {
    ($($arg:tt)*) => { $crate::print_fmt(format_args!($($arg)*)) };
}

#[cfg(not(test))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    let mut line = LineBuf::new();
    let _ = match info.location() {
        Some(loc) => write!(
            line,
            "panicked at {}:{}: {}",
            loc.file(),
            loc.line(),
            info.message()
        ),
        None => write!(line, "panicked: {}", info.message()),
    };
    let _ = debug_write(line.as_bytes());
    exit(PANIC_EXIT_CODE)
}
