//! Shared interface between the kernel and user-space components.
//!
//! Everything here is `no_std`, allocation-free and host-testable:
//!
//! - [`syscall`]: system-call numbers and the register convention;
//! - [`Error`] and the return-value encoding ([`encode_result`],
//!   [`decode_result`]);
//! - [`ExitStatus`] and its encoding;
//! - [`layout`] and [`limits`]: the user address-space layout and the
//!   bounds the kernel enforces on every argument;
//! - power-management capability types used by the (in-kernel) desktop
//!   model.
//!
//! This is not POSIX and makes no compatibility promise beyond
//! [`ABI_VERSION`]: a program built for one version may be rejected by a
//! kernel of another.

#![no_std]
#![forbid(unsafe_code)]

/// Version of this interface. Bump on any incompatible change.
///
/// - 0: power types only (no system calls).
/// - 1: first system-call set (see [`syscall`]).
///   File calls 12..21 are additive; older v1 kernels return UnknownSyscall.
pub const ABI_VERSION: u32 = 1;

/// System-call numbers and calling convention.
///
/// A user program executes `int 0x80` ([`VECTOR`]) with:
///
/// | Register | Meaning |
/// | --- | --- |
/// | `rax` | call number (below) |
/// | `rdi`, `rsi`, `rdx`, `r10` | arguments 1-4 (unused ones are ignored) |
/// | `rax` (on return) | [`encode_result`] of the outcome |
///
/// Every other register is preserved. Pointers are user virtual addresses;
/// the kernel validates each `(pointer, length)` pair against the calling
/// process's own page tables before touching it.
pub mod syscall {
    /// Interrupt vector of the system-call gate (the only gate user code may
    /// invoke).
    pub const VECTOR: u8 = 0x80;

    /// `() -> ABI_VERSION`.
    pub const ABI_VERSION: u64 = 0;
    /// `(code: i32) -> !`: end the calling process.
    pub const EXIT: u64 = 1;
    /// `(ptr, len) -> len`: write UTF-8 (or bytes) to the kernel log, at
    /// most [`super::limits::DEBUG_WRITE_MAX`] bytes.
    pub const DEBUG_WRITE: u64 = 2;
    /// `() -> 0`: give up the rest of the time slice.
    pub const YIELD: u64 = 3;
    /// `(milliseconds) -> 0`: sleep at least that long (capped at
    /// [`super::limits::SLEEP_MAX_MS`]).
    pub const SLEEP: u64 = 4;
    /// `() -> nanoseconds`: monotonic time since boot.
    pub const CLOCK: u64 = 5;
    /// `(name_ptr, name_len, channel_handle or 0) -> process handle`: start
    /// a program from the boot bundle. A non-zero channel handle is moved
    /// to the child, which receives it as its startup handle.
    pub const SPAWN: u64 = 6;
    /// `(process handle) -> encoded ExitStatus`: block until the process
    /// has exited.
    pub const WAIT: u64 = 7;
    /// `(handle) -> 0`: close a handle.
    pub const CLOSE: u64 = 8;
    /// `(out_ptr) -> 0`: create a channel; writes its two endpoint handles
    /// as two little-endian `u32` at `out_ptr`.
    pub const CHANNEL_CREATE: u64 = 9;
    /// `(handle, ptr, len) -> 0`: queue one message (at most
    /// [`super::limits::MESSAGE_MAX`] bytes) for the other endpoint.
    pub const CHANNEL_SEND: u64 = 10;
    /// `(handle, ptr, capacity, flags) -> len`: take the oldest message.
    /// Blocks while the queue is empty unless `flags` has
    /// [`RECV_NONBLOCK`].
    pub const CHANNEL_RECV: u64 = 11;

    /// Number of defined calls (numbers are `0..COUNT`).
    pub const COUNT: u64 = 26;
    /// One writeback sector/flush; returns 1 when complete, 0 while pending.
    /// Mutations return WouldBlock during writeback; reads remain available.
    pub const FS_SYNC_STEP: u64 = 25;
    /// Bundled display only, desktop kernel: dimensions, RGB row copy, input packet.
    pub const DISPLAY_INFO: u64 = 22;
    pub const DISPLAY_ROW: u64 = 23;
    pub const DISPLAY_INPUT: u64 = 24;

    /// `(path_ptr, path_len, flags) -> handle`; flags WRITE=1, CREATE=2.
    /// CREATE is exclusive and requires WRITE. Handles are process-owned.
    pub const FILE_OPEN: u64 = 12;
    /// `(handle, out_ptr, capacity, offset) -> bytes`; at most 4096 bytes.
    pub const FILE_READ: u64 = 13;
    /// `(handle, ptr, len, offset) -> bytes`; at most 4096 bytes, no partial writes.
    pub const FILE_WRITE: u64 = 14;
    /// `(path_ptr, path_len, out_ptr, capacity) -> bytes`; newline listing.
    pub const FS_LIST: u64 = 15;
    /// `(path_ptr, path_len) -> 0`.
    pub const FS_MKDIR: u64 = 16;
    /// `(from_ptr, from_len, to_ptr, to_len) -> 0`; no overwrite.
    pub const FS_RENAME: u64 = 17;
    /// `(path_ptr, path_len) -> 0`; only empty directories may be removed.
    pub const FS_REMOVE: u64 = 18;
    /// `() -> 0`; flush the entire volume, close does not flush.
    pub const FS_SYNC: u64 = 19;
    /// `() -> 0 (temporary RAM) or 1 (RSFS1 disposable disk)`.
    pub const FS_VOLUME: u64 = 20;
    /// `() -> !`; init only, designated QEMU data disk only. Flush then
    /// Q35 test-machine power-off. Not a general ACPI implementation.
    pub const FS_TEST_SHUTDOWN: u64 = 21;

    /// `CHANNEL_RECV` flag: fail with `WouldBlock` instead of waiting.
    pub const RECV_NONBLOCK: u64 = 1;
}

/// Bounds the kernel enforces on system-call arguments and resources.
pub mod limits {
    /// Largest `DEBUG_WRITE`.
    pub const DEBUG_WRITE_MAX: usize = 256;
    /// Largest channel message.
    pub const MESSAGE_MAX: usize = 256;
    /// Messages a channel endpoint can queue before `CHANNEL_SEND` fails
    /// with `WouldBlock`.
    pub const CHANNEL_DEPTH: usize = 8;
    /// Longest program name.
    pub const NAME_MAX: usize = 32;
    /// Open handles per process.
    pub const HANDLES_PER_PROCESS: usize = 16;
    /// Longest single `SLEEP`.
    pub const SLEEP_MAX_MS: u64 = 60_000;
}

/// User address-space layout (x86_64, 4-level paging).
///
/// The lower canonical half `[0, USER_END)` belongs to the process; the
/// upper half is the kernel's and is never user-accessible. Executables are
/// static ELF files linked inside `[IMAGE_START, IMAGE_END)`; the stack sits
/// below [`STACK_TOP`] with an unmapped guard page beneath it.
pub mod layout {
    /// Lowest address an executable segment may use (keeps page 0 and the
    /// first 4 MiB unmapped, so null-pointer accesses fault).
    pub const IMAGE_START: u64 = 0x40_0000;
    /// End (exclusive) of the executable region.
    pub const IMAGE_END: u64 = 0x4000_0000;
    /// Initial stack pointer region top (exclusive).
    pub const STACK_TOP: u64 = 0x8000_0000;
    /// Mapped stack size; the page below is a guard.
    pub const STACK_SIZE: u64 = 64 * 1024;
    /// End (exclusive) of the user half.
    pub const USER_END: u64 = 0x0000_8000_0000_0000;
    /// Largest total size of an executable's loadable segments.
    pub const IMAGE_MAX_BYTES: u64 = 16 * 1024 * 1024;

    // The regions are ordered, page-aligned and leave a guard page below
    // the stack (checked at compile time).
    const _: () = {
        assert!(IMAGE_START < IMAGE_END);
        assert!(IMAGE_END <= STACK_TOP - STACK_SIZE - 4096);
        assert!(STACK_TOP < USER_END);
        assert!(STACK_TOP.is_multiple_of(4096) && STACK_SIZE.is_multiple_of(4096));
        assert!(IMAGE_MAX_BYTES <= IMAGE_END - IMAGE_START);
    };
}

/// Why a system call failed. Encoded as the negated discriminant in `rax`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum Error {
    /// No such system call.
    UnknownSyscall = 1,
    /// An argument is out of range or a flag is unknown.
    InvalidArgument = 2,
    /// A pointer/length pair is not valid user memory with the needed
    /// access (unmapped, kernel, non-canonical, overflowing, read-only).
    BadAddress = 3,
    /// Not an open handle of the calling process.
    BadHandle = 4,
    /// The handle refers to the wrong kind of object.
    WrongType = 5,
    /// A length exceeds its limit.
    TooLarge = 6,
    /// A table or memory is exhausted.
    NoResources = 7,
    /// No program of that name.
    NotFound = 8,
    /// The operation would have to wait.
    WouldBlock = 9,
    /// The other channel endpoint is closed.
    PeerClosed = 10,
    /// The next message does not fit the buffer (it stays queued).
    BufferTooSmall = 11,
    AlreadyExists = 12,
    NotDirectory = 13,
    IsDirectory = 14,
    NotEmpty = 15,
    NoSpace = 16,
    Io = 17,
    Corrupt = 18,
    ReadOnly = 19,
    PermissionDenied = 20,
}

impl Error {
    pub const ALL: [Error; 20] = [
        Self::UnknownSyscall,
        Self::InvalidArgument,
        Self::BadAddress,
        Self::BadHandle,
        Self::WrongType,
        Self::TooLarge,
        Self::NoResources,
        Self::NotFound,
        Self::WouldBlock,
        Self::PeerClosed,
        Self::BufferTooSmall,
        Self::AlreadyExists,
        Self::NotDirectory,
        Self::IsDirectory,
        Self::NotEmpty,
        Self::NoSpace,
        Self::Io,
        Self::Corrupt,
        Self::ReadOnly,
        Self::PermissionDenied,
    ];

    pub const fn code(self) -> u16 {
        self as u16
    }

    pub fn from_code(code: u16) -> Option<Self> {
        Self::ALL.into_iter().find(|e| e.code() == code)
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::UnknownSyscall => "unknown syscall",
            Self::InvalidArgument => "invalid argument",
            Self::BadAddress => "bad address",
            Self::BadHandle => "bad handle",
            Self::WrongType => "wrong handle type",
            Self::TooLarge => "too large",
            Self::NoResources => "no resources",
            Self::NotFound => "not found",
            Self::WouldBlock => "would block",
            Self::PeerClosed => "peer closed",
            Self::BufferTooSmall => "buffer too small",
            Self::AlreadyExists => "already exists",
            Self::NotDirectory => "not a directory",
            Self::IsDirectory => "is a directory",
            Self::NotEmpty => "directory not empty",
            Self::NoSpace => "disk full",
            Self::Io => "storage I/O error",
            Self::Corrupt => "corrupt volume",
            Self::ReadOnly => "read-only volume",
            Self::PermissionDenied => "permission denied",
        }
    }
}

/// Largest successful return value: values with the top bit set are errors.
pub const RESULT_MAX: u64 = i64::MAX as u64;

/// Encode a system-call outcome for `rax`: `Ok(v)` as `v` (must be at most
/// [`RESULT_MAX`]), `Err(e)` as `-(code)`.
pub const fn encode_result(result: Result<u64, Error>) -> u64 {
    match result {
        Ok(value) => {
            if value > RESULT_MAX {
                // Never produced by the kernel; keep the encoding total.
                (-(Error::InvalidArgument.code() as i64)) as u64
            } else {
                value
            }
        }
        Err(err) => (-(err.code() as i64)) as u64,
    }
}

/// Decode `rax` after a system call. Unknown negative values decode as
/// [`Error::InvalidArgument`] so a newer kernel never panics an older
/// program.
pub fn decode_result(raw: u64) -> Result<u64, Error> {
    if raw <= RESULT_MAX {
        return Ok(raw);
    }
    let code = (raw as i64).unsigned_abs();
    Err(u16::try_from(code)
        .ok()
        .and_then(Error::from_code)
        .unwrap_or(Error::InvalidArgument))
}

/// How a process ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitStatus {
    /// Called `EXIT` with this code.
    Exited(i32),
    /// Terminated by the kernel after a CPU exception in user mode.
    Faulted { vector: u8 },
}

const STATUS_FAULT: u64 = 1 << 32;

impl ExitStatus {
    /// Encoding returned by `WAIT`: an exit code in the low 32 bits, or bit
    /// 32 set and the exception vector in the low 8 bits.
    pub const fn encode(self) -> u64 {
        match self {
            Self::Exited(code) => code as u32 as u64,
            Self::Faulted { vector } => STATUS_FAULT | vector as u64,
        }
    }

    pub const fn decode(raw: u64) -> Option<Self> {
        match raw >> 32 {
            0 => Some(Self::Exited(raw as u32 as i32)),
            1 if raw & 0xFFFF_FF00 == 0 => Some(Self::Faulted { vector: raw as u8 }),
            _ => None,
        }
    }
}

/// A power transition the desktop may request from the kernel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerAction {
    Shutdown,
    Restart,
    Sleep,
}

impl PowerAction {
    pub const ALL: [PowerAction; 3] = [Self::Shutdown, Self::Restart, Self::Sleep];
}

/// Power mechanisms the kernel has actually verified on the running platform.
///
/// Every field defaults to `false`; a mechanism is only reported once the
/// kernel has a working, tested implementation for it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PowerCapabilities {
    pub shutdown: bool,
    pub restart: bool,
    /// Genuine guest suspend *and* resume. Host-side VM pausing does not count.
    pub suspend_resume: bool,
}

/// Why a power action cannot be offered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnavailableReason {
    /// The kernel has no verified implementation of this mechanism.
    NotImplemented,
}

/// Whether the UI may present a power action as enabled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Availability {
    Available,
    Unavailable(UnavailableReason),
}

impl PowerCapabilities {
    /// Decide whether `action` may be offered. Each action depends only on its
    /// own capability, so working shutdown never implies working sleep.
    pub const fn availability(self, action: PowerAction) -> Availability {
        let supported = match action {
            PowerAction::Shutdown => self.shutdown,
            PowerAction::Restart => self.restart,
            PowerAction::Sleep => self.suspend_resume,
        };
        if supported {
            Availability::Available
        } else {
            Availability::Unavailable(UnavailableReason::NotImplemented)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn results_round_trip() {
        for value in [0, 1, 255, RESULT_MAX] {
            assert_eq!(decode_result(encode_result(Ok(value))), Ok(value));
        }
        for err in Error::ALL {
            let raw = encode_result(Err(err));
            assert!(raw > RESULT_MAX, "errors have the top bit set");
            assert_eq!(decode_result(raw), Err(err));
        }
        // Unknown error codes and values that cannot be represented.
        assert_eq!(decode_result((-999i64) as u64), Err(Error::InvalidArgument));
        assert_eq!(decode_result(i64::MIN as u64), Err(Error::InvalidArgument));
        assert_eq!(
            decode_result(encode_result(Ok(u64::MAX))),
            Err(Error::InvalidArgument)
        );
    }

    #[test]
    fn error_codes_are_unique_and_named() {
        for (i, a) in Error::ALL.iter().enumerate() {
            assert_eq!(Error::from_code(a.code()), Some(*a));
            assert!(!a.name().is_empty());
            for b in &Error::ALL[i + 1..] {
                assert_ne!(a.code(), b.code());
            }
        }
        assert_eq!(Error::from_code(0), None);
    }

    #[test]
    fn exit_status_round_trips() {
        for status in [
            ExitStatus::Exited(0),
            ExitStatus::Exited(-1),
            ExitStatus::Exited(i32::MAX),
            ExitStatus::Exited(i32::MIN),
            ExitStatus::Faulted { vector: 14 },
            ExitStatus::Faulted { vector: 0 },
        ] {
            assert_eq!(ExitStatus::decode(status.encode()), Some(status));
        }
        assert_eq!(ExitStatus::decode(2 << 32), None);
        assert_eq!(ExitStatus::decode(STATUS_FAULT | 0x100), None);
    }

    #[test]
    fn nothing_is_available_by_default() {
        let caps = PowerCapabilities::default();
        for action in PowerAction::ALL {
            assert_eq!(
                caps.availability(action),
                Availability::Unavailable(UnavailableReason::NotImplemented)
            );
        }
    }

    #[test]
    fn sleep_stays_unavailable_when_shutdown_and_restart_work() {
        let caps = PowerCapabilities {
            shutdown: true,
            restart: true,
            suspend_resume: false,
        };
        assert_eq!(
            caps.availability(PowerAction::Shutdown),
            Availability::Available
        );
        assert_eq!(
            caps.availability(PowerAction::Restart),
            Availability::Available
        );
        assert_eq!(
            caps.availability(PowerAction::Sleep),
            Availability::Unavailable(UnavailableReason::NotImplemented)
        );
    }

    #[test]
    fn sleep_requires_its_own_capability() {
        let caps = PowerCapabilities {
            suspend_resume: true,
            ..PowerCapabilities::default()
        };
        assert_eq!(
            caps.availability(PowerAction::Sleep),
            Availability::Available
        );
        assert_ne!(
            caps.availability(PowerAction::Shutdown),
            Availability::Available
        );
    }
}
