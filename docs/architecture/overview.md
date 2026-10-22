# Architecture overview

## Shape

A **modular monolithic kernel** for x86_64. Subsystems are separate Rust
modules and crates with one-way dependencies running in ring 0. User
programs run as isolated processes in ring 3, each in its own address
space, and talk to the kernel only through the versioned system-call
interface in `os-abi` (see [processes.md](processes.md)). The interactive desktop runs as a trusted ring-3 shell process. Its Rust
window manager, dock and task controls use the host-tested `desktop` crate;
the kernel exposes only validated device, storage, power and read-only
monitoring calls. See [desktop environment](../desktop-environment.md).

| Layer | Components |
| --- | --- |
| External boot | UEFI firmware and the Rust bootloader UEFI stage |
| Kernel architecture | CPU tables, faults, IRQs, PIC/PIT, context switching |
| Kernel services | Validated boot data, memory, scheduler, process/ELF/handles/channels/syscalls, VFS, device drivers and Q35 power |
| Device boundary | RGB row copies, bounded input queue, file calls, power requests and read-only system snapshots |
| Ring-3 desktop | Shell, window manager/compositor, dock, Task Manager and built-in Files/Trash/Settings/About views |
| User support | `os-abi`, `ulib` and static ELF programs from the boot bundle |


Rules that keep the later split possible:

- `userspace/*` crates never touch hardware, never use kernel internals, and
  depend only on `os-abi` (plus pure no_std helpers).
- `os-abi` holds plain data types and policies; nothing in it assumes a shared
  address space. Its types are `Copy` and pointer-free where practical so they
  can cross a system-call boundary.
- Shared logic is `no_std` and host-testable; hardware access stays in narrow,
  documented `unsafe` wrappers inside the kernel.
- Interrupt handlers only capture bounded data into lock-free rings (and
  wake threads through the scheduler's interrupt-safe cell) and never lock,
  allocate or print; decoding happens in the `ui` thread. CPU exception
  handlers never allocate or wait on locks.
- No thread is ever preempted while it holds a lock; blocking with one held
  is a checked bug. See [processes.md](processes.md#locking-rules).

## Workspace

| Crate | Kind | Target | Purpose |
| --- | --- | --- | --- |
| `kernel` | bin, `no_std`, `no_main` | `x86_64-unknown-none` | Entry, CPU tables and exception reports, memory management and its self-test, 8259/PIT/IRQ dispatch, TSC clock, PS/2 driver, event loop and input screen, serial driver, boot-info checks, framebuffer ownership, panic handler, scheduler and its self-test, process address spaces, ELF loader, handles, channels, system calls |
| `os-abi` | lib, `no_std`, no `unsafe` | any | Kernel ↔ user-space interface: system-call numbers, errors and their encoding, exit status, user address layout and limits, power-action availability |
| `exec` | lib, `no_std`, no `unsafe` | any | Validating static ELF64 parser and the program-bundle format; used by the kernel loader and by xtask |
| `ulib` | lib, `no_std` | `x86_64-unknown-none` | User-space support: entry point, panic handler, system-call wrappers (`int 0x80`), handles, channels, processes, `println!` |
| `apps` | bins, `no_std`, `no_main` | `x86_64-unknown-none` | User programs in the boot bundle: `init` (process test driver), `echo`, `spin`, `fault`, `abuse` |
| `gfx` | lib, `no_std`, no `unsafe` | any | Framebuffer layout validation, clipped drawing and blitting, bitmap text, boot screens, input panel, pointer, push button |
| `input` | lib, `no_std`, no `unsafe` | any | Interrupt-safe byte rings, scancode set 1 and PS/2 mouse decoders, pointer state, bounded event queue; see [interrupts-input.md](interrupts-input.md) |
| `boot-data` | lib, `no_std`, no `unsafe` | any | Memory-map validation and copy; serial boot markers shared with `xtask smoke` |
| `mm` | lib, `no_std`, `unsafe` only in the heap | any | Typed addresses, frame bitmap, page-table mapper (generic over table access), reclaiming free-list heap; see [memory.md](memory.md) |
| `desktop` | lib, `no_std` | any | Software display/window core, protocol, surfaces; Computer/Files/Trash and power policy |
| `xtask` | bin, `std` | host | Build orchestration and repository checks |
| `image-builder` | bin, `std` | host | Wraps the kernel ELF into a UEFI disk image |

`default-members` lists only host-testable crates so `cargo test` and
`cargo clippy` work without a target flag; xtask builds the kernel and the
user programs with `--target x86_64-unknown-none`.

## Isolation path

1. Done: per-process page tables, ring 3, a system-call gate, validated
   arguments and user pointers, handle-based access, fault containment
   (see [processes.md](processes.md)). Programs come only from the boot
   bundle built with the OS.
2. Done: the desktop and Files run in the ring-3 shell using drawing, input
   and file calls. Next: split built-in views into independent processes.
3. Only after more hardening (resource limits, a security review, more
   adversarial testing) may untrusted programs run.

## Out of scope for the first release

SMP, networking, GPU acceleration, USB beyond what a VM requires, audio, other
architectures, and guest sleep. Sleep stays visibly disabled until genuine
suspend/resume is implemented and tested.

## Experimental display service

The `desktop` kernel variant starts a ring-3 display service in place of the
normal init test driver. Software composition and window policy use the pure
`userspace/desktop` core; RGB row presentation and PS/2 input copies are the
narrow kernel boundary. The normal image retains the diagnostic UI.
See [display.md](display.md) for the protocol, memory ownership and controls.
