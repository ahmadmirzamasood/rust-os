# Graphical desktop

`cargo xtask run --release` (or `cargo xtask desktop --release`) boots the
custom Rust kernel into its ring-3 graphical shell. `cargo xtask diagnostics`
retains the original welcome/input screen and process-test driver. The original
two-process graphics demo remains covered by `smoke --scenario desktop`.
The shell image is `target/images/<profile>/rust-os-uefi-shell.img`.

Computer reports the mounted volume and opens Files with Enter/click. Files
provides navigation, metadata, file operations and a read-only text viewer.
Task Manager provides app controls and live process telemetry; see [desktop environment](desktop-environment.md).
Trash supports original-path restore and confirmed permanent deletion. See
[Files controls, errors and limits](files.md). Settings changes the
desktop accent. About reports version, build profile, architecture and ABI.
These six built-in views share the trusted shell process; independent client
processes remain supported by the display demo, but runtime executable discovery
and third-party application installation are unavailable.

## Motion and rendering

The launcher slides by 12 pixels over 140 ms, with a gliding selection and
mouse hover selection. Desktop icons and Applications have hover feedback;
windows have offset shadows, a highlighted active edge and a distinct close
button. The pinned dock distinguishes focused, running and minimized windows.
F4 toggles reduced motion for this boot; Settings reports the current choice.
Reduced motion snaps launcher transitions to their final state.

The shell retains a cursor-free desktop image. Pointer-only movement restores
the old cursor from that image and composes the new cursor into temporary rows;
it compares at most 38 scanlines rather than repainting the whole desktop.
Hover changes, dragging, application changes and the one-second uptime update
still reconstruct the scene. Only changed rows cross the display syscall.
Solid fills grow one encoded span and reuse it across rows, preserving stride
padding and all supported pixel formats. No extra full-screen allocation is
needed. Frame pacing subtracts processing time from a 16.67 ms budget, with a
minimum one-millisecond sleep when late. This is a target, not guaranteed FPS.

Every five seconds the serial log reports `SHELL RENDER`: submitted frames,
scene compositions, copied rows, cursor-only frames/rows, total loop work in
microseconds and the elapsed interval in milliseconds. Idle time is included
in the interval; a stationary desktop intentionally submits few frames. These
counts are not monitor refresh rate, vsync or a benchmark against earlier code.

`cargo xtask shell-smoke [--release]` also sweeps the cursor, checks that the
desktop below the clock returns byte-for-byte to its original screenshot,
requires cursor-only rendering telemetry, and exercises the F4 toggle. Host
tests cover clipped cursors, stationary-cursor scene damage, failed-copy retry,
animation retargeting, completion and reduced-motion behavior. Software row
copies can still tear; no GPU acceleration or vsync is provided.

Local validation of these changes (Windows, rustc 1.101.0-nightly
`1d81eb4ad`, pinned nightly-2026-10-08, QEMU 11.1.0
`v11.1.0-12130-ge470268ff4`, bundled `edk2-x86_64-code.fd` and
`edk2-i386-vars.fd`, q35, 256 MiB, one CPU, 1280x800):

| Command | Result |
| --- | --- |
| `cargo xtask ci` | PASS: formatting, lint, host tests, guest builds, bundle validation and privacy checks |
| `cargo xtask smoke` | PASS: all nine debug scenarios, including input, process containment and display demo |
| `cargo xtask shell-smoke --release` | PASS: cursor sweep/F4, both persistence boots, Files, restart and clean shutdown |
| `cargo xtask shell-smoke` | PASS: the same checks in debug |

Command output is in `target/gui-ci.log`, `target/gui-smoke.log`,
`target/gui-shell-release.log` and `target/gui-shell-debug.log`. Shell serial
logs and screenshots are in `target/shell-test/release-1792614064745891700/`
and `target/shell-test/debug-1792614128391272500/`. One release sweep interval
reported `frames=123 scenes=8 cursor_frames=115 cursor_rows=2223` over 5017 ms.
This is a finite, host-paced input trace including idle time, not sustained
60 FPS evidence or a controlled before/after throughput comparison.

Click desktop icons or Applications to launch. The Applications menu also shows
Shut Down and Restart when the kernel detects supported Q35 power hardware.
Both require confirmation. Sleep stays disabled with a missing-resume explanation.
See [power coordination, VM configurations and evidence](power.md).
Esc opens the menu; typing searches applications, Backspace edits, and Esc
clears a query before closing the menu. Either Super key also toggles the menu.
Up/Down move the visible selection and Enter launches the matching application.
Tab switches between applications on the current workspace and restores minimized ones. F1 minimizes,
F2 restores applications on this workspace, F3 closes the focused application. Title controls minimize, maximize
and close; dragging the title moves a window. The pinned dock launches closed applications, restores running ones, and
minimizes the focused application on a second click. Focus has a blue title and the menu selection has
a blue background. Enter/Space or a click changes the Settings accent.

The numbered panel controls switch between four workspaces. F10/F11 select the
previous/next workspace; F12 moves the focused application to the next workspace
and follows it. The global dock returns to an application's existing workspace.
Focus and window geometry survive switching for this boot. See
[desktop environment](desktop-environment.md) for the complete controls and roadmap.

The top panel explicitly displays **Uptime**, derived from the monotonic kernel
clock. No wall-clock or time-zone setting is presented. Firmware chooses display
resolution; keyboard layout is US. Unsupported settings are explained as
unavailable rather than presented as actionable controls.

Preferences use the versioned five-byte `/desktop.pref` file through existing
validated file syscalls. Changes write and sync immediately; Settings reports
success or failure. A RAM volume is labelled temporary. Interactive run/desktop creates a new designated disposable RSFS1 data disk at
`target/desktop-data/DISPOSABLE-rsfs.img` once, then reuses it across launches.
Removing target also removes these preferences. Existing images are never
reformatted; corruption is reported by the kernel. See [FILESYSTEM.md](FILESYSTEM.md).
The existing filesystem has no power-loss recovery guarantee. Corrupt or unknown
preference encodings fall back to the cool accent. All applications share the
volume, including this preference file.

`cargo xtask shell-smoke [--release]` creates a new disposable data disk, boots
the shell, drives keyboard launches and window lifecycle, clicks a desktop icon,
checks content/background pixels, changes and syncs the accent, exits QEMU, and
boots again to check the loaded value and pixels. This tests an explicit sync
followed by guest restart and clean shutdown, then a new VM boot. Current power
test results and limits are recorded in [power.md](power.md).
Evidence is under `target/shell-test/<profile>-<unique-id>/`.
`cargo xtask ci` checks policy/protocol tests and bare-metal builds/lints;
`cargo xtask smoke` retains boot, fault, input, process and demo regressions.

Limits: software rendering, bounded 12 MiB arena, six built-in views, fixed-size
backing surfaces, bitmap ASCII text, PS/2 input, single display/CPU. Larger
accepted display modes may exhaust the arena and return an error. No Linux guest,
hosted UI, new assembly or guest dependencies are used by the shell.

Verified locally on 2026-10-19 with Windows, nightly-2026-10-08 and QEMU
11.1.0 (v11.1.0-12130-ge470268ff4), bundled edk2, q35, 256 MiB, one CPU,
1280x800 GOP mode: `cargo xtask ci` passed 145 host tests and all lint/build/
privacy checks. `cargo xtask shell-smoke` and `cargo xtask shell-smoke --release`
passed both VM boots in each profile. `cargo xtask run --release --headless
--timeout 15` reached `SHELL READY` with a persistent volume.

| Acceptance gate | Evidence / scope |
| --- | --- |
| Boots into desktop | Shell test boots and normal run readiness plus framebuffer checks |
| Launch, focus, minimize, restore, close | Host lifecycle tests and QMP keyboard lifecycle for built-in views |
| Keyboard and mouse | QMP launcher navigation, app switching and desktop icon click |
| Preferences survive reboot | Accent synced, then decoded and drawn after a new VM boot |
| Normal navigation needs no terminal | Launcher, window controls, task strip and Settings available in the GUI |

These gates apply to the bounded built-in desktop. General-purpose storage,
general ACPI power management, independent built-in application processes and compatibility
with other hypervisors or physical hardware remain unverified or unimplemented.

The existing regression suite also passed all nine scenarios in both debug and
release (`cargo xtask smoke`, `cargo xtask smoke --release`) on that Windows/QEMU
configuration. The CI workflow now includes shell-smoke; no hosted CI result is
claimed for these local changes.
