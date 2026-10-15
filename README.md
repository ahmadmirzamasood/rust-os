# rust-os (working name)

An experimental, open-source operating system written in Rust, with its own
graphical desktop: Computer, Files and Trash icons, a taskbar, basic window
management, file browsing, and shutdown/restart. The look aims for the
clarity of GNOME and Pantheon without using their code or branding.

**Status: kernel with memory management.** There is no usable OS yet. In
QEMU with UEFI firmware the kernel boots and logs each boot stage on the
serial port. It installs CPU exception handling and validates the boot
information. It then sets up physical-frame allocation, kernel page mappings
with guard pages and W^X, and a reclaiming heap, and checks all of it with an
on-boot self-test. Finally it draws a welcome screen and halts. There is no
input, interrupt-driven I/O, scheduler, file system or desktop yet.

| Area | State |
| --- | --- |
| Workspace, toolchain pin, lint/test commands | Working on Windows; run in CI on Ubuntu and Windows (macOS untested) |
| Kernel boot (serial log, boot-info checks, welcome screen, panic report) | Verified in QEMU 11.1.0 + bundled edk2 on Windows by `cargo xtask smoke` |
| UEFI disk image | Builds from a fresh checkout; boots in QEMU 11.1.0 |
| QEMU launch (`cargo xtask run`) | Verified on Windows with QEMU 11.1.0 and its bundled edk2 firmware |
| CPU exceptions (GDT/TSS/IDT, double-fault stack, fault reports) | Verified in QEMU (Windows host): deliberate page fault and stack overflow are reported by `cargo xtask smoke` |
| Memory management (frames, kernel mappings, guard pages, W^X, heap) | Verified in QEMU (Windows host): on-boot self-test passes in debug and release; see [docs/architecture/memory.md](docs/architecture/memory.md) |
| External interrupts, timer, input | Not started |
| Desktop, Files, Trash, window manager | Not started (only a host-tested model) |
| Shutdown / restart | Not started |
| Sleep | Deliberately unavailable until real suspend/resume exists |
| VirtualBox / VMware | Planned, untested |

## Target

- x86_64 only, one CPU, running in a virtual machine.
- UEFI boot through the [`bootloader`](https://github.com/rust-osdev/bootloader)
  crate (0.11.17). BIOS is not built.
- QEMU first; VirtualBox and VMware later.
- Modular monolithic kernel. The desktop initially runs as trusted code in the
  kernel address space, so there is **no isolation between applications yet**.

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
crates/os-abi/       no_std interface shared by kernel and user space
crates/gfx/          no_std framebuffer drawing, bitmap text, boot screens (host-tested)
crates/boot-data/    no_std boot-info validation and serial boot markers (host-tested)
crates/mm/           no_std frame allocator, page-table mapper, heap (host-tested)
userspace/desktop/   no_std desktop shell model (host-tested)
xtask/               host build tool: lint, test, build, image, run, smoke
tools/image-builder/ host tool wrapping the kernel into a UEFI disk image
docs/                public architecture and build documentation
```

Architecture: [docs/architecture/overview.md](docs/architecture/overview.md).
Memory management: [docs/architecture/memory.md](docs/architecture/memory.md).
Decisions: [docs/architecture/decisions.md](docs/architecture/decisions.md).

## Rust boundary

Project-owned code — kernel, drivers, services, applications and build tools —
is Rust. Minimal Rust inline/global assembly is permitted only where CPU entry,
context switching or hardware access require it, and each use is documented.
Firmware (OVMF), compilers, linkers and emulators are external dependencies
and not part of that claim. Third-party crates compiled into the guest image
are listed in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

## Contributing and license

Read [CONTRIBUTING.md](CONTRIBUTING.md) and [SECURITY.md](SECURITY.md).
Licensed under the [MIT License](LICENSE).
