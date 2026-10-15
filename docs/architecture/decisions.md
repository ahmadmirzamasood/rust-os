# Architecture decisions

Short records of decisions that shape the public code. Status is *accepted*
unless noted. Supersede a record by adding a new one rather than editing
history.

## ADR-001 — x86_64 only, VM first

x86_64 in a VM, one CPU. QEMU is the first target; VirtualBox and VMware are
validated separately later. Other architectures are out of scope; supporting
several at once would multiply boot and driver work before anything runs.

## ADR-002 — UEFI, not BIOS

**Decision:** boot through UEFI only. BIOS images are not built.

**Why:** UEFI provides a linear framebuffer through GOP, which the graphical
desktop needs; QEMU (with OVMF), VirtualBox and VMware all offer EFI firmware;
and building only the UEFI stage removes the bootloader's 16/32-bit BIOS
stages from our build and dependency audit.

**Cost:** QEMU needs OVMF firmware supplied by the developer; legacy-BIOS-only
VM configurations are unsupported. Adding BIOS later means enabling the
`bios` feature of `bootloader`.

## ADR-003 — `bootloader` 0.11.17 as the boot path

**Decision:** use the rust-osdev [`bootloader`](https://github.com/rust-osdev/bootloader)
crate, version 0.11.17 (latest release, 2026-07-27), exact-pinned. The kernel
depends on `bootloader_api`; the host tool `image-builder` uses
`bootloader::UefiBoot` to write a GPT disk image.

**Why:** it is a maintained Rust bootloader, hands the kernel a memory map
and framebuffer, and keeps the boot path in Rust. Writing our own UEFI loader
is not justified before the kernel exists.

**Rejected:** the upstream template's artifact-dependency (`-Z bindeps`)
build-script setup — it ties every workspace build to the bootloader build.
A separate host tool keeps `cargo test` and lint fast and offline.

**Cost:** the bootloader contains third-party inline assembly and pulls in an
MPL-2.0 crate (`ucs2`); see [THIRD_PARTY_NOTICES.md](../../THIRD_PARTY_NOTICES.md).

## ADR-004 — Pinned nightly toolchain

**Decision:** `nightly-2026-10-08`, pinned in `rust-toolchain.toml`.

**Why:** `bootloader`'s build script requires `-Zbuild-std` (nightly-only);
upstream states 0.11.16+ needs nightly 2026-07-06 or newer. The kernel uses no
nightly language features, so moving it to stable later is possible if the
image step stops needing nightly.

**Rule:** bump the date deliberately, run `cargo xtask ci` and
`cargo xtask image`, and record the bump here.

## ADR-005 — Modular monolithic kernel, trusted desktop first

The kernel is one binary with clear internal boundaries. The desktop initially
runs as trusted code in the kernel address space. This is explicitly **not**
isolation. The `os-abi` crate and the `userspace/` boundary exist from day one
so the move to protected user processes is a planned phase, not a rewrite.

## ADR-006 — Rust xtask for build automation

All build, image, run and repository checks are Rust (`xtask`), not shell
scripts, so they work the same on Windows, Linux and macOS and stay inside the
Rust-only rule for project-owned tooling.

## ADR-007 — Lints that enforce the unsafe policy

Workspace lints deny `unsafe_op_in_unsafe_fn`, `clippy::undocumented_unsafe_blocks`
and `clippy::missing_safety_doc`, so every `unsafe` block needs a `// SAFETY:`
comment and every `unsafe fn` a `# Safety` section.

## ADR-008 — Sleep is unavailable by construction

`os-abi::PowerCapabilities` reports each power mechanism independently and
defaults to "not implemented". The desktop's power menu lists Sleep but keeps
it disabled unless the kernel reports verified suspend *and* resume. Host-side
VM pausing never counts.

## ADR-009 — Early diagnostics: polled COM1, one assembly module

**Decision:** the kernel brings up a polled 16550 UART on COM1 before
anything else and logs every boot stage with a `[rust-os]` prefix. All inline
assembly in project-owned kernel code lives in `kernel/src/arch/x86_64.rs`:
`in`/`out` for port I/O, `cli`, and `cli; hlt`. The panic handler reports over
serial first, then draws a banner only if the framebuffer is free, and never
allocates or spins on a lock.

**Why:** serial works before (and without) graphics, is captured by every VM,
and makes black-screen failures diagnosable. Keeping assembly in one small,
documented file keeps the Rust-only claim auditable.

**Cost:** output-only, polled serial; lines may interleave once interrupts or
more CPUs exist.

## ADR-010 — Boot smoke test via serial markers and QMP screendump

**Decision:** `cargo xtask smoke` boots images in headless QEMU with the
serial port in a file and QMP on stdio. Success and failure are explicit
serial markers defined once in `crates/boot-data` and shared by kernel and
test. Drawing scenarios are checked with a QMP `screendump` against colours
defined once in `crates/gfx`. A deliberate panic is tested with a kernel built
with the `deliberate-panic` feature into a separate target directory and
image.

**Why:** markers plus a bounded timeout give a clear pass/fail without a
guest-side exit device, so the tested image is the shipped image (except for
the panic scenario). Checking real pixels catches stride and channel-order
bugs that serial output cannot. Rejected for now: QEMU `isa-debug-exit`
(needs a test-only guest backend in the normal image).

**Cost:** the panic scenario boots a different kernel build than the normal
one. Screenshots depend on QEMU's display device; other VMs need their own
evidence.

## ADR-011 — Kernel-owned CPU tables and recoverable probes

**Decision:** the kernel installs its own GDT (code, data, TSS), a TSS
whose IST slot 1 serves double faults, and an IDT covering the 32 exception
vectors. Each vector has a `global_asm` entry stub that saves all registers
and calls one Rust dispatcher. Fatal exceptions are reported on serial and
then panic. Faults raised by three small probe routines (read, write, call)
are recovered, so self-tests can prove that guard, read-only and
no-execute pages really fault. All project-owned assembly remains under
`kernel/src/arch/x86_64/` (this supersedes the single-file rule of
ADR-009).

**Why:** without a TSS, a stack overflow escalates to a double fault on the
same exhausted stack and resets the machine silently. Hand-written stubs
avoid the unstable `x86-interrupt` ABI and keep the register frame
explicit. Recoverable probes are the only way to test real fault behaviour
in the running kernel without a separate image per check.

**Cost:** about 60 lines of assembly, documented in place. An `asm!`
register-allocation bug in segment reloading (a `lateout` scratch register
that could share an input's register) only showed up in release builds.
The smoke test now runs in both profiles before a change is accepted.

## ADR-012 — Memory management design

**Decision:**

- **Physical access:** ask the bootloader for a full physical-memory
  window. Only `kernel/src/memory/phys.rs` turns physical addresses into
  pointers, for page tables and owned frames.
- **Frames:** a bitmap with separate *managed* and *free* bits.
  Reservations come from walking the live page tables at boot (every table
  frame and every frame a boot mapping points to), not from a hand-kept list
  of what the bootloader might use.
- **Kernel mappings:** the kernel claims one unused PML4 slot and never
  edits bootloader mappings. Inside it are the heap, guarded stacks and
  self-test scratch space. A mapper generic over table access enforces W^X
  and no-replace, and returns a must-use TLB flush token.
- **Heap:** a first-fit, address-ordered free list with coalescing. It has
  16-byte granularity, grows on demand by mapping pages, and refuses growth
  up front when frames or address space would not suffice.
- Bookkeeping lives in the host-tested `mm` crate. The kernel adds the
  machine-facing parts and an on-boot self-test.

**Why:** walking the live tables catches memory the firmware map wrongly
calls usable. In QEMU it found the bootloader's still-mapped trampoline
frames. A private PML4 slot makes ownership trivially checkable. A free
list is small, reclaims memory, and its invariants can be checked
mechanically.

**Rejected for now:** the `x86_64` crate (larger unsafe surface than the
few structures needed, see ADR-009). The `linked_list_allocator` crate
(same algorithm class; own code keeps alignment and failure rules
documented and tested here). A recursive page-table mapping (more address
arithmetic, and still needs physical access for new frames).

**Cost:** bootloader-owned memory is never reclaimed; the heap never
shrinks; only 4 KiB pages are created. See
[memory.md](memory.md#known-limitations).

## ADR-013 — 8259 PIC, PIT tick, TSC clock

**Decision:** external interrupts use the legacy 8259 pair, remapped to
vectors 32–47, with the local APIC left in the firmware's virtual-wire mode.
The PIT drives a 1000 Hz wake-up tick on IRQ 0. Uptime comes from the TSC,
calibrated once against PIT channel 2 by polling. The IDT has stubs for all
256 vectors, so a stray vector is counted instead of faulting.

**Why:** on one CPU in QEMU `q35`, the 8259 needs no ACPI parsing and no
MMIO mappings, so it is the smallest working path. Measured on the Windows
host, QEMU delivered only 85–700 of the 1000 PIT interrupts per second (the
Ubuntu CI runner got all 1000), so a tick counter is not a reliable clock.
Polled calibration does not depend on interrupt delivery.

**Rejected for now:** I/O APIC + local APIC (needed later for SMP and MSI;
requires MADT parsing and MMIO); HPET (more setup for no benefit at this
stage).

**Cost:** assumes a constant-rate TSC. Must be revisited for SMP and for
VirtualBox/VMware.

## ADR-014 — Interrupt-side byte rings, main-loop decoding

**Decision:** keyboard and mouse handlers only push raw bytes into fixed
lock-free single-producer/single-consumer rings (256 and 512 bytes). Full
means drop the new byte and count it. Decoding, pointer state, a 64-event
queue (merge moves, else drop oldest), drawing and logging run in the main
loop. The logic lives in the host-tested `input` crate, which has no
`unsafe` code.

**Why:** handlers stay short and lock-free, so a handler can never wait on
the code it interrupted. Dropping the newest byte keeps the producer out of
slots the consumer may be reading. Drop counters let decoders resynchronize
multi-byte sequences.

**Verified:** a deliberate 8 s main-loop stall overflowed both rings in
QEMU; the system recovered without corruption or deadlock (see
[interrupts-input.md](interrupts-input.md)).

## ADR-015 — Lower half for processes, upper half for the kernel

**Decision:** the bootloader places every mapping it chooses (kernel
image, boot stack, boot information, framebuffer, physical-memory window,
ramdisk) in the upper half (`dynamic_range_start = 0xffff_8000_0000_0000`).
Each process gets its own PML4 whose upper half is a copy of the kernel's
entries and whose lower half holds only its own pages.

**Why:** a fixed split makes "is this a user address?" a range check, keeps
kernel mappings identical in every address space, and needs no change to the
kernel's own mapping rules (it still only edits its own slot). The kernel
never adds an upper-half PML4 entry after boot, so the copies stay valid.

**Cost:** relies on that invariant; adding kernel PML4 entries later would
need propagation to every process. The bootloader's identity-mapped
trampoline stays in the kernel's own PML4 (lower half) but is not copied
into processes.

## ADR-016 — Preemptive round-robin; preemption off while a lock is held

**Decision:** one CPU, one FIFO ready queue, a 10 ms slice measured with the
TSC and checked on the timer interrupt; switching happens when an interrupt
or system call returns. Scheduler and process state live in an `IrqCell`
(interrupts disabled while used). Holding any `TryLock` disables
preemption, and blocking while holding one is a checked bug.

**Why:** the existing kernel relies on `TryLock` never being contended
outside exception re-entry. Disabling preemption while one is held keeps
that true without spinning or sleeping locks, and keeps interrupts enabled
during long work such as screen updates (unlike disabling interrupts).
Round-robin is the simplest policy that guarantees every runnable thread
progresses.

**Rejected for now:** priorities, per-thread wait queues, sleeping
mutexes (nothing needs them yet), `swapgs`-based per-CPU data (one CPU).

**Cost:** a long locked section delays preemption to the next tick after
it. Revisit for SMP.

## ADR-017 — `int 0x80` system calls, handles, channels

**Decision:** system calls enter through a DPL-3 interrupt gate at vector
0x80, reusing the interrupt entry path (full register frame, same stack
switch through TSS.RSP0). The interface is in `os-abi`, versioned
(`ABI_VERSION` 1), with twelve calls driven by the test programs.
Resources are named by per-process, generation-checked handles; IPC is
bounded bidirectional message channels whose endpoints move to a child at
spawn.

**Why:** one entry path means one place that saves state and one place that
can preempt. No `swapgs` or MSR setup is needed. Handles in a per-process
table make "can this process name that object?" a table lookup.

**Rejected for now:** `syscall`/`sysret` (faster, but needs `swapgs`, a
separate entry stub and care with non-canonical return addresses; the GDT
order already allows it); POSIX-like file descriptors (no files yet).

## ADR-018 — Static ELF programs in a boot bundle

**Decision:** user programs are static, non-PIE `ET_EXEC` files linked at
`0x400000`, checked by a host-tested parser (`crates/exec`) at build time and
again at boot, and shipped as one project-defined bundle that the bootloader
loads as its ramdisk.

**Why:** there is no file system yet; the ramdisk path already exists in the
bootloader. Static executables need no relocation or dynamic linker, so the
loader is small and every field it trusts is validated.

**Cost:** fixed link address (no ASLR); programs are known at image-build
time.

## ADR-019 — x87/SSE/AVX disabled

**Decision:** at boot the kernel sets CR0.EM and clears CR4.OSFXSR,
OSXMMEXCPT and OSXSAVE. The kernel and user programs are compiled
soft-float.

**Why:** the scheduler does not save FPU/SIMD registers. With the units
enabled (UEFI leaves SSE on), one process could read register values another
left behind. Disabled, such code is terminated instead.

**Cost:** user programs cannot use floating-point hardware. Enabling it
later needs `xsave`/`fxsave` state per thread.
