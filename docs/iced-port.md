# Iced guest port

The guest uses a local, reduced `no_std` port of Iced's core and widget code.
This is a source-derived subset of Iced 0.14.0, not the complete upstream
`iced` application runtime or a claim that arbitrary Iced applications run
on rust-os.

## Source and licensing

The source baseline is the published `iced_core` and `iced_widget` 0.14.0
crate archives. Both record upstream commit
`3997291f318a8bc06fa522f5579836fb3feb94df` in `.cargo_vcs_info.json`.
The archives were obtained from crates.io; their SHA-256 hashes are:

| Archive | SHA-256 |
| --- | --- |
| `iced_core-0.14.0.crate` | `91ab1937d699403e7e69252ae743a902bcee9f4ab2052cc4c9a46fcf34729d85` |
| `iced_widget-0.14.0.crate` | `b698f173f370d959cac589c2000268b44862543d6995cf8f2934cf07afad2e54` |

Upstream sources:

- [Iced core at the baseline commit](https://github.com/iced-rs/iced/tree/3997291f318a8bc06fa522f5579836fb3feb94df/core).
- [Iced widgets at the baseline commit](https://github.com/iced-rs/iced/tree/3997291f318a8bc06fa522f5579836fb3feb94df/widget).
- [Upstream MIT license](https://github.com/iced-rs/iced/blob/3997291f318a8bc06fa522f5579836fb3feb94df/LICENSE).

Iced is MIT-licensed. Its `core/src/layout/flex.rs` separately retains an
Apache-2.0 notice, copyright 2018 The xi-editor Authors and Héctor Ramón.
Preserve that source header and distribute both license texts with the port.
The local copies are [the Iced MIT license](../vendor/iced/LICENSE) and
[the Apache-2.0 license](../vendor/iced/LICENSE-APACHE).
See [third-party notices](../THIRD_PARTY_NOTICES.md).

## Guest integration

The integration keeps the existing ring-3 desktop process, window manager
and validated display/input system calls. The local core supplies layout,
widget state and message dispatch; the local widgets supply the supported
controls. A guest renderer translates their drawing into the existing
software framebuffer path. Application state changes remain in the shell,
including preference saving through the guest filesystem.

The port replaces standard-library imports with `core` and `alloc` and
narrows interfaces to the implemented guest facilities. It does not link
`winit`, `wgpu`, a host windowing library or a Linux runtime into the guest.
Floating-point layout arithmetic uses the bare-metal target's software
floating-point support. It must not enable unsaved CPU FP/SIMD state.

Settings uses the ported `Button` and `Column` with a bitmap-label adaptation
of Iced's text widget. `Row` and `Space` are also available. Controls retain
their state tree across declarative view rebuilds. Button styling uses an
explicit theme/status closure instead of upstream's theme catalog.

Settings provides accent and motion controls with pointer hover/press states
and a visible keyboard selection. Up/Down selects a control and Enter/Space
activates it through the button state machine. The guest adapter supplies
this keyboard behavior; the port does not expose the full upstream keyboard
event API. Accent changes use the existing preference file; reduced motion
applies only for the current boot, as the Settings view states.

The repository's checked-in port sources and manifests define the available
API; upstream examples may require APIs that are absent here. Detailed
source changes are listed in [the core port notes](../vendor/iced/PORT.md)
and [the widget port notes](../vendor/iced/widget/PORT.md).

## Limits

The port does not provide the upstream async task executor, subscriptions,
clipboard, IME, overlays, image/SVG loading, GPU rendering or advanced text
shaping. Text uses the guest's existing bitmap font and character coverage.
It does not add a general-purpose package/application installation system.
The desktop remains a trusted shared process with a bounded heap and the
existing window and surface limits.

The software renderer supports solid colors, alpha blending, rounded borders
and clipped bitmap text. Its clipping stack has sixteen entries; deeper
layers draw nothing until unwound. Quad coordinates are converted to integer
pixels. Although core types retain shadow and pixel-snapping metadata, this
renderer does not render shadows or implement upstream's selectable snapping
behavior. Geometry operations requiring trigonometry, square roots or
rotation are omitted from the local core. The existing fixed window backing
surface remains; changing a window's outer size does not reflow its content.

This is an initial integration, not a completed platform backend for the
whole Iced ecosystem. Further widgets should be ported with their state,
layout, input cancellation and rendering behavior together.

## Validation

Required repository checks remain:

```bash
cargo xtask ci
cargo xtask smoke
cargo xtask shell-smoke
```

Host tests should exercise widget layout, clipping and message delivery,
including disabled controls, cancelled pointer presses and release outside
a control. Guest validation must exercise the actual Settings controls in
QEMU, keyboard and mouse input, preference persistence, and the existing
desktop regression checks. Build success alone does not establish guest
boot, rendering, or compatibility with any VM.

Record the command, profile, QEMU/firmware versions, serial output and
screenshots for any claimed VM result. VirtualBox and VMware remain
unverified until separately tested.

### Recorded integration checks — 2026-10-22

On Windows, `cargo xtask ci` passed, including 48 desktop tests, the port's
core/widget tests, guest builds, linting and the private-path check.
`cargo xtask shell-smoke` and `cargo xtask shell-smoke --release` passed
the new iced pixel/input checks and the existing desktop, Files, restart,
shutdown and two-boot preference-byte checks.
`cargo xtask smoke` also passed all nine debug scenarios: boot, panic,
no-display, page-fault, stack-overflow, interrupt-fault, input, processes
and the independent display-service desktop demo. Output is in
`target/iced-smoke.log`, with per-scenario serial logs and screenshots in
`target/smoke/debug/`.

The VM was QEMU 11.1.0 (`v11.1.0-12130-ge470268ff4`), Q35, one CPU,
256 MiB, using its bundled edk2 firmware:

| File under `C:\Program Files\qemu\share` | SHA-256 |
| --- | --- |
| `edk2-x86_64-code.fd` | `33090cc07675baa5190d9f1e84bf5176b33bcbfa9bacac522961150cdb6dbb2a` |
| `edk2-i386-vars.fd` (copied before use) | `5d2ac383371b408398accee7ec27c8c09ea5b74a0de0ceea6513388b15be5d1e` |

Local command output is in `target/iced-ci.log`,
`target/iced-shell-smoke.log` and `target/iced-shell-smoke-release.log`.
Serial logs and screenshots are in these untracked evidence directories:

- `target/shell-test/debug-1792657596692922900/`
- `target/shell-test/release-1792657867019771000/`

Both shell runs reported:

```text
iced Settings PASS: hover/pressed pixels, release-only accent changes, outside cancellation, pointer and keyboard motion controls
shell boot 1: PASS (independent remount, preference bytes; Files checks=true)
shell boot 2: PASS (independent remount, preference bytes; Files checks=true)
```

The serial logs also record `SHELL ICED MOTION true/false`,
`SHELL PREFERENCE true/false Ok(())` and actual guest `POWER CLEAN` restart
and shutdown markers. Host tests additionally cover clipped releases,
input cancellation, mixed keyboard/mouse input, alpha blending, malformed
geometry and clipping-stack overflow. The failed first smoke attempt had an
incorrect keyboard-selection assumption after mouse focus; the corrected
test passed in both profiles.
