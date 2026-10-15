# rust-os (working name)

An experimental, open-source operating system written in Rust, with its own
graphical desktop: Computer, Files and Trash icons, a taskbar, basic window
management, file browsing, and shutdown/restart. The look aims for the
clarity of GNOME and Pantheon without using their code or branding.

**Status: scaffolding.** There is no usable OS yet. In QEMU with UEFI
firmware, the bootloader loads the kernel and logs that it is jumping to the
kernel entry point. The kernel itself produces no output yet, so nothing past
that jump is observable.

| Area | State |
| --- | --- |
| Workspace, toolchain pin, lint/test commands | Working (Windows host verified; Linux/macOS untested) |
| Kernel | Compiles for `x86_64-unknown-none`; reached by the bootloader in QEMU; no output of its own |
| UEFI disk image | Builds; boots through firmware and bootloader in QEMU 11.1.0 |
| QEMU launch (`cargo xtask run`) | Verified on Windows with QEMU 11.1.0 and its bundled edk2 firmware |
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

```bash
cargo xtask image
```

See [docs/building.md](docs/building.md) for prerequisites, every command, the
image format, and how QEMU will be launched.

## Layout

```text
kernel/              no_std kernel binary (x86_64-unknown-none)
crates/os-abi/       no_std interface shared by kernel and user space
userspace/desktop/   no_std desktop shell model (host-tested)
xtask/               host build tool: lint, test, build, image, run
tools/image-builder/ host tool wrapping the kernel into a UEFI disk image
docs/                public architecture and build documentation
```

Architecture: [docs/architecture/overview.md](docs/architecture/overview.md).
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
