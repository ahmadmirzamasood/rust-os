# Building, checking and running

Everything is driven by the Rust `xtask` crate (`cargo xtask <command>`).
"Verified" below means the command was run successfully on the recorded host;
anything else is marked.

## Prerequisites

| Requirement | Why | Notes |
| --- | --- | --- |
| `rustup` | Installs the pinned toolchain from `rust-toolchain.toml` | Toolchain: `nightly-2026-10-08` with `rustfmt`, `clippy`, `rust-src`, `llvm-tools-preview`, target `x86_64-unknown-none` |
| A host linker | Builds `xtask` and `image-builder` | Linux/macOS: the system C toolchain. Windows: MSVC Build Tools, **or** the `x86_64-pc-windows-gnullvm` host with [llvm-mingw](https://github.com/mstorsjo/llvm-mingw) on `PATH` (`rustup set default-host x86_64-pc-windows-gnullvm`) |
| Network access, first `image` build only | `bootloader`'s build script runs `cargo install bootloader-x86_64-uefi` from crates.io | Cached afterwards in `target/` |
| QEMU (`qemu-system-x86_64` on `PATH`) | `cargo xtask run` | Not needed for build or tests. Windows: `winget install SoftwareFreedomConservancy.QEMU`, then add `C:\Program Files\qemu` to `PATH` |
| OVMF UEFI firmware | `cargo xtask run` | Linux: your distribution's `ovmf`/`edk2-ovmf` package. Windows QEMU builds bundle it as `share\edk2-x86_64-code.fd` + `share\edk2-i386-vars.fd` |

Why nightly: `bootloader` 0.11.17 builds its UEFI stage with `-Zbuild-std`,
which is nightly-only. Upstream states 0.11.16+ needs nightly 2026-07-06 or
newer. The kernel itself uses no nightly features.

## Commands

| Command | What it does | Status |
| --- | --- | --- |
| `cargo xtask lint` | `cargo fmt --all --check`; clippy on host crates (`--all-targets`); clippy on the kernel for `x86_64-unknown-none`; warnings are errors | Verified (Windows) |
| `cargo xtask test` | `cargo test` on host-testable crates (`os-abi`, `desktop`, `xtask`) | Verified (Windows) |
| `cargo xtask build [--release]` | Builds `target/x86_64-unknown-none/<profile>/kernel` (ELF) | Verified (Windows) |
| `cargo xtask image [--release]` | Builds the kernel, then `tools/image-builder` writes `target/images/<profile>/rust-os-uefi.img` | Verified; the image boots to the kernel hand-off in QEMU (see `run`) |
| `cargo xtask run --ovmf-code <file> [--ovmf-vars <file>] [--timeout <secs>]` | Builds the image and starts QEMU (`-machine q35 -m 256M -serial stdio -no-reboot`) | Verified (Windows, QEMU 11.1.0): firmware → bootloader → "Jumping to kernel entry point" on serial. The kernel prints nothing yet |
| `cargo xtask check-private` | Fails if private paths are tracked or staged, or if `.agent-private/` is not ignored | Verified (Windows) |
| `cargo xtask ci` | `lint` + `test` + `build` + `check-private` | Verified (Windows) |

`cargo xtask lint` does not lint `tools/image-builder`, because that compiles
the bootloader stage. Lint it explicitly when changing it:

```bash
cargo clippy -p image-builder -- -D warnings
```

Example (Windows, QEMU from winget, `C:\Program Files\qemu` on `PATH`):

```bash
cargo xtask run --ovmf-code "C:\Program Files\qemu\share\edk2-x86_64-code.fd" --ovmf-vars "C:\Program Files\qemu\share\edk2-i386-vars.fd" --timeout 20
```

QEMU also opens a display window showing the bootloader's framebuffer log.
`--timeout` stops it; without it, close the window or press Ctrl+C.

`RUST_OS_OVMF_CODE` / `RUST_OS_OVMF_VARS` may replace the `--ovmf-*` options.
The vars file is copied next to the image so the template is never modified.

## Image format

`rust-os-uefi.img` is a raw disk image (not an ISO) with a GPT partition table
and a FAT EFI System Partition holding `EFI/BOOT/BOOTX64.EFI` (the bootloader)
and the kernel. Attach it as a raw disk. VirtualBox and VMware require
conversion (for example `qemu-img convert -O vdi` / `-O vmdk`) and EFI firmware
enabled; neither has been tried.

## Virtual machine plan

1. **QEMU + OVMF** first: q35 machine, one CPU, 256 MiB, serial on stdio. All
   early testing happens here.
2. **VirtualBox** (version to be chosen): EFI enabled, converted disk.
3. **VMware** (product/version to be chosen): EFI firmware, converted disk.

Passing in QEMU will not be taken as evidence for VirtualBox or VMware; each
gets its own recorded test.
