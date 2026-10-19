# rust-os (working name)

An experimental, open-source operating system written in Rust, with its own
graphical desktop: Computer, Files and Trash icons, a taskbar, basic window
management, file browsing, and shutdown/restart. The look aims for the
clarity of GNOME and Pantheon without using their code or branding.

**Status: experimental graphical desktop, isolated processes and storage.** There is no general-purpose OS
yet. In QEMU with UEFI firmware the kernel boots and logs each boot stage
on the serial port. It installs CPU exception handling, validates the boot
information, sets up physical-frame allocation, kernel page mappings with
guard pages and W^X, and a reclaiming heap, and checks them with an on-boot
self-test. It programs the 8259 interrupt controller, a PIT timer tick, a
TSC-based clock, and the PS/2 keyboard and mouse. It then starts a
preemptive round-robin scheduler and runs Rust user programs as isolated
ring-3 processes with their own address spaces, a small versioned
system-call interface, handles and message channels. A crashing process is
terminated without affecting the kernel or other processes. The screen
shows a welcome card with a mouse pointer, an input-debug panel and a
clickable test button. A bounded RAM/RSFS1 filesystem is accessible through
validated user-space calls. The interactive command boots a user-space desktop with built-in views and bounded Files/Trash operations (see [Files](docs/files.md)).

| Area | State |
| --- | --- |
| Workspace, toolchain pin, lint/test commands | Working on Windows; run in CI on Ubuntu and Windows (macOS untested) |
| Kernel boot (serial log, boot-info checks, welcome screen, panic report) | Verified in QEMU 11.1.0 + bundled edk2 on Windows by `cargo xtask smoke` |
| UEFI disk image | Builds from a fresh checkout; boots in QEMU 11.1.0 |
| QEMU launch (`cargo xtask run`) | Verified on Windows with QEMU 11.1.0 and its bundled edk2 firmware |
| CPU exceptions (GDT/TSS/IDT, double-fault stack, fault reports) | Verified in QEMU (Windows host): deliberate page fault and stack overflow are reported by `cargo xtask smoke` |
| Memory management (frames, kernel mappings, guard pages, W^X, heap) | Verified in QEMU (Windows host): on-boot self-test passes in debug and release; see [docs/architecture/memory.md](docs/architecture/memory.md) |
| Interrupts, timer, PS/2 keyboard and mouse, pointer, input panel | Verified in QEMU by `cargo xtask smoke --scenario input` (Windows host; GitHub CI on Ubuntu): injected keys, pointer moves and clicks, queue overflow and recovery, timer vs host time; see [docs/architecture/interrupts-input.md](docs/architecture/interrupts-input.md). USB input is not supported |
| Kernel threads, preemptive round-robin scheduler, idle thread | Verified in QEMU (Windows host, debug and release): on-boot scheduler self-test (never-yielding threads all progress and are preempted; join, sleep, yield; stacks reclaimed) |
| User processes in ring 3: address spaces, static ELF loader, system calls, handles, channels, fault containment | Verified in QEMU by `cargo xtask smoke --scenario processes` (Windows host, debug and release, 10 repeated runs; also with `-cpu max`, SMEP/SMAP on): IPC, preemption of CPU-bound processes, 9 contained crash kinds including reads, writes and jumps into kernel memory, 78 invalid-argument checks; see [docs/architecture/processes.md](docs/architecture/processes.md). Also passes in GitHub CI on Ubuntu |
| Software display and desktop | Ring-3 shell, launcher, built-in views and saved accent; see [desktop.md](docs/desktop.md). Files/Trash controls and limits are in [files.md](docs/files.md) |
| Storage and filesystem | Verified in QEMU 11.1.0 on Windows, debug and release: RAM/RSFS1, PIIX3 ATA PIO, user file calls, clean-shutdown persistence and corrupt-volume rejection. See [docs/FILESYSTEM.md](docs/FILESYSTEM.md) for limits and evidence |
| Shutdown / restart | General power management not started; storage smoke has a restricted Q35 test power-off |
| Sleep | Deliberately unavailable until real suspend/resume exists |
| VirtualBox / VMware | Planned, untested |

## Target

- x86_64 only, one CPU, running in a virtual machine.
- UEFI boot through the [`bootloader`](https://github.com/rust-osdev/bootloader)
  crate (0.11.17). BIOS is not built.
- QEMU first; VirtualBox and VMware later.
- Modular monolithic kernel. User programs run in ring 3 in separate
  address spaces; the optional desktop service runs in a separate user process.
  Only programs built with the OS (the boot bundle) can run; this is not a
  platform for untrusted code yet.

## Quick start

Requires `rustup`; the pinned nightly toolchain installs itself from
[`rust-toolchain.toml`](rust-toolchain.toml).

```bash
cargo xtask ci
```

With QEMU and OVMF installed, boot it in a window (serial log on the
terminal), or run the automated boot smoke test:

```bash
cargo xtask run
```

```bash
cargo xtask smoke
```

See [docs/building.md](docs/building.md) for prerequisites, every command, the
image format, and what the smoke test checks.

## Layout

```text
kernel/              no_std kernel binary (x86_64-unknown-none)
crates/os-abi/       no_std system-call interface shared by kernel and user space
crates/exec/         no_std ELF parser and program-bundle format (host-tested)
crates/storage/      no_std block/VFS interfaces, RAM filesystem, RSFS1 (host-tested)
crates/gfx/          no_std framebuffer drawing, bitmap text, boot screens (host-tested)
crates/boot-data/    no_std boot-info validation and serial boot markers (host-tested)
crates/mm/           no_std frame allocator, page-table mapper, heap (host-tested)
crates/input/        no_std input rings, PS/2 decoders, pointer, event queue (host-tested)
userspace/desktop/   no_std software display/window core and shell policy (host-tested)
userspace/ulib/      no_std user-space support library (entry, syscalls, IPC)
userspace/apps/      user programs: init (process/file tests), echo, spin, fault, abuse, fileprobe
xtask/               host build tool: lint, test, build, image, run, smoke
tools/image-builder/ host tool wrapping the kernel into a UEFI disk image
docs/                public architecture and build documentation
```

Architecture: [docs/architecture/overview.md](docs/architecture/overview.md).
Memory management: [docs/architecture/memory.md](docs/architecture/memory.md).
Interrupts and input: [docs/architecture/interrupts-input.md](docs/architecture/interrupts-input.md).
Threads, processes and system calls: [docs/architecture/processes.md](docs/architecture/processes.md).
Decisions: [docs/architecture/decisions.md](docs/architecture/decisions.md).

## Rust boundary

Project-owned code — kernel, drivers, services, applications and build tools —
is Rust. Minimal Rust inline/global assembly is permitted only where CPU entry,
context switching or hardware access require it, and each use is documented
(kernel: `kernel/src/arch/x86_64/`; user space: the `int 0x80` wrapper in
`userspace/ulib/src/sys.rs`, plus deliberate faults in the `fault` test
program).
Firmware (OVMF), compilers, linkers and emulators are external dependencies
and not part of that claim. Third-party crates compiled into the guest image
are listed in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

## Contributing and license

Read [CONTRIBUTING.md](CONTRIBUTING.md) and [SECURITY.md](SECURITY.md).
Licensed under the [MIT License](LICENSE).

Graphical desktop: run `cargo xtask run --release` or `cargo xtask desktop --release`.
The existing diagnostic boot remains available through `cargo xtask diagnostics`.
See [display architecture and protocol](docs/architecture/display.md) for
controls, limits, validation commands and the kernel/device boundary.

See [desktop controls and limits](docs/desktop.md) for the shell, built-in
applications, saved preferences and reboot test.
