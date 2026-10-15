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
| QEMU (`qemu-system-x86_64`) | `cargo xtask run`, `cargo xtask smoke` | Not needed for build or tests. Found on `PATH`, via `RUST_OS_QEMU`, or at `C:\Program Files\qemu` on Windows (`winget install SoftwareFreedomConservancy.QEMU`) |
| OVMF UEFI firmware | `cargo xtask run`, `cargo xtask smoke` | Linux: your distribution's `ovmf`/`edk2-ovmf` package. Windows QEMU builds bundle it as `share\edk2-x86_64-code.fd` + `share\edk2-i386-vars.fd`. Common install locations are detected automatically |

Why nightly: `bootloader` 0.11.17 builds its UEFI stage with `-Zbuild-std`,
which is nightly-only. Upstream states 0.11.16+ needs nightly 2026-07-06 or
newer. The kernel itself uses no nightly features.

## Commands

| Command | What it does | Status |
| --- | --- | --- |
| `cargo xtask lint` | `cargo fmt --all --check`; clippy on host crates (`--all-targets`); clippy on the kernel for `x86_64-unknown-none`, normal and with each fault-test feature (`deliberate-panic`, `deliberate-page-fault`, `deliberate-stack-overflow`); warnings are errors | Verified (Windows) |
| `cargo xtask test` | `cargo test` on host-testable crates (`os-abi`, `gfx`, `boot-data`, `mm`, `desktop`, `xtask`) | Verified (Windows) |
| `cargo xtask build [--release]` | Builds `target/x86_64-unknown-none/<profile>/kernel` (ELF) | Verified (Windows) |
| `cargo xtask image [--release]` | Builds the kernel, then `tools/image-builder` writes `target/images/<profile>/rust-os-uefi.img` | Verified (Windows, including from an empty `target/`) |
| `cargo xtask run [--ovmf-code <file>] [--ovmf-vars <file>] [--timeout <secs>] [--headless]` | Builds the image and starts QEMU with a display window and the serial port on stdio | Verified (Windows, QEMU 11.1.0) |
| `cargo xtask smoke [--scenario boot\|panic\|no-display\|page-fault\|stack-overflow\|all] [--timeout <secs>] [--release]` | Automated boot test in headless QEMU; see below | Verified (Windows, QEMU 11.1.0, debug and release) |
| `cargo xtask check-private` | Fails if private paths are tracked or staged, or if `.agent-private/` is not ignored | Verified (Windows) |
| `cargo xtask ci` | `lint` + `test` + `build` + `check-private` (no QEMU needed) | Verified (Windows) |

`cargo xtask lint` does not lint `tools/image-builder`, because that compiles
the bootloader stage. Lint it explicitly when changing it:

```bash
cargo clippy -p image-builder -- -D warnings
```

Example with explicit firmware (Windows, QEMU from winget):

```bash
cargo xtask run --ovmf-code "C:\Program Files\qemu\share\edk2-x86_64-code.fd" --ovmf-vars "C:\Program Files\qemu\share\edk2-i386-vars.fd" --timeout 20
```

The window shows the welcome screen; the terminal shows the serial log.
`--timeout` stops QEMU; without it, close the window or press Ctrl+C.

`RUST_OS_OVMF_CODE` / `RUST_OS_OVMF_VARS` may replace the `--ovmf-*` options.
The vars file is copied before use so the template is never modified.

All QEMU runs use `-machine q35 -m 256M -smp 1 -no-reboot -no-shutdown`, the
firmware as pflash and the image as a raw disk.

## What the kernel does at boot

Every step logs a `[rust-os]` line on COM1 (115200 8N1):

1. Masks interrupts (no IDT exists yet) and initializes the 16550 UART,
   checking it with a loopback byte. Waits on the UART are bounded; without a
   UART, output is dropped instead of hanging.
2. Installs its own GDT, TSS and IDT: every CPU exception is reported, and
   double faults run on a separate stack so a stack overflow is reported
   too.
3. Checks the bootloader API version, then copies the memory map into
   kernel-owned storage, sorts it and validates it (non-empty, no inverted or
   overlapping ranges, page-aligned usable regions, some usable memory).
4. Takes the framebuffer out of the boot information and validates width,
   height, stride, bytes per pixel and pixel format against the buffer
   length. Drawing is clipped and never writes stride padding.
5. Sets up memory management: checks NX and write protection (and enables
   SMEP/SMAP when the CPU has them). It builds the frame allocator from the
   memory map and withdraws every frame the live boot page tables use. It
   then claims its own region of the address space, maps the heap and a
   guarded double-fault stack, and prints `[rust-os] memory ready:`. See
   [architecture/memory.md](architecture/memory.md).
6. Runs the memory self-test (frames, mappings, real faults on guard,
   read-only and no-execute pages, heap stress, exhaustion) and prints
   `[rust-os] MEMORY SELF-TEST PASS (<n> checks)`.
7. Draws the welcome screen and prints `[rust-os] BOOT OK`. If any stage
   failed, the screen and log say so and the final line is
   `[rust-os] BOOT DEGRADED`. Without a framebuffer, serial output continues.
8. Halts the CPU (`cli; hlt` loop).

On a panic the kernel prints `[rust-os] KERNEL PANIC` with the source
location and message, draws a red banner if the framebuffer is usable, and
halts. It never allocates or waits on a lock while doing so. A fatal CPU
exception first prints `[rust-os] CPU EXCEPTION:` with the decoded cause,
the faulting address (CR2), the registers and the guard page involved (if
any), then panics the same way.

## Boot smoke test

`cargo xtask smoke` builds the images first, then boots each scenario in
headless QEMU with the serial port written to a file and QMP (QEMU's control
protocol) on stdio. A scenario passes only on its explicit success signal and
fails on a failure signal, an early QEMU exit, or the timeout (default 120 s
per scenario, measured from QEMU start).

| Scenario | Image | Passes when |
| --- | --- | --- |
| `boot` | Normal | Serial has the memory self-test pass marker and `[rust-os] BOOT OK`, and no panic or CPU exception; a QMP screendump matches the kernel's reported resolution, the background colour at (1,1) is exact (catches RGB/BGR swaps), and title text, card and status dots are present |
| `panic` | Kernel built with the `deliberate-panic` feature | Serial has the full panic report: message, location, "banner drawn", "CPU halted"; the screendump shows the red banner over the welcome screen |
| `no-display` | Normal, QEMU with `-vga none` | No framebuffer is provided; serial reports `graphics unavailable`, the memory self-test still passes, and the log ends with `[rust-os] BOOT DEGRADED` |
| `page-fault` | Kernel built with `deliberate-page-fault` | After the self-test passes, the kernel writes into the unmapped guard page below its double-fault stack. Serial must report a page fault with cause "page not present, write, kernel mode", a CR2 equal to the written address, and that guard page as the diagnosis, followed by the full panic report; the screendump shows the banner |
| `stack-overflow` | Kernel built with `deliberate-stack-overflow` | After the self-test passes, the kernel recurses until the boot stack hits its guard page. Serial must report a double fault (handled on its own stack) diagnosed as the boot stack's guard page, followed by the full panic report; the screendump shows the banner |

Every scenario fails as soon as the log shows a memory self-test failure or a
memory initialization failure. A guest that triple-faults does not exit,
because QEMU runs with `-no-reboot -no-shutdown`; it shows up as the
timeout.

Output per scenario goes to `target/smoke/<profile>/<scenario>/`:
`serial.log` and, where the scenario draws, `screen.png`.

Fault-test kernels are built into `target/fault-tests/<feature>/` and their
images are named `rust-os-uefi-<feature>.img`, so they never replace the
normal kernel or image. The `deliberate-*` features exist only for these
tests. Never ship an image built with one.

## Continuous integration

[`.github/workflows/ci.yml`](../.github/workflows/ci.yml) runs on every push
to `master`, every pull request, and on demand:

- **check** (Ubuntu and Windows): `cargo xtask ci`.
- **boot-smoke** (Ubuntu): lints `image-builder`, then runs
  `cargo xtask smoke` (all scenarios) with the distribution's QEMU and OVMF.
  It has passed on GitHub for the `boot`, `panic` and `no-display`
  scenarios. The `page-fault` and `stack-overflow` scenarios and the memory
  self-test have so far been verified only on the Windows host above.

Actions are pinned to commit SHAs; the toolchain comes from
`rust-toolchain.toml`.

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
