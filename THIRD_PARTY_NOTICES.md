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
| Local `iced_core` / `iced_widget` port, derived from Iced 0.14.0 (linked into the ring-3 desktop) | MIT; `core/src/layout/flex.rs` is Apache-2.0 | Source baseline: upstream commit `3997291f318a8bc06fa522f5579836fb3feb94df`. Preserve the Iced MIT notice and the xi-editor Authors / Héctor Ramón Apache notice. The reduced guest API and provenance are documented in [docs/iced-port.md](docs/iced-port.md) |
| `noto-sans-mono-bitmap` 0.2.0 (linked into the kernel via `crates/gfx`) | MIT (crate, © 2022 Philipp Schuster) | Glyphs rasterized from Noto Sans Mono, which is under the SIL Open Font License 1.1. Kernel uses regular + bold, 16 and 32 px, Basic Latin only. Ship the MIT notice and the OFL text with any released image |
| `bootloader-x86_64-uefi` / `-common` / `-boot-config` 0.11.17 | MIT OR Apache-2.0 | Contains inline assembly (third-party) |
| `uefi` 0.38, `uefi-raw` 0.15, `uefi-macros` 0.19, `uguid` 2.2 | MIT OR Apache-2.0 | |
| `x86_64` 0.15, `x86` 0.52, `raw-cpuid` 10.7 | MIT / Apache-2.0, MIT | |
| `ucs2` 0.3.3 | **MPL-2.0** | File-level copyleft: when distributing binaries, make the source of this crate (as used) available and keep its notices |
| `noto-sans-mono-bitmap` 0.2.0 (bootloader's own copy) | MIT | Same crate as above, used by the bootloader's log output |
| `ryu` 1.0 | Apache-2.0 OR BSL-1.0 | |
| `unicode-ident` 1.0 | (MIT OR Apache-2.0) AND Unicode-DFS-2016 | Build-time (proc-macro) dependency |
| `byteorder` 1.4 | Unlicense OR MIT | |
| Remaining crates in the UEFI stage lockfile (`log`, `spin`, `spinning_top`, `conquer-once`, `rand`, `rand_core`, `rand_hc`, `serde`, `serde-json-core`, `heapless`, `hash32`, `volatile`, `xmas-elf`, `uart_16550`, `ptr_meta`, `zero`, `bit_field`, `bitflags`, `lock_api`, `scopeguard`, ...) | MIT and/or Apache-2.0 | Some are build-time only; `atomic-polyfill` 1.0.3 and `critical-section` 1.2.0 confirmed MIT OR Apache-2.0 from exact cached manifests |
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

No automated advisory scan (for example `cargo deny`) is
configured yet. Unsafe code and build scripts of guest dependencies have not
been reviewed beyond identifying them.

The experimental user-space display also links `gfx` and the same bitmap font.
The font crate's MIT notice and Noto's OFL copyright/license are preserved in
`licenses/noto-sans-mono-bitmap-MIT.txt` and `licenses/Noto-OFL.txt`.
OFL source: https://raw.githubusercontent.com/notofonts/noto-fonts/main/LICENSE.
Window controls and desktop symbols are original Rust drawing primitives under
this project's MIT license; no external icon pack or branding is used.

## Exact release inventory

`cargo xtask release --release` collects the union of the workspace and exact
UEFI-stage lockfile packages, including optional and host-only crates. The
bundle includes each original crate archive (full source, including ucs2),
root license/notice files, both lockfiles and declared licenses/checksums in
`dependencies/INVENTORY.md`. Bootloader sibling crates share the root
bootloader MIT/Apache notices. Noto font OFL is in source/licenses. Rust
copyright and library notices, MIT/Apache and LLVM exception are included.
Missing cached sources or license declarations fail packaging. This is a
conservative distribution inventory, not a dependency vulnerability audit.
Host-only SHA-256 tooling uses sha2 0.10.9 (MIT OR Apache-2.0), not guest code.

The modified Iced sources are local path dependencies under `vendor/iced/`.
Their source and license texts must accompany releases in the public source
snapshot; they are not unmodified registry archives in the dependency cache.
The notices are [Iced MIT](vendor/iced/LICENSE) and
[Apache-2.0](vendor/iced/LICENSE-APACHE).
See [the port provenance and limits](docs/iced-port.md) before extending or
redistributing this subset.
