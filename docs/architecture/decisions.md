# Architecture decisions

Short records of decisions that shape the public code. Status is *accepted*
unless noted. Supersede a record by adding a new one rather than editing
history.

## ADR-001 — x86_64 only, VM first

x86_64 in a VM, one CPU. QEMU is the first target; VirtualBox and VMware are
validated separately later. Other architectures are out of scope; supporting
several at once would multiply boot and driver work before anything runs.

## ADR-002 — UEFI, not BIOS

**Decision:** boot through UEFI only. BIOS images are not built.

**Why:** UEFI provides a linear framebuffer through GOP, which the graphical
desktop needs; QEMU (with OVMF), VirtualBox and VMware all offer EFI firmware;
and building only the UEFI stage removes the bootloader's 16/32-bit BIOS
stages from our build and dependency audit.

**Cost:** QEMU needs OVMF firmware supplied by the developer; legacy-BIOS-only
VM configurations are unsupported. Adding BIOS later means enabling the
`bios` feature of `bootloader`.

## ADR-003 — `bootloader` 0.11.17 as the boot path

**Decision:** use the rust-osdev [`bootloader`](https://github.com/rust-osdev/bootloader)
crate, version 0.11.17 (latest release, 2026-07-27), exact-pinned. The kernel
depends on `bootloader_api`; the host tool `image-builder` uses
`bootloader::UefiBoot` to write a GPT disk image.

**Why:** it is a maintained Rust bootloader, hands the kernel a memory map
and framebuffer, and keeps the boot path in Rust. Writing our own UEFI loader
is not justified before the kernel exists.

**Rejected:** the upstream template's artifact-dependency (`-Z bindeps`)
build-script setup — it ties every workspace build to the bootloader build.
A separate host tool keeps `cargo test` and lint fast and offline.

**Cost:** the bootloader contains third-party inline assembly and pulls in an
MPL-2.0 crate (`ucs2`); see [THIRD_PARTY_NOTICES.md](../../THIRD_PARTY_NOTICES.md).

## ADR-004 — Pinned nightly toolchain

**Decision:** `nightly-2026-10-08`, pinned in `rust-toolchain.toml`.

**Why:** `bootloader`'s build script requires `-Zbuild-std` (nightly-only);
upstream states 0.11.16+ needs nightly 2026-07-06 or newer. The kernel uses no
nightly language features, so moving it to stable later is possible if the
image step stops needing nightly.

**Rule:** bump the date deliberately, run `cargo xtask ci` and
`cargo xtask image`, and record the bump here.

## ADR-005 — Modular monolithic kernel, trusted desktop first

The kernel is one binary with clear internal boundaries. The desktop initially
runs as trusted code in the kernel address space. This is explicitly **not**
isolation. The `os-abi` crate and the `userspace/` boundary exist from day one
so the move to protected user processes is a planned phase, not a rewrite.

## ADR-006 — Rust xtask for build automation

All build, image, run and repository checks are Rust (`xtask`), not shell
scripts, so they work the same on Windows, Linux and macOS and stay inside the
Rust-only rule for project-owned tooling.

## ADR-007 — Lints that enforce the unsafe policy

Workspace lints deny `unsafe_op_in_unsafe_fn`, `clippy::undocumented_unsafe_blocks`
and `clippy::missing_safety_doc`, so every `unsafe` block needs a `// SAFETY:`
comment and every `unsafe fn` a `# Safety` section.

## ADR-008 — Sleep is unavailable by construction

`os-abi::PowerCapabilities` reports each power mechanism independently and
defaults to "not implemented". The desktop's power menu lists Sleep but keeps
it disabled unless the kernel reports verified suspend *and* resume. Host-side
VM pausing never counts.
