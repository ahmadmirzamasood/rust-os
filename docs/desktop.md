# Graphical desktop

`cargo xtask run --release` (or `cargo xtask desktop --release`) boots the
custom Rust kernel into its ring-3 graphical shell. `cargo xtask diagnostics`
retains the original welcome/input screen and process-test driver. The original
two-process graphics demo remains covered by `smoke --scenario desktop`.
The shell image is `target/images/<profile>/rust-os-uefi-shell.img`.

Computer reports the mounted volume and opens Files with Enter/click. Files
provides navigation, metadata, file operations and a read-only text viewer.
Trash supports original-path restore and confirmed permanent deletion. See
[Files controls, errors and limits](files.md). Settings changes the
desktop accent. About reports version, build profile, architecture and ABI.
These five built-in views share the trusted shell process; independent client
processes remain supported by the display demo, but runtime executable discovery
and third-party application installation are unavailable.

Click desktop icons or Applications to launch. The Applications menu also shows
Shut Down and Restart when the kernel detects supported Q35 power hardware.
Both require confirmation. Sleep stays disabled with a missing-resume explanation.
See [power coordination, VM configurations and evidence](power.md).
Esc toggles the menu, Up/Down move the visible selection, and Enter launches.
Tab switches between open applications and restores minimized ones. F1 minimizes,
F2 restores, F3 closes the focused application. Title controls minimize, maximize
and close; dragging the title moves a window. The bottom task strip restores and
focuses a running application. Focus has a blue title and the menu selection has
a blue background. Enter/Space or a click changes the Settings accent.

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

Limits: software rendering, bounded 8 MiB arena, five built-in views, fixed-size
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
