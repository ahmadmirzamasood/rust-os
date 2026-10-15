# Contributing

The project is at an early stage: the kernel boots in QEMU to a welcome
screen and halts; nothing is usable yet. Focused, well-tested
changes are welcome. Open an issue first for anything larger than a small fix.

## Before you start

Read [README.md](README.md), [SECURITY.md](SECURITY.md),
[docs/architecture/overview.md](docs/architecture/overview.md) and
[docs/building.md](docs/building.md). You do not need any private maintainer
notes to contribute.

## Rules

- Project-owned code is Rust: kernel, drivers, services, applications and
  build tools. No C/C++ runtime code, no Linux kernel, no hosted UI standing in
  for the guest desktop.
- Inline or global assembly only where CPU entry, context switching or
  hardware access requires it. Document what it does and why it is sound.
- Every `unsafe` block carries a `// SAFETY:` comment stating its invariants
  (range, alignment, lifetime, aliasing, synchronization); every `unsafe fn` has
  a `# Safety` section. Clippy enforces this.
- Guest crates are `no_std`. Do not assume ordinary desktop crates work in the
  guest.
- Keep existing behavior working. Add tests that exercise real behavior,
  including a failure path where one exists.
- Before adding a dependency, check its license, `no_std` support, build script
  and unsafe/native code, and update [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)
  if it reaches the guest image.
- Never commit VM disks, logs with secrets, credentials or private notes.

## Checks

Run before every pull request:

```bash
cargo xtask ci
```

If you touched the kernel, `crates/gfx`, `crates/boot-data`,
`tools/image-builder` or boot configuration, also run `cargo xtask smoke`
(needs QEMU and OVMF). If you claim anything boots, include the exact command,
VM and firmware versions, and the serial output. Never describe untested
behavior as working.

## Licensing

Contributions are accepted under the project's [MIT License](LICENSE). Keep
third-party copyright notices intact.
