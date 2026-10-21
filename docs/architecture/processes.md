# Threads, processes and system calls

This page describes how the kernel schedules threads, runs user programs
in ring 3, isolates them from the kernel and from each other, and what the
first system-call interface looks like. Code: `kernel/src/sched/`,
`kernel/src/process/`, `kernel/src/syscall.rs`, `kernel/src/memory/user.rs`,
`kernel/src/arch/x86_64/switch.rs`, `crates/os-abi`, `crates/exec`,
`userspace/ulib`, `userspace/apps`.

**Scope.** One CPU. One thread per process. A small, versioned interface
driven by the programs in `userspace/apps`; it is not POSIX and makes no
compatibility promise. The desktop is still not a user process.

## Threads and scheduling

A thread is a kernel stack (8 pages, with an unmapped guard page below),
a saved stack pointer, the address space it runs in, and a state:
*new*, *ready*, *running*, *blocked* (waiting for an interrupt, a deadline,
another thread's exit, or a kernel object) or *exited*. The table holds at
most 32 threads.

| Thread | Role |
| --- | --- |
| `ui` | The boot context. After boot it runs the input/screen event loop and blocks until the next interrupt when idle. |
| `idle` | Runs only when nothing is ready: frees the stacks of exited threads and the address spaces of exited processes, then halts until an interrupt. |
| kernel threads | Created with `sched::spawn_kernel` (the scheduler self-test uses them). |
| user threads | One per process; enters ring 3 at the program's entry point. |

**Policy: preemptive round-robin.** One FIFO ready queue, no priorities. A
thread runs until it blocks, yields, exits, or its 10 ms time slice
(measured with the TSC clock) has expired while another thread is ready.
The timer interrupt checks the slice; the switch happens when the
interrupt returns. Woken and preempted threads go to the back of the queue.
A thread woken while `idle` runs is switched to as soon as the waking
interrupt returns. On Windows hosts QEMU delivers fewer timer interrupts
than requested (see [interrupts-input.md](interrupts-input.md)), which only
lengthens slices.

**Context switch.** `switch_context` (global assembly) pushes the six
callee-saved registers, stores the stack pointer, loads the next thread's
and pops its registers. Everything else is on the threads' own stacks
(interrupt frames, return addresses). The kernel is compiled soft-float, so
there is no FPU/SIMD state. When the next thread runs in another address
space, CR3 is reloaded; when it is a user thread, the TSS RSP0 is pointed at
its kernel stack, so interrupts and system calls from ring 3 land there.

### Locking rules

Preemption is only safe if no thread is switched out while it holds a
lock and interrupt handlers can never find shared state half-updated:

- **`TryLock`** (heap, memory manager, display, ...) disables preemption
  while held. Interrupts still run, but the scheduler does not switch at
  their end; it switches at a later interrupt. So a lock can only be found
  held by re-entry from an exception or the panic handler, as before.
- **`IrqCell`** holds state shared with interrupt handlers (scheduler,
  process table). It is used with interrupts disabled only, so on one CPU
  nothing else can touch it meanwhile; re-entry panics. The process table
  may call into the scheduler, never the other way round.
- A thread never blocks, yields or sleeps while holding a `TryLock` (this
  is asserted).
- Blocking calls check their condition and mark the thread blocked with
  interrupts disabled throughout, so a wake-up cannot be lost.
- Interrupt handlers still only touch atomics, plus the scheduler's
  `IrqCell` (to wake threads). Nothing in the scheduler allocates.
- A kernel log line is written with preemption disabled, so lines from
  different threads never interleave.

## Address spaces

```text
0x0000_0000_0000_0000 ┐
  page 0 .. 4 MiB     │ never mapped (null-pointer accesses fault)
0x0000_0000_0040_0000 │ program image (static ELF, linked at 0x40_0000;
  .. 0x4000_0000      │   at most 16 MiB of segments)
  .. 0x7FFE_F000      │ unmapped guard page
0x7FFF_0000..0x8000_0000 user stack (64 KiB, writable, no-execute)
0x0000_7FFF_FFFF_FFFF ┘ end of the user half
---------------------------------------------------------------
0xFFFF_8000_0000_0000 ┐ kernel half: kernel image, boot stack, boot info,
          ..          │ framebuffer, physical-memory window, ramdisk
          ..          │ (placed by the bootloader), the kernel's own slot
0xFFFF_FFFF_FFFF_FFFF ┘ (heap, thread stacks). Never user-accessible.
```

The bootloader is configured to place all of its mappings in the upper
half (`dynamic_range_start`), so the lower half belongs entirely to
processes. Each process gets its own PML4: entries 256-511 are copied from
the kernel's PML4 (no user bit anywhere on those paths), entries 0-255 hold
only the process's own pages. The kernel never adds or removes an
upper-half PML4 entry after boot, so the copies never go stale.

Page permissions come from the ELF segments: read-only, read+execute or
read+write. Writable and executable at once is refused by both the ELF
parser and the page-table mapper (W^X). Every user page is a fresh zeroed
frame. When a process has exited and its thread has been switched away
from, the idle thread frees every frame and table of its lower half and the
PML4 itself.

### User memory access by the kernel

The kernel never dereferences a user pointer. To read or write user memory
it walks the *calling process's* page tables for every page of the range,
requires each page to be present and user-accessible (and writable for
writes), and copies through the physical-memory window. Consequences:

- kernel, non-canonical, unmapped, overflowing and read-only ranges are
  rejected before a single byte is copied, never faulted on;
- SMEP and SMAP stay enabled when the CPU has them (verified with
  `-cpu max`): the kernel never runs or touches user pages through user
  addresses;
- a range checked at the start of a system call stays valid until the
  call returns, because a process has one thread and nothing unmaps its
  pages while it lives.

### CPU state that could leak

- x87, SSE and AVX are disabled for all code at boot (CR0.EM set, CR4.OSFXSR
  and OSXSAVE cleared). The scheduler does not save those registers, so
  leaving them enabled would let a process read values another left behind.
  A program that uses them is terminated with #UD/#NM.
- New user threads start with every general-purpose register zeroed except
  `rdi` (the startup handle) and `rsp`.
- RFLAGS starts as `0x202`: interrupts enabled, IOPL 0. The TSS has no I/O
  permission bitmap. `cli`, `hlt`, port I/O and `int n` for any vector but
  the system-call gate raise #GP.
- UMIP is not enabled, so `sidt`/`sgdt` reveal descriptor-table addresses
  to ring 3 (the `fault` test program uses this to find a mapped kernel
  page). This leaks layout information, not access.

## Executables and the program bundle

User programs are `no_std` Rust binaries in `userspace/apps`, built by
`cargo xtask user` for `x86_64-unknown-none` with
`-C relocation-model=static` and `--image-base=0x400000` (non-PIE `ET_EXEC`).
The build tool checks each ELF with the same parser the kernel uses
(`crates/exec`) and packs them into a **program bundle**: a flat archive
with a magic number, a version, up to 32 named entries (`[a-z0-9._-]`,
1-32 bytes) and 16-byte-aligned data. The image builder hands the bundle to
the bootloader as its ramdisk; the kernel validates it once at boot and
rejects any malformed entry.

The ELF loader accepts only 64-bit little-endian x86_64 `ET_EXEC` files
whose `PT_LOAD` segments lie inside the image range, do not share pages,
are not writable and executable, keep their file bytes inside the file and
total at most 16 MiB; the entry point must lie in an executable segment.
Dynamic linking (`PT_INTERP`, `PT_DYNAMIC`) and thread-local storage
(`PT_TLS`) are rejected. Both parsers are host-tested with malformed and
randomly corrupted input.

## System calls

A program executes `int 0x80` (the only gate with DPL 3). Arguments go in
`rdi`, `rsi`, `rdx`, `r10`, the call number in `rax`; the result comes back
in `rax`: a value `0..=i64::MAX`, or `-code` for an error. Every other
register is preserved. ABI version: **1** (`os_abi::ABI_VERSION`); `ulib`
refuses to run on a kernel with another version.

| # | Call | Arguments → result |
| --- | --- | --- |
| 0 | `ABI_VERSION` | → version |
| 1 | `EXIT` | code → never returns |
| 2 | `DEBUG_WRITE` | ptr, len (≤ 256) → len; one kernel log line `[user <name>/<pid>] ...`, non-printable bytes shown as `.` |
| 3 | `YIELD` | → 0 |
| 4 | `SLEEP` | milliseconds (capped at 60 s) → 0 |
| 5 | `CLOCK` | → nanoseconds since boot |
| 6 | `SPAWN` | name ptr, name len (≤ 32), channel handle or 0 → process handle; the channel endpoint moves to the child, which gets it in `rdi` |
| 7 | `WAIT` | process handle → encoded exit status (blocks until exit) |
| 8 | `CLOSE` | handle → 0 |
| 9 | `CHANNEL_CREATE` | out ptr → 0; writes two endpoint handles (`u32` each) |
| 10 | `CHANNEL_SEND` | handle, ptr, len (≤ 256) → 0; `WouldBlock` when the peer's inbox holds 8 messages |
| 11 | `CHANNEL_RECV` | handle, ptr, capacity, flags (`1` = don't wait) → length; blocks while empty |
| 12–20 | File and volume calls | Open/read/write, list, mkdir, rename, remove, sync and volume kind; see [FILESYSTEM.md](../FILESYSTEM.md) |
| 21 | `FS_TEST_SHUTDOWN` | Init-only, designated disposable disk only: sync and Q35 S5 test power-off |
| 25 | `FS_SYNC_STEP` | One writeback sector or flush; 0=pending, 1=complete; mutations return WouldBlock until finished |

Errors: `UnknownSyscall`, `InvalidArgument`, `BadAddress`, `BadHandle`,
`WrongType`, `TooLarge`, `NoResources`, `NotFound`, `WouldBlock`,
`PeerClosed`, `BufferTooSmall` (the message stays queued), plus the filesystem
errors documented in [FILESYSTEM.md](../FILESYSTEM.md). Failed validation
changes nothing; failed disk sync can leave partial writes. Exit status:
an exit code, or "terminated by fault
vector *n*".

**Handles** are per-process values `(generation << 8) | (slot + 1)` in a
16-entry table; a slot's 24-bit generation changes on reuse, so closed
handle values stay invalid. Calls only look in the caller's own table, so a
process cannot name another's objects; channel endpoints move to a child at
spawn, which is the only way to share them. **Channels** are bidirectional
and message-based; when one endpoint closes, the other sees `PeerClosed`
once its inbox is empty. At most 16 processes and 32 channels exist at
once.

## Faults

An exception raised in ring 3 (the saved code segment has RPL 3) is the
process's fault, never the kernel's: the kernel prints
`[rust-os] USER FAULT: <name> (pid <n>): <exception> at rip ...` with the
decoded cause and address, closes the process's handles, wakes anyone
waiting for it, and switches away from its thread for good. Its parent sees
`Faulted { vector }` from `WAIT`. Every other process and the kernel keep
running. An exception raised by kernel code is still fatal and reported as
before.

## Tests

| Where | What |
| --- | --- |
| `cargo xtask test` | `os-abi` (result and status encodings, layout), `exec` (ELF and bundle parsers, including corrupted input), `mm` (user mappings, teardown frees every frame once), `xtask` (smoke judges, including spoofed markers) |
| Every boot | Scheduler self-test: three kernel threads that never yield must all progress and each observe the others running (preemption), join/sleep/yield ping-pong, and their stacks are reclaimed: `[rust-os] SCHED SELF-TEST PASS` |
| Every boot | `init` runs the process tests and the kernel reports its exit status: `[rust-os] PROCESS TESTS PASS` |
| `cargo xtask smoke --scenario processes` | Waits for the verdict, then checks the kernel's own reports: nine contained user faults (null read; read, write and execute of a mapped kernel page; `cli`; port I/O; `ud2`; SSE; stack overflow into the guard page), each process reported terminated, `init` exited with 0; then the kernel must still produce a heartbeat and decode a key press |

`init` checks: five IPC round trips with `echo` and its clean exit;
`PeerClosed` after the peer exits; three CPU-bound `spin` processes that
never yield all run overlapping in time. READY/GO coordinates loading, then the parent receives all START messages before sending STOP; workers do not block during that interval. A ten-second worker deadline and the VM timeout bound failures;
each `fault` mode ends with the expected vector; and `abuse` (78 checks)
sees every invalid call fail with the documented error: unknown numbers;
null, kernel, non-canonical, overflowing, unmapped, read-only and
guard-crossing pointers; oversized lengths; invalid, stale, closed and
wrong-type handles; handle-table exhaustion without leaks; full queues;
closed peers; unknown flags and programs.

It also runs filesystem tests through validated syscalls: directory/file
operations, full-volume failure and reuse, pointer/flag errors, owned
read-only/stale/deleted handles, and `fileprobe` in another address space.
Storage smoke adds a second boot after clean shutdown; see
[FILESYSTEM.md](../FILESYSTEM.md).

## Known limitations

- One CPU; no SMP, no per-CPU data, no TLB shootdown.
- One thread per process; no shared memory, no memory-allocation call, no
  arguments beyond one startup handle, no kill call. Files use a bounded,
  shared-access volume without per-application permissions.
- Round-robin only: no priorities, no real-time guarantees. Long kernel
  work with a `TryLock` held (for example a full-screen redraw) delays
  preemption until the next timer interrupt after it.
- Fixed tables (32 threads, 16 processes, 32 channels, 16 handles per
  process); one process can use up to 8 channels, so a few can exhaust the
  global channel table.
- Sends never block (`WouldBlock` on a full inbox).
- `int 0x80` rather than `syscall`/`sysret`: simpler and sharing the
  interrupt path, slower. The GDT order already allows `sysret` later.
- Kernel stacks and their bounded virtual reservations are reclaimed
  (a bounded set of shared page-table frames stays allocated).
- Programs come only from the boot bundle; filesystem executable loading is unsupported.
- Verified only in QEMU (q35, OVMF), default CPU model and `-cpu max`.
