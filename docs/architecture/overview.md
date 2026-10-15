# Architecture overview

## Shape

A **modular monolithic kernel** for x86_64. Subsystems are separate Rust
modules and crates with one-way dependencies, all running in ring 0 in one
address space at first. The desktop and applications are written against a
narrow shared interface (`os-abi`) so they can later move into isolated user
processes without being rewritten.

```text
UEFI firmware (OVMF, external)
  └─ bootloader 0.11 UEFI stage (third-party Rust)
       └─ kernel                                   kernel/
            ├─ arch: port I/O, CPU tables, exceptions, kernel/src/arch/x86_64/
            │        8259 PIC, PIT, IRQ dispatch
            ├─ boot info: validate, copy memory map   crates/boot-data/ + kernel/src/main.rs
            ├─ memory: frames, mapping, stacks, heap  crates/mm/ + kernel/src/memory/
            ├─ drivers: 16550 serial (COM1)           kernel/src/serial.rs
            │           PS/2 keyboard + mouse          kernel/src/drivers/ (block planned)
            ├─ time: tick counter, TSC clock          kernel/src/time.rs
            ├─ input: rings, decoders, pointer        crates/input/ + kernel/src/input_events.rs
            ├─ event loop + input-debug screen        kernel/src/event_loop.rs, kernel/src/ui.rs
            ├─ graphics: framebuffer, drawing, text   crates/gfx/ + kernel/src/display.rs
            ├─ vfs + filesystems                      (planned)
            ├─ power: shutdown, restart               (planned)
            └─ trusted user-space modules (linked in for now)
                 └─ desktop shell, Files, ...         userspace/
                      ↕ os-abi interface               crates/os-abi/
```

Rules that keep the later split possible:

- `userspace/*` crates never touch hardware, never use kernel internals, and
  depend only on `os-abi` (plus pure no_std helpers).
- `os-abi` holds plain data types and policies; nothing in it assumes a shared
  address space. Its types are `Copy` and pointer-free where practical so they
  can cross a system-call boundary.
- Shared logic is `no_std` and host-testable; hardware access stays in narrow,
  documented `unsafe` wrappers inside the kernel.
- Interrupt handlers only capture bounded data into lock-free rings and
  never lock, allocate or print; decoding happens in the main loop. CPU
  exception handlers never allocate or wait on locks.

## Workspace

| Crate | Kind | Target | Purpose |
| --- | --- | --- | --- |
| `kernel` | bin, `no_std`, `no_main` | `x86_64-unknown-none` | Entry, CPU tables and exception reports, memory management and its self-test, 8259/PIT/IRQ dispatch, TSC clock, PS/2 driver, event loop and input screen, serial driver, boot-info checks, framebuffer ownership, panic handler |
| `os-abi` | lib, `no_std` | any | Kernel ↔ user-space interface (power-action availability today) |
| `gfx` | lib, `no_std`, no `unsafe` | any | Framebuffer layout validation, clipped drawing and blitting, bitmap text, boot screens, input panel, pointer, push button |
| `input` | lib, `no_std`, no `unsafe` | any | Interrupt-safe byte rings, scancode set 1 and PS/2 mouse decoders, pointer state, bounded event queue; see [interrupts-input.md](interrupts-input.md) |
| `boot-data` | lib, `no_std`, no `unsafe` | any | Memory-map validation and copy; serial boot markers shared with `xtask smoke` |
| `mm` | lib, `no_std`, `unsafe` only in the heap | any | Typed addresses, frame bitmap, page-table mapper (generic over table access), reclaiming free-list heap; see [memory.md](memory.md) |
| `desktop` | lib, `no_std` | any | Desktop model: Computer/Files/Trash icons, power menu |
| `xtask` | bin, `std` | host | Build orchestration and repository checks |
| `image-builder` | bin, `std` | host | Wraps the kernel ELF into a UEFI disk image |

`default-members` lists only host-testable crates so `cargo test` and
`cargo clippy` work without a target flag; xtask builds the kernel with
`--target x86_64-unknown-none`.

## Isolation path

1. Now: everything trusted, one address space. **No security isolation.**
2. Later: per-process page tables, ring 3, a system-call entry, and
   handle-based access; `os-abi` becomes the system-call interface; desktop
   and Files move out of the kernel.
3. Only after that may untrusted programs run.

## Out of scope for the first release

SMP, networking, GPU acceleration, USB beyond what a VM requires, audio, other
architectures, and guest sleep. Sleep stays visibly disabled until genuine
suspend/resume is implemented and tested.
