# Third-party components and license obligations

This project's own code is MIT-licensed (see [LICENSE](LICENSE)). It depends on
third-party components with their own terms. This file records what is used,
where it ends up, and what distribution requires. It is a working audit, not
legal advice; re-check it whenever `Cargo.lock` or the toolchain changes.

## Inside the guest disk image

A UEFI image from `cargo xtask image` contains our kernel plus the UEFI
bootloader stage, which `bootloader` 0.11.17 compiles from the crate
`bootloader-x86_64-uefi` 0.11.17 using that crate's own lockfile.

| Component | License | Notes |
| --- | --- | --- |
| `bootloader_api` 0.11.17 (linked into the kernel) | MIT OR Apache-2.0 | |
| `bootloader-x86_64-uefi` / `-common` / `-boot-config` 0.11.17 | MIT OR Apache-2.0 | Contains inline assembly (third-party) |
| `uefi` 0.38, `uefi-raw` 0.15, `uefi-macros` 0.19, `uguid` 2.2 | MIT OR Apache-2.0 | |
| `x86_64` 0.15, `x86` 0.52, `raw-cpuid` 10.7 | MIT / Apache-2.0, MIT | |
| `ucs2` 0.3.3 | **MPL-2.0** | File-level copyleft: when distributing binaries, make the source of this crate (as used) available and keep its notices |
| `noto-sans-mono-bitmap` 0.2.0 | MIT | Rasterized from Noto Sans Mono; check upstream font license (SIL OFL 1.1) notice requirements before release |
| `ryu` 1.0 | Apache-2.0 OR BSL-1.0 | |
| `unicode-ident` 1.0 | (MIT OR Apache-2.0) AND Unicode-DFS-2016 | Build-time (proc-macro) dependency |
| `byteorder` 1.4 | Unlicense OR MIT | |
| Remaining crates in the UEFI stage lockfile (`log`, `spin`, `spinning_top`, `conquer-once`, `rand`, `rand_core`, `rand_hc`, `serde`, `serde-json-core`, `heapless`, `hash32`, `volatile`, `xmas-elf`, `uart_16550`, `ptr_meta`, `zero`, `bit_field`, `bitflags`, `lock_api`, `scopeguard`, ...) | MIT and/or Apache-2.0 | Some are build-time only; `atomic-polyfill` and `critical-section` licenses not yet confirmed |
| Rust `core` / `compiler_builtins` (via the toolchain, `-Zbuild-std`) | MIT OR Apache-2.0 (with LLVM exception for parts of `compiler_builtins`) | Toolchain-provided runtime code |

**Release obligation:** a published disk image must ship with the license texts
and copyright notices of every crate compiled into it (MIT and Apache-2.0
both require this), plus the MPL-2.0 source offer for `ucs2`. Generate the
final list from the exact lockfiles at release time rather than trusting this
table.

## Host-only tools (not in the image)

`tools/image-builder` uses `bootloader` 0.11.17 (MIT OR Apache-2.0) and its
host dependencies (`gpt`, `fatfs`, `anyhow`, `serde_json`, `tempfile`, `llvm-tools`,
...). They run on the developer's machine and are not distributed in the
image.

## External infrastructure (not covered by the project license)

- Rust toolchain (`rustc`, `cargo`, `rust-lld`, LLVM tools) — MIT OR Apache-2.0, with LLVM components under Apache-2.0 WITH LLVM-exception.
- OVMF UEFI firmware (EDK II) — BSD-2-Clause-Patent; supplied by the user, not redistributed by this project.
- QEMU (GPL-2.0), VirtualBox and VMware — external emulators/hypervisors, never linked into the image.

## Not yet audited

No automated license or advisory scan (for example `cargo deny`) is
configured yet. Unsafe code and build scripts of guest dependencies have not
been reviewed beyond identifying them.
