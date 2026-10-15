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
            ├─ arch: CPU entry, interrupts, paging    (planned)
            ├─ memory: frames, mapping, heap          (planned)
            ├─ drivers: serial, input, block          (planned)
            ├─ graphics: framebuffer, drawing         (planned)
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
- Interrupt handlers only capture bounded data and enqueue events.

## Workspace

| Crate | Kind | Target | Purpose |
| --- | --- | --- | --- |
| `kernel` | bin, `no_std`, `no_main` | `x86_64-unknown-none` | Kernel entry (currently idle loop only) |
| `os-abi` | lib, `no_std` | any | Kernel ↔ user-space interface (power-action availability today) |
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
