//! Processes, handles and channels.
//!
//! A process is an address space ([`crate::memory::user`]), one user thread
//! ([`crate::sched`]) and a handle table. Everything is in fixed tables
//! inside one [`IrqCell`], so there is no unbounded growth.
//!
//! # Handles
//!
//! A handle is a per-process `u32`: `(generation << 8) | (slot + 1)`. Slot
//! indexes the process's table of [`limits::HANDLES_PER_PROCESS`] entries;
//! the 24-bit generation changes every time a slot is reused, so a closed
//! handle value stays invalid. 0 is never a handle. A handle names a
//! channel endpoint or a process. Every system call looks handles up in the
//! *calling* process's table only, so a process cannot name another's
//! objects.
//!
//! # Channels
//!
//! Bidirectional, message-oriented: each endpoint has an inbox of up to
//! [`limits::CHANNEL_DEPTH`] messages of up to [`limits::MESSAGE_MAX`]
//! bytes. Sending to a full inbox fails with `WouldBlock`; receiving from an
//! empty inbox blocks (or fails with `WouldBlock` when asked not to). When
//! one endpoint closes, the other gets `PeerClosed` once its inbox is empty.
//! The channel is freed when both endpoints are closed. Endpoints move to a
//! child at spawn time; that is the only way to share them.
//!
//! # Lifetime
//!
//! `exit` or a user-mode CPU exception ends a process: its handles close at
//! once, waiters wake, and its thread switches away for good. The idle
//! thread (or the next spawn) later frees the address space; the table slot
//! is freed when no process handle refers to it any more.
//!
//! # Locking
//!
//! [`TABLE`] is an `IrqCell` (interrupts off while it is used). It may call
//! into the scheduler (waking threads), never the other way round. Blocking
//! calls check their condition and call `sched::block` with interrupts
//! disabled in between, so a wake-up cannot be lost.

pub mod display;
pub mod files;
pub mod loader;

use alloc::boxed::Box;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use boot_data::markers;
use mm::PhysFrame;
use os_abi::{Error, ExitStatus, limits};

use crate::arch::x86_64::{self as arch, exceptions::ExceptionFrame};
use crate::memory::user;
use crate::sched::{self, ThreadId, Wait};
use crate::sync::IrqCell;
use crate::{kprintln, programs, time};

/// Most processes alive (or exited but still referenced) at once.
pub const MAX_PROCESSES: usize = 16;
/// Most channels open at once, system-wide.
pub const MAX_CHANNELS: usize = 32;

const HANDLES: usize = limits::HANDLES_PER_PROCESS;

type Name = ([u8; limits::NAME_MAX], usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Object {
    File { id: u64, writable: bool },
    Channel { index: usize, side: usize },
    Process { slot: usize, pid: u32 },
}

#[derive(Debug, Clone, Copy)]
struct HandleSlot {
    generation: u32,
    object: Option<Object>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Free,
    Alive,
    Exited(ExitStatus),
}

#[derive(Debug, Clone, Copy)]
struct Process {
    state: State,
    pid: u32,
    name: Name,
    root: Option<PhysFrame>,
    thread: Option<ThreadId>,
    handles: [HandleSlot; HANDLES],
    /// Process handles (in any table) that refer to this process.
    refs: u32,
    started_ns: u64,
    /// The kernel reports this process's exit as the process-test verdict.
    test_driver: bool,
    display_service: bool,
}

impl Process {
    const EMPTY: Self = Self {
        state: State::Free,
        pid: 0,
        name: ([0; limits::NAME_MAX], 0),
        root: None,
        thread: None,
        handles: [HandleSlot {
            generation: 0,
            object: None,
        }; HANDLES],
        refs: 0,
        started_ns: 0,
        test_driver: false,
        display_service: false,
    };

    fn lookup(&self, value: u64) -> Result<Object, Error> {
        let (slot, generation) = decode_handle(value).ok_or(Error::BadHandle)?;
        let entry = self.handles[slot];
        match entry.object {
            Some(object) if entry.generation == generation => Ok(object),
            _ => Err(Error::BadHandle),
        }
    }

    fn free_handles(&self) -> usize {
        self.handles.iter().filter(|h| h.object.is_none()).count()
    }

    fn insert(&mut self, object: Object) -> Result<u32, Error> {
        let (slot, entry) = self
            .handles
            .iter_mut()
            .enumerate()
            .find(|(_, h)| h.object.is_none())
            .ok_or(Error::NoResources)?;
        entry.generation = (entry.generation + 1) & 0xFF_FFFF;
        if entry.generation == 0 {
            entry.generation = 1;
        }
        entry.object = Some(object);
        Ok((entry.generation << 8) | (slot as u32 + 1))
    }

    fn remove(&mut self, value: u64) -> Result<Object, Error> {
        let object = self.lookup(value)?;
        let (slot, _) = decode_handle(value).ok_or(Error::BadHandle)?;
        self.handles[slot].object = None;
        Ok(object)
    }
}

/// Name of process `pid` (empty if unknown).
fn name_of(pid: u32) -> Name {
    TABLE
        .with(|t| t.slot_of(pid).map(|s| t.processes[s].name))
        .unwrap_or(([0; limits::NAME_MAX], 0))
}

fn name_str(name: &Name) -> &str {
    core::str::from_utf8(&name.0[..name.1]).unwrap_or("?")
}

fn decode_handle(value: u64) -> Option<(usize, u32)> {
    let value = u32::try_from(value).ok()?;
    let slot = usize::try_from(value & 0xFF).ok()?.checked_sub(1)?;
    (slot < HANDLES).then_some((slot, value >> 8))
}

#[derive(Debug)]
struct Message {
    len: usize,
    data: [u8; limits::MESSAGE_MAX],
}

#[derive(Debug)]
struct Inbox {
    messages: [Message; limits::CHANNEL_DEPTH],
    head: usize,
    len: usize,
}

#[derive(Debug)]
struct Channel {
    open: [bool; 2],
    /// `inbox[side]` holds messages for endpoint `side`.
    inbox: [Inbox; 2],
}

impl Inbox {
    const EMPTY: Self = Self {
        messages: [const {
            Message {
                len: 0,
                data: [0; limits::MESSAGE_MAX],
            }
        }; limits::CHANNEL_DEPTH],
        head: 0,
        len: 0,
    };

    fn front(&self) -> Option<&Message> {
        (self.len > 0).then(|| &self.messages[self.head])
    }

    fn pop(&mut self) {
        self.head = (self.head + 1) % limits::CHANNEL_DEPTH;
        self.len -= 1;
    }

    fn push(&mut self, data: &[u8]) -> Result<(), Error> {
        if self.len == limits::CHANNEL_DEPTH {
            return Err(Error::WouldBlock);
        }
        let m = &mut self.messages[(self.head + self.len) % limits::CHANNEL_DEPTH];
        m.data[..data.len()].copy_from_slice(data);
        m.len = data.len();
        self.len += 1;
        Ok(())
    }
}

#[derive(Debug)]
struct Table {
    processes: [Process; MAX_PROCESSES],
    channels: [Option<Box<Channel>>; MAX_CHANNELS],
}

static TABLE: IrqCell<Table> = IrqCell::new(Table {
    processes: [Process::EMPTY; MAX_PROCESSES],
    channels: [const { None }; MAX_CHANNELS],
});

static NEXT_PID: AtomicU32 = AtomicU32::new(1);
/// Exited processes whose address space is not yet freed.
static UNREAPED: AtomicU64 = AtomicU64::new(0);

fn inbox_key(index: usize, side: usize) -> u64 {
    (1 << 40) | ((index as u64) << 1) | side as u64
}

fn process_key(pid: u32) -> u64 {
    (2 << 40) | u64::from(pid)
}

impl Table {
    fn slot_of(&self, pid: u32) -> Option<usize> {
        self.processes
            .iter()
            .position(|p| p.state != State::Free && p.pid == pid)
    }

    /// The calling process's slot.
    fn current(&self) -> Result<usize, Error> {
        self.slot_of(sched::current_pid())
            .ok_or(Error::InvalidArgument)
    }

    fn channel(&mut self, index: usize) -> &mut Channel {
        self.channels[index]
            .as_deref_mut()
            .expect("handle refers to a live channel")
    }

    /// Drop one reference to `object` (its handle was closed).
    fn release(&mut self, object: Object) {
        match object {
            Object::File { .. } => {}
            Object::Channel { index, side } => {
                let channel = self.channel(index);
                channel.open[side] = false;
                let both_closed = !channel.open[0] && !channel.open[1];
                // A receiver blocked on the other endpoint must see the
                // closure.
                sched::wake_object(inbox_key(index, 1 - side));
                if both_closed {
                    self.channels[index] = None;
                }
            }
            Object::Process { slot, pid } => {
                let p = &mut self.processes[slot];
                if p.pid == pid && p.state != State::Free {
                    p.refs -= 1;
                    self.maybe_free(slot);
                }
            }
        }
    }

    /// Free an exited process's slot once nothing refers to it.
    fn maybe_free(&mut self, slot: usize) {
        let p = &mut self.processes[slot];
        if matches!(p.state, State::Exited(_)) && p.refs == 0 && p.root.is_none() {
            *p = Process {
                // Keep handle generations so stale values stay invalid.
                handles: p.handles,
                ..Process::EMPTY
            };
        }
    }
}

/// Run `f` on the table with the calling process's slot.
fn with_current<R>(f: impl FnOnce(&mut Table, usize) -> Result<R, Error>) -> Result<R, Error> {
    TABLE.with(|t| {
        let slot = t.current()?;
        f(t, slot)
    })
}

/// Address space of the calling process.
fn current_root() -> Result<PhysFrame, Error> {
    with_current(|t, slot| t.processes[slot].root.ok_or(Error::InvalidArgument))
}

fn user_error(_: user::UserAccessError) -> Error {
    Error::BadAddress
}

/// Run `f` with interrupts disabled, restoring the previous state.
fn interrupts_off<R>(f: impl FnOnce() -> R) -> R {
    let enabled = arch::interrupts_enabled();
    arch::disable_interrupts();
    let result = f();
    if enabled {
        arch::enable_interrupts();
    }
    result
}

// --- Creation --------------------------------------------------------------

/// Why the kernel could not start a process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartError {
    NotFound,
    Load(loader::LoadError),
    Thread(sched::SpawnError),
    Table(Error),
}

impl StartError {
    fn abi(self) -> Error {
        match self {
            Self::NotFound => Error::NotFound,
            Self::Load(loader::LoadError::Elf(_)) => Error::InvalidArgument,
            Self::Load(loader::LoadError::Memory(_)) | Self::Thread(_) => Error::NoResources,
            Self::Table(err) => err,
        }
    }
}

/// Load `name` and start it. With a `parent` slot, a process handle is
/// added to the parent's table and `transfer` (a channel handle of the
/// parent, or 0) moves to the child. Returns the new pid and the parent's
/// handle value.
fn start(
    name: &[u8],
    parent: Option<usize>,
    transfer: u64,
    test_driver: bool,
) -> Result<(u32, u32), StartError> {
    let program = programs::find(name).ok_or(StartError::NotFound)?;
    sched::reap();
    reap();
    let loaded = loader::load(program).map_err(StartError::Load)?;
    let pid = NEXT_PID.fetch_add(1, Ordering::Relaxed);
    let thread = match sched::create_user(pid, loaded.root, loaded.entry, loaded.stack_pointer) {
        Ok(thread) => thread,
        Err(err) => {
            let _ = user::destroy(loaded.root);
            return Err(StartError::Thread(err));
        }
    };
    let mut stored = ([0; limits::NAME_MAX], name.len());
    stored.0[..name.len()].copy_from_slice(name);
    let registered = TABLE.with(|t| {
        let slot = t
            .processes
            .iter()
            .position(|p| p.state == State::Free)
            .ok_or(Error::NoResources)?;
        // Everything that can fail is checked before anything changes.
        if let Some(parent) = parent {
            if transfer != 0 {
                match t.processes[parent].lookup(transfer)? {
                    Object::Channel { .. } => {}
                    _ => return Err(Error::WrongType),
                }
            } else if t.processes[parent].free_handles() == 0 {
                // (A transferred handle frees the slot the process handle
                // then takes.)
                return Err(Error::NoResources);
            }
        }
        let child = &mut t.processes[slot];
        *child = Process {
            state: State::Alive,
            pid,
            name: stored,
            root: Some(loaded.root),
            thread: Some(thread),
            handles: child.handles,
            refs: u32::from(parent.is_some()),
            started_ns: time::uptime_ns(),
            test_driver,
            display_service: cfg!(feature = "desktop") && parent.is_none() && name == b"display",
        };
        let mut child_handle = 0;
        let mut parent_handle = 0;
        if let Some(parent) = parent {
            if transfer != 0 {
                let object = t.processes[parent].remove(transfer)?;
                child_handle = t.processes[slot].insert(object)?;
            }
            parent_handle = t.processes[parent].insert(Object::Process { slot, pid })?;
        }
        Ok((child_handle, parent_handle))
    });
    match registered {
        Ok((child_handle, parent_handle)) => {
            kprintln!(
                "{} {} (pid {}): {} segments, {} KiB image, entry {:#x}",
                markers::PROCESS_STARTED,
                core::str::from_utf8(name).unwrap_or("?"),
                pid,
                loaded.segments,
                loaded.image_bytes / 1024,
                loaded.entry
            );
            sched::start_user(thread, u64::from(child_handle));
            Ok((pid, parent_handle))
        }
        Err(err) => {
            sched::discard(thread);
            let _ = user::destroy(loaded.root);
            Err(StartError::Table(err))
        }
    }
}

/// Start the first user process (no parent). Its exit status is reported
/// as the process-test verdict.
pub fn start_init(name: &str) -> Result<u32, StartError> {
    start(name.as_bytes(), None, 0, true).map(|(pid, _)| pid)
}

/// Start the bundled display as a boot service, without process-test reporting.
pub fn start_display() -> Result<u32, StartError> {
    start(b"display", None, 0, false).map(|(pid, _)| pid)
}

// --- Ending ----------------------------------------------------------------

/// End the calling process with `status`. Never returns.
pub fn exit_current(status: ExitStatus) -> ! {
    let pid = sched::current_pid();
    let thread = sched::current();
    crate::display_device::release(pid);
    let stats = sched::stats(thread).unwrap_or_default();
    let ended = TABLE.with(|t| {
        let slot = t.slot_of(pid)?;
        let p = &mut t.processes[slot];
        p.state = State::Exited(status);
        let (name, started, test_driver) = (p.name, p.started_ns, p.test_driver);
        // Close every handle; generations stay so old values stay invalid.
        let handles = t.processes[slot].handles;
        for (i, h) in handles.iter().enumerate() {
            if let Some(object) = h.object {
                t.processes[slot].handles[i].object = None;
                t.release(object);
            }
        }
        sched::wake_object(process_key(pid));
        UNREAPED.fetch_add(1, Ordering::Relaxed);
        Some((name, started, test_driver))
    });
    if let Some((name, started, test_driver)) = ended {
        let name = name_str(&name);
        let lifetime_ms = time::uptime_ns().saturating_sub(started) / 1_000_000;
        let outcome = describe(status);
        kprintln!(
            "{} {} (pid {}): {}; ran {} ms, cpu {} ms, preempted {} times",
            markers::PROCESS_EXITED,
            name,
            pid,
            outcome.as_str(),
            lifetime_ms,
            stats.runtime_ns / 1_000_000,
            stats.preempted
        );
        if test_driver {
            report_tests(status);
        }
    }
    sched::exit_current()
}

fn describe(status: ExitStatus) -> gfx::FmtBuf<48> {
    use core::fmt::Write;
    let mut out = gfx::FmtBuf::<48>::new();
    let _ = match status {
        ExitStatus::Exited(code) => write!(out, "exit code {code}"),
        ExitStatus::Faulted { vector } => write!(out, "terminated by fault vector {vector}"),
    };
    out
}

fn report_tests(status: ExitStatus) {
    if status == ExitStatus::Exited(0) {
        kprintln!(
            "{}: init ran every process test and exited with code 0",
            markers::PROCESS_TESTS_PASS
        );
    } else {
        kprintln!(
            "{}: init ended with {}",
            markers::PROCESS_TESTS_FAIL,
            describe(status).as_str()
        );
    }
}

/// Report that the test driver could not even start.
pub fn report_start_failure(name: &str, err: StartError) {
    kprintln!(
        "{}: could not start {}: {:?}",
        markers::PROCESS_TESTS_FAIL,
        name,
        err
    );
}

/// A CPU exception in ring 3: report it and end the process. Called by the
/// exception dispatcher (interrupts disabled) on the faulting thread's
/// kernel stack. Never returns to the process.
pub fn user_fault(frame: &ExceptionFrame, cr2: u64) -> ! {
    use crate::arch::x86_64::exceptions::{self, PageFaultCode, SelectorErrorCode};
    let pid = sched::current_pid();
    assert!(pid != 0, "ring-3 exception on a kernel thread");
    let name = name_of(pid);
    let name = name_str(&name);
    let (mnemonic, what) = exceptions::name(frame.vector);
    let vector = frame.vector as u8;
    if usize::from(vector) == exceptions::PAGE_FAULT {
        kprintln!(
            "{} {} (pid {}): {} ({}, vector {}) at rip {:#x}; address {:#x}: {}; process terminated",
            markers::USER_FAULT,
            name,
            pid,
            what,
            mnemonic,
            vector,
            frame.rip,
            cr2,
            PageFaultCode(frame.error_code)
        );
    } else if frame.error_code != 0 && matches!(vector, 10..=13) {
        kprintln!(
            "{} {} (pid {}): {} ({}, vector {}) at rip {:#x}; selector: {}; process terminated",
            markers::USER_FAULT,
            name,
            pid,
            what,
            mnemonic,
            vector,
            frame.rip,
            SelectorErrorCode(frame.error_code)
        );
    } else {
        kprintln!(
            "{} {} (pid {}): {} ({}, vector {}) at rip {:#x}; process terminated",
            markers::USER_FAULT,
            name,
            pid,
            what,
            mnemonic,
            vector,
            frame.rip
        );
    }
    exit_current(ExitStatus::Faulted { vector })
}

/// Free address spaces of exited processes whose thread is gone. Called
/// from thread context (idle thread, process creation).
pub fn reap() {
    if UNREAPED.load(Ordering::Relaxed) == 0 {
        return;
    }
    for slot in 0..MAX_PROCESSES {
        let root = TABLE.with(|t| {
            let p = &mut t.processes[slot];
            let gone = p.thread.is_none_or(|id| !sched::exists(id));
            if matches!(p.state, State::Exited(_)) && p.root.is_some() && gone {
                p.thread = None;
                p.root.take()
            } else {
                None
            }
        });
        if let Some(root) = root {
            if let Err(err) = user::destroy(root) {
                kprintln!("[rust-os] process: address space not freed: {:?}", err);
            }
            UNREAPED.fetch_sub(1, Ordering::Relaxed);
            TABLE.with(|t| t.maybe_free(slot));
        }
    }
}

// --- System calls ------------------------------------------------------------

/// `DEBUG_WRITE`: print `len` bytes from `ptr` as one log line, tagged with
/// the process name and pid. Bytes outside printable ASCII become `.`, so a
/// process cannot start a new line or fake a kernel marker.
pub fn debug_write(ptr: u64, len: u64) -> Result<u64, Error> {
    if len > limits::DEBUG_WRITE_MAX as u64 {
        return Err(Error::TooLarge);
    }
    if len == 0 {
        return Ok(0);
    }
    let root = current_root()?;
    let mut buf = [0u8; limits::DEBUG_WRITE_MAX];
    let text = &mut buf[..len as usize];
    user::copy_from(root, ptr, text).map_err(user_error)?;
    for byte in text.iter_mut() {
        if !(0x20..0x7f).contains(byte) {
            *byte = b'.';
        }
    }
    let pid = sched::current_pid();
    let name = name_of(pid);
    kprintln!(
        "{}{}/{}] {}",
        markers::USER_OUTPUT,
        name_str(&name),
        pid,
        core::str::from_utf8(text).unwrap_or("?")
    );
    Ok(len)
}

/// `SPAWN`.
pub fn spawn(name_ptr: u64, name_len: u64, transfer: u64) -> Result<u64, Error> {
    if name_len == 0 {
        return Err(Error::NotFound);
    }
    if name_len > limits::NAME_MAX as u64 {
        return Err(Error::TooLarge);
    }
    let root = current_root()?;
    let mut name = [0u8; limits::NAME_MAX];
    let name = &mut name[..name_len as usize];
    user::copy_from(root, name_ptr, name).map_err(user_error)?;
    let parent = with_current(|t, slot| {
        if transfer != 0 {
            match t.processes[slot].lookup(transfer)? {
                Object::Channel { .. } => {}
                _ => return Err(Error::WrongType),
            }
        }
        Ok(slot)
    })?;
    start(name, Some(parent), transfer, false)
        .map(|(_, handle)| u64::from(handle))
        .map_err(StartError::abi)
}

/// `WAIT`.
pub fn wait(handle: u64) -> Result<u64, Error> {
    loop {
        let step = interrupts_off(|| {
            let step = with_current(|t, slot| match t.processes[slot].lookup(handle)? {
                Object::Process { slot: target, pid } => {
                    let p = &t.processes[target];
                    match p.state {
                        State::Exited(status) if p.pid == pid => Ok(Ok(status.encode())),
                        State::Alive if p.pid == pid => Ok(Err(process_key(pid))),
                        _ => Err(Error::BadHandle),
                    }
                }
                _ => Err(Error::WrongType),
            });
            if let Ok(Err(key)) = step {
                sched::block(Wait::Object(key));
            }
            step
        })?;
        if let Ok(status) = step {
            return Ok(status);
        }
    }
}

/// `CLOSE`.
pub fn close(handle: u64) -> Result<u64, Error> {
    with_current(|t, slot| {
        let object = t.processes[slot].remove(handle)?;
        t.release(object);
        Ok(0)
    })
}

/// `CHANNEL_CREATE`.
pub fn channel_create(out: u64) -> Result<u64, Error> {
    let root = current_root()?;
    user::check(root, out, 8, true).map_err(user_error)?;
    let (a, b) = with_current(|t, slot| {
        if t.processes[slot].free_handles() < 2 {
            return Err(Error::NoResources);
        }
        let index = t
            .channels
            .iter()
            .position(Option::is_none)
            .ok_or(Error::NoResources)?;
        t.channels[index] = Some(Box::new(Channel {
            open: [true, true],
            inbox: [Inbox::EMPTY, Inbox::EMPTY],
        }));
        let p = &mut t.processes[slot];
        let a = p.insert(Object::Channel { index, side: 0 })?;
        let b = p.insert(Object::Channel { index, side: 1 })?;
        Ok((a, b))
    })?;
    let mut bytes = [0u8; 8];
    bytes[..4].copy_from_slice(&a.to_le_bytes());
    bytes[4..].copy_from_slice(&b.to_le_bytes());
    if user::copy_to(root, out, &bytes).is_err() {
        // Checked above and nothing unmaps user memory meanwhile; undo
        // anyway rather than leak.
        let _ = close(u64::from(a));
        let _ = close(u64::from(b));
        return Err(Error::BadAddress);
    }
    Ok(0)
}

fn endpoint(t: &Table, slot: usize, handle: u64) -> Result<(usize, usize), Error> {
    match t.processes[slot].lookup(handle)? {
        Object::Channel { index, side } => Ok((index, side)),
        _ => Err(Error::WrongType),
    }
}

/// `CHANNEL_SEND`.
pub fn channel_send(handle: u64, ptr: u64, len: u64) -> Result<u64, Error> {
    if len > limits::MESSAGE_MAX as u64 {
        return Err(Error::TooLarge);
    }
    let root = current_root()?;
    let mut buf = [0u8; limits::MESSAGE_MAX];
    let message = &mut buf[..len as usize];
    user::copy_from(root, ptr, message).map_err(user_error)?;
    with_current(|t, slot| {
        let (index, side) = endpoint(t, slot, handle)?;
        let peer = 1 - side;
        let channel = t.channel(index);
        if !channel.open[peer] {
            return Err(Error::PeerClosed);
        }
        channel.inbox[peer].push(message)?;
        sched::wake_object(inbox_key(index, peer));
        Ok(0)
    })
}

/// `CHANNEL_RECV`.
pub fn channel_recv(handle: u64, ptr: u64, capacity: u64, flags: u64) -> Result<u64, Error> {
    use os_abi::syscall::RECV_NONBLOCK;
    if flags & !RECV_NONBLOCK != 0 {
        return Err(Error::InvalidArgument);
    }
    let root = current_root()?;
    let usable = capacity.min(limits::MESSAGE_MAX as u64);
    user::check(root, ptr, usable, true).map_err(user_error)?;
    let mut buf = [0u8; limits::MESSAGE_MAX];
    loop {
        let step = interrupts_off(|| {
            let step = with_current(|t, slot| {
                let (index, side) = endpoint(t, slot, handle)?;
                let channel = t.channel(index);
                let inbox = &mut channel.inbox[side];
                if let Some(message) = inbox.front() {
                    if message.len as u64 > capacity {
                        return Err(Error::BufferTooSmall);
                    }
                    let len = message.len;
                    buf[..len].copy_from_slice(&message.data[..len]);
                    inbox.pop();
                    return Ok(Ok(len));
                }
                if !channel.open[1 - side] {
                    return Err(Error::PeerClosed);
                }
                if flags & RECV_NONBLOCK != 0 {
                    return Err(Error::WouldBlock);
                }
                Ok(Err(inbox_key(index, side)))
            });
            if let Ok(Err(key)) = step {
                sched::block(Wait::Object(key));
            }
            step
        })?;
        if let Ok(len) = step {
            user::copy_to(root, ptr, &buf[..len]).map_err(user_error)?;
            return Ok(len as u64);
        }
    }
}
