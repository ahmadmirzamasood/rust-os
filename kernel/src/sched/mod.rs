//! Threads and the scheduler (one CPU).
//!
//! # Policy
//!
//! Preemptive round-robin over one FIFO ready queue, no priorities:
//!
//! - A thread runs until it blocks, yields, exits, or its time slice of
//!   [`SLICE_NS`] (measured with the TSC clock) has expired *and* another
//!   thread is ready. The check happens on every timer interrupt; the switch
//!   happens when the interrupt returns, unless preemption is disabled
//!   (a [`crate::sync::TryLock`] is held), in which case it waits for a
//!   later interrupt.
//! - Woken and preempted threads go to the back of the queue. A thread woken
//!   while the idle thread runs is switched to as soon as the waking
//!   interrupt returns.
//! - The idle thread runs only when the queue is empty: it frees the
//!   resources of exited threads and processes, then halts until the next
//!   interrupt.
//! - Kernel threads and user threads (one per process) share the queue. A
//!   user thread is preempted in ring 3 exactly like a kernel thread in
//!   ring 0.
//!
//! With the PIT at 1000 Hz the slice is 10 ticks; QEMU on Windows delivers
//! fewer ticks (see `time`), which only lengthens slices.
//!
//! # Synchronization
//!
//! All scheduler state lives in one [`IrqCell`]: it is touched with
//! interrupts disabled only, by threads and by the timer and IRQ paths
//! (the one exception to "interrupt handlers take no locks": an `IrqCell`
//! cannot be contended on one CPU). Nothing in here allocates, so it is safe
//! from interrupt context. Context switches happen with interrupts
//! disabled; each thread restores its own interrupt state when it resumes.
//!
//! A thread must never block, yield or sleep while preemption is disabled
//! (holding a `TryLock`): that is checked and panics.

pub mod selftest;

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use mm::PhysFrame;

use crate::arch::x86_64::{self as arch, switch, tables};
use crate::memory::{self, KernelStack};
use crate::sync::{self, IrqCell};
use crate::time;

/// Most threads alive at once (including the boot and idle threads).
pub const MAX_THREADS: usize = 32;
/// Time slice.
pub const SLICE_NS: u64 = 10_000_000;
/// Kernel stack size of every new thread (plus one guard page).
pub const STACK_PAGES: u64 = 8;

/// What a blocked thread waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wait {
    /// Any interrupt (the UI event loop waits for input or the timer).
    Interrupt,
    /// The clock reaching this many nanoseconds of uptime.
    Sleep { until_ns: u64 },
    /// The thread exiting.
    Thread(ThreadId),
    /// A kernel object (channel endpoint, process) changing; the key is
    /// chosen by the object's owner and passed to [`wake_object`].
    Object(u64),
}

/// Identifies one thread for its whole life (slots are reused, ids are not).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreadId {
    slot: u8,
    generation: u32,
}

impl ThreadId {
    pub fn slot(self) -> usize {
        usize::from(self.slot)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Free,
    /// Created but not yet started.
    New,
    Ready,
    Running,
    Blocked(Wait),
    /// Finished; resources freed by [`reap`].
    Exited,
}

/// What a new thread runs first.
#[derive(Debug, Clone, Copy)]
enum Entry {
    None,
    Kernel { function: fn(u64), arg: u64 },
    User { entry: u64, stack: u64, arg: u64 },
}

#[derive(Debug, Clone, Copy)]
struct Thread {
    state: State,
    generation: u32,
    name: &'static str,
    /// Saved stack pointer while not running.
    rsp: u64,
    stack: Option<KernelStack>,
    /// CR3 value to load when switching to this thread.
    root: u64,
    /// Process id of a user thread (0 for kernel threads).
    pid: u32,
    entry: Entry,
    stats: ThreadStats,
    /// Uptime when it last started running.
    running_since_ns: u64,
}

/// Counters for one thread.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ThreadStats {
    /// Times it was switched to.
    pub switched_in: u64,
    /// Times it was switched out by the timer (involuntarily).
    pub preempted: u64,
    /// Time spent running.
    pub runtime_ns: u64,
}

impl Thread {
    const EMPTY: Self = Self {
        state: State::Free,
        generation: 0,
        name: "",
        rsp: 0,
        stack: None,
        root: 0,
        pid: 0,
        entry: Entry::None,
        stats: ThreadStats {
            switched_in: 0,
            preempted: 0,
            runtime_ns: 0,
        },
        running_since_ns: 0,
    };
}

/// Fixed-capacity FIFO of thread slots.
#[derive(Debug)]
struct Queue {
    slots: [u8; MAX_THREADS],
    head: usize,
    len: usize,
}

impl Queue {
    const fn new() -> Self {
        Self {
            slots: [0; MAX_THREADS],
            head: 0,
            len: 0,
        }
    }

    fn push(&mut self, slot: usize) {
        // Each slot is queued at most once (only Ready threads are queued),
        // so the queue cannot overflow.
        assert!(self.len < MAX_THREADS, "ready queue overflow");
        self.slots[(self.head + self.len) % MAX_THREADS] = slot as u8;
        self.len += 1;
    }

    fn pop(&mut self) -> Option<usize> {
        if self.len == 0 {
            return None;
        }
        let slot = self.slots[self.head];
        self.head = (self.head + 1) % MAX_THREADS;
        self.len -= 1;
        Some(usize::from(slot))
    }
}

#[derive(Debug)]
struct Scheduler {
    threads: [Thread; MAX_THREADS],
    ready: Queue,
    current: usize,
    idle: usize,
    slice_start_ns: u64,
    need_resched: bool,
    switches: u64,
    preemptions: u64,
}

static SCHED: IrqCell<Scheduler> = IrqCell::new(Scheduler {
    threads: [Thread::EMPTY; MAX_THREADS],
    ready: Queue::new(),
    current: 0,
    idle: 0,
    slice_start_ns: 0,
    need_resched: false,
    switches: 0,
    preemptions: 0,
});

/// Set once [`init`] succeeded; until then the timer and interrupt paths
/// leave the scheduler alone.
static STARTED: AtomicBool = AtomicBool::new(false);
/// Exited threads waiting for [`reap`] (a hint, so idle skips the scan).
static EXITED: AtomicU64 = AtomicU64::new(0);

/// Why a thread could not be created.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnError {
    NotStarted,
    /// No free thread slot.
    TooManyThreads,
    /// No memory for the kernel stack.
    Stack(memory::MemError),
}

impl Scheduler {
    fn id(&self, slot: usize) -> ThreadId {
        ThreadId {
            slot: slot as u8,
            generation: self.threads[slot].generation,
        }
    }

    fn free_slot(&self) -> Option<usize> {
        self.threads.iter().position(|t| t.state == State::Free)
    }

    fn make_ready(&mut self, slot: usize) {
        self.threads[slot].state = State::Ready;
        if slot != self.idle {
            self.ready.push(slot);
        }
        if self.current == self.idle {
            self.need_resched = true;
        }
    }

    /// Pick the next thread and update bookkeeping. The caller has already
    /// set the current thread's new state. Returns the switch to perform,
    /// or `None` if the current thread simply keeps running.
    fn switch_away(&mut self, preempted: bool) -> Option<(*mut u64, u64)> {
        let prev = self.current;
        let next = self.ready.pop().unwrap_or(self.idle);
        let now = time::uptime_ns();
        self.need_resched = false;
        self.slice_start_ns = now;
        if next == prev {
            self.threads[prev].state = State::Running;
            return None;
        }
        let old = &mut self.threads[prev];
        old.stats.runtime_ns += now.saturating_sub(old.running_since_ns);
        if preempted {
            old.stats.preempted += 1;
            self.preemptions += 1;
        }
        self.switches += 1;
        let new = &mut self.threads[next];
        new.state = State::Running;
        new.stats.switched_in += 1;
        new.running_since_ns = now;
        let (root, user_stack) = (new.root, new.pid != 0);
        if user_stack && let Some(stack) = new.stack {
            // SAFETY: the thread's own kernel stack, mapped for its whole
            // life; it is about to run and is the only user of it.
            unsafe { tables::set_kernel_stack(stack.top) };
        }
        if arch::read_cr3() & mm::paging::bits::ADDRESS != root {
            // SAFETY: `root` is the kernel's PML4 or a process PML4 created
            // by `memory::user::create`, which shares the kernel half, so
            // the running kernel stays mapped. Process roots are destroyed
            // only after their thread exited and was switched away from.
            unsafe { arch::write_cr3(root) };
        }
        self.current = next;
        let base = SCHED.as_ptr();
        // SAFETY: `base` points to the static scheduler; this only computes
        // the address of the previous thread's saved-RSP field, which only
        // `switch_context` writes (while this thread is suspended).
        let save = unsafe { &raw mut (*base).threads[prev].rsp };
        Some((save, self.threads[next].rsp))
    }
}

/// Perform a switch decided by `switch_away`. Interrupts must be off.
fn perform(switch: Option<(*mut u64, u64)>) {
    if let Some((save, load)) = switch {
        // SAFETY: interrupts are disabled; `save` is the current thread's
        // field in the static scheduler (written only here); `load` was
        // saved by a previous switch or built by `initial_frame` on a live
        // stack (threads' stacks are freed only after they exited and are
        // never switched to again).
        unsafe { switch::switch_context(save, load) };
    }
}

/// Register the running boot context as the first thread and create the
/// idle thread. Call once, with interrupts enabled, after memory
/// management is up.
pub fn init(boot_name: &'static str) -> Result<(), SpawnError> {
    let root = memory::user::kernel_root()
        .map_err(SpawnError::Stack)?
        .start()
        .as_u64();
    SCHED.with(|s| {
        let boot = &mut s.threads[0];
        boot.state = State::Running;
        boot.generation = 1;
        boot.name = boot_name;
        boot.root = root;
        boot.running_since_ns = time::uptime_ns();
        s.current = 0;
    });
    let idle = create(
        "idle",
        Entry::Kernel {
            function: idle_loop,
            arg: 0,
        },
        root,
        0,
    )?;
    SCHED.with(|s| {
        s.idle = idle.slot();
        // The idle thread is never queued: it is picked when nothing else is
        // ready.
        s.threads[idle.slot()].state = State::Ready;
        s.slice_start_ns = time::uptime_ns();
    });
    STARTED.store(true, Ordering::SeqCst);
    Ok(())
}

pub fn started() -> bool {
    STARTED.load(Ordering::SeqCst)
}

/// Allocate a slot and a kernel stack for a thread in state `New`.
fn create(name: &'static str, entry: Entry, root: u64, pid: u32) -> Result<ThreadId, SpawnError> {
    // Reserve the slot first so a full table fails before allocating.
    let slot = SCHED.with(|s| {
        let slot = s.free_slot()?;
        let t = &mut s.threads[slot];
        t.state = State::New;
        t.generation = t.generation.wrapping_add(1).max(1);
        Some(slot)
    });
    let Some(slot) = slot else {
        return Err(SpawnError::TooManyThreads);
    };
    let stack = match memory::allocate_stack(STACK_PAGES, boot_data::markers::THREAD_STACK_GUARD) {
        Ok(stack) => stack,
        Err(err) => {
            SCHED.with(|s| s.threads[slot].state = State::Free);
            return Err(SpawnError::Stack(err));
        }
    };
    // SAFETY: the stack was just mapped for this thread alone.
    let rsp = unsafe { switch::initial_frame(stack.top) };
    Ok(SCHED.with(|s| {
        let t = &mut s.threads[slot];
        t.name = name;
        t.rsp = rsp;
        t.stack = Some(stack);
        t.root = root;
        t.pid = pid;
        t.entry = entry;
        t.stats = ThreadStats::default();
        s.id(slot)
    }))
}

/// Start a kernel thread running `function(arg)`.
pub fn spawn_kernel(
    name: &'static str,
    function: fn(u64),
    arg: u64,
) -> Result<ThreadId, SpawnError> {
    if !started() {
        return Err(SpawnError::NotStarted);
    }
    let root = SCHED.with(|s| s.threads[0].root);
    let id = create(name, Entry::Kernel { function, arg }, root, 0)?;
    SCHED.with(|s| s.make_ready(id.slot()));
    Ok(id)
}

/// Create (but do not start) the thread of process `pid`: it will enter
/// ring 3 at `entry` with stack pointer `stack` in address space `root`.
pub fn create_user(
    pid: u32,
    root: PhysFrame,
    entry: u64,
    stack: u64,
) -> Result<ThreadId, SpawnError> {
    if !started() {
        return Err(SpawnError::NotStarted);
    }
    create(
        "user",
        Entry::User {
            entry,
            stack,
            arg: 0,
        },
        root.start().as_u64(),
        pid,
    )
}

/// Start a thread made by [`create_user`], passing `arg` in `rdi`.
pub fn start_user(id: ThreadId, arg: u64) {
    SCHED.with(|s| {
        let slot = id.slot();
        if s.threads[slot].generation == id.generation && s.threads[slot].state == State::New {
            if let Entry::User { entry, stack, .. } = s.threads[slot].entry {
                s.threads[slot].entry = Entry::User { entry, stack, arg };
            }
            s.make_ready(slot);
        }
    });
}

/// Undo [`create_user`] for a thread that was never started.
pub fn discard(id: ThreadId) {
    let stack = SCHED.with(|s| {
        let t = &mut s.threads[id.slot()];
        if t.generation != id.generation || t.state != State::New {
            return None;
        }
        t.state = State::Free;
        t.stack.take()
    });
    if let Some(stack) = stack {
        // SAFETY: the thread never ran, so nothing uses its stack.
        let _ = unsafe { memory::free_stack(stack) };
    }
}

/// First Rust code of every new thread (called by the trampoline, with
/// interrupts disabled, on the new thread's stack).
pub extern "C" fn thread_entry() -> ! {
    let entry = SCHED.with(|s| s.threads[s.current].entry);
    match entry {
        Entry::Kernel { function, arg } => {
            arch::enable_interrupts();
            function(arg);
            exit_current()
        }
        // SAFETY: interrupts are disabled; the switch to this thread loaded
        // its process's address space (which maps entry and stack for user
        // access, set up by the loader) and set RSP0 to its kernel stack.
        Entry::User { entry, stack, arg } => unsafe { switch::enter_user(entry, stack, arg) },
        Entry::None => panic!("thread started without an entry point"),
    }
}

/// The current thread's id.
pub fn current() -> ThreadId {
    SCHED.with(|s| s.id(s.current))
}

/// Process id of the current thread (0 for kernel threads).
pub fn current_pid() -> u32 {
    SCHED.with(|s| s.threads[s.current].pid)
}

/// Statistics of a live (or exited, not yet reaped) thread.
pub fn stats(id: ThreadId) -> Option<ThreadStats> {
    SCHED.with(|s| {
        let t = &s.threads[id.slot()];
        let live = t.generation == id.generation && t.state != State::Free;
        live.then(|| {
            let mut stats = t.stats;
            if id.slot() == s.current {
                stats.runtime_ns += time::uptime_ns().saturating_sub(t.running_since_ns);
            }
            stats
        })
    })
}

/// Whether `id` refers to a thread that still has a slot (not yet reaped).
pub fn exists(id: ThreadId) -> bool {
    SCHED.with(|s| {
        let t = &s.threads[id.slot()];
        t.generation == id.generation && t.state != State::Free
    })
}

fn assert_may_block() {
    assert!(
        !sync::preemption_disabled(),
        "thread tried to block or yield while holding a lock"
    );
}

/// Give the CPU to the next ready thread, if any.
pub fn yield_now() {
    assert_may_block();
    let enabled = arch::interrupts_enabled();
    arch::disable_interrupts();
    let switch = SCHED.with(|s| {
        if s.ready.len == 0 {
            return None;
        }
        let me = s.current;
        if me != s.idle {
            s.threads[me].state = State::Ready;
            s.ready.push(me);
        }
        s.switch_away(false)
    });
    perform(switch);
    if enabled {
        arch::enable_interrupts();
    }
}

/// Block the current thread until [`wake`] (or the timer) releases `wait`.
///
/// Interrupts must be disabled by the caller (so the condition it checked
/// cannot change before the thread is marked blocked); they are still
/// disabled when this returns.
pub fn block(wait: Wait) {
    assert_may_block();
    assert!(
        !arch::interrupts_enabled(),
        "block() needs interrupts disabled"
    );
    let switch = SCHED.with(|s| {
        let me = s.current;
        assert!(me != s.idle, "the idle thread cannot block");
        s.threads[me].state = State::Blocked(wait);
        s.switch_away(false)
    });
    perform(switch);
}

/// Make every thread blocked on a wait matching `matches` ready. Returns
/// how many were woken. Callable from interrupt context.
fn wake(matches: impl Fn(Wait) -> bool) -> usize {
    SCHED.with(|s| {
        let mut woken = 0;
        for slot in 0..MAX_THREADS {
            if let State::Blocked(wait) = s.threads[slot].state
                && matches(wait)
            {
                s.make_ready(slot);
                woken += 1;
            }
        }
        woken
    })
}

/// Wake threads blocked on [`Wait::Object`] with `key`.
pub fn wake_object(key: u64) -> usize {
    wake(|w| w == Wait::Object(key))
}

/// Sleep for at least `ms` milliseconds.
pub fn sleep_ms(ms: u64) {
    let until_ns = time::uptime_ns().saturating_add(ms.saturating_mul(1_000_000));
    while time::uptime_ns() < until_ns {
        let enabled = arch::interrupts_enabled();
        arch::disable_interrupts();
        block(Wait::Sleep { until_ns });
        if enabled {
            arch::enable_interrupts();
        }
    }
}

/// Wait until thread `id` has exited.
pub fn join(id: ThreadId) {
    loop {
        let enabled = arch::interrupts_enabled();
        arch::disable_interrupts();
        let done = SCHED.with(|s| {
            let t = &s.threads[id.slot()];
            t.generation != id.generation || matches!(t.state, State::Exited | State::Free)
        });
        if !done {
            block(Wait::Thread(id));
        }
        if enabled {
            arch::enable_interrupts();
        }
        if done {
            return;
        }
    }
}

/// End the current thread. Its stack is freed later by [`reap`].
pub fn exit_current() -> ! {
    assert_may_block();
    arch::disable_interrupts();
    let switch = SCHED.with(|s| {
        let me = s.current;
        assert!(me != 0 && me != s.idle, "boot and idle threads never exit");
        s.threads[me].state = State::Exited;
        let id = s.id(me);
        for slot in 0..MAX_THREADS {
            if s.threads[slot].state == State::Blocked(Wait::Thread(id)) {
                s.make_ready(slot);
            }
        }
        s.switch_away(false)
    });
    EXITED.fetch_add(1, Ordering::Relaxed);
    perform(switch);
    unreachable!("an exited thread was resumed")
}

/// Free the stacks of exited threads. Called from thread context (idle,
/// and before creating processes).
pub fn reap() {
    if EXITED.load(Ordering::Relaxed) == 0 {
        return;
    }
    for slot in 0..MAX_THREADS {
        let stack = SCHED.with(|s| {
            let t = &mut s.threads[slot];
            if t.state != State::Exited || slot == s.current {
                return None;
            }
            t.state = State::Free;
            EXITED.fetch_sub(1, Ordering::Relaxed);
            t.stack.take()
        });
        if let Some(stack) = stack {
            // SAFETY: the thread exited and was switched away from; it is
            // never resumed, so nothing runs on or points into its stack
            // (RSP0 is rewritten before any user thread runs).
            let _ = unsafe { memory::free_stack(stack) };
        }
    }
}

/// Timer interrupt hook (interrupts disabled): wake sleepers, and request
/// a switch when the slice is used up and someone else is ready.
pub fn on_timer_tick() {
    if !started() {
        return;
    }
    let now = time::uptime_ns();
    wake(|w| matches!(w, Wait::Sleep { until_ns } if now >= until_ns));
    SCHED.with(|s| {
        if s.ready.len > 0 && now.saturating_sub(s.slice_start_ns) >= SLICE_NS {
            s.need_resched = true;
        }
    });
}

/// Hook for every hardware interrupt (interrupts disabled): wake threads
/// waiting for an interrupt.
pub fn on_interrupt() {
    if started() {
        wake(|w| w == Wait::Interrupt);
    }
}

/// Called when an interrupt or system call is about to return (interrupts
/// disabled): switch if a switch was requested and preemption is allowed.
pub fn preempt_point() {
    if !started() || sync::preemption_disabled() {
        return;
    }
    let switch = SCHED.with(|s| {
        if !s.need_resched {
            return None;
        }
        let me = s.current;
        if me == s.idle {
            return s.switch_away(false);
        }
        if s.ready.len == 0 {
            s.need_resched = false;
            return None;
        }
        s.threads[me].state = State::Ready;
        s.ready.push(me);
        s.switch_away(true)
    });
    perform(switch);
}

/// Scheduler-wide counters.
#[derive(Debug, Clone, Copy, Default)]
pub struct Counters {
    pub switches: u64,
    pub preemptions: u64,
    pub threads: usize,
}

pub fn counters() -> Counters {
    SCHED.with(|s| Counters {
        switches: s.switches,
        preemptions: s.preemptions,
        threads: s.threads.iter().filter(|t| t.state != State::Free).count(),
    })
}

/// Body of the idle thread.
fn idle_loop(_: u64) {
    loop {
        reap();
        crate::process::reap();
        arch::disable_interrupts();
        let anything_ready = SCHED.with(|s| s.ready.len > 0);
        if anything_ready {
            arch::enable_interrupts();
            yield_now();
        } else {
            // `sti; hlt`: an interrupt arriving after the check still wakes
            // the halt; its return path switches to whatever it woke.
            arch::enable_interrupts_and_wait();
        }
    }
}
