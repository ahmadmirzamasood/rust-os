# Rust desktop environment

Launch with `cargo xtask run --release` or `cargo xtask desktop --release`.
This is the OS's own `no_std` Rust desktop in ring 3, using its software
compositor, validated display/input syscalls and filesystem. Firmware and QEMU
are external. Settings uses a local `no_std` subset of Iced's layout and widget
code with the guest software renderer; see [the port and its limits](iced-port.md).

## Desktop and dock

The desktop includes Computer, Files, Trash, Settings, About and Task Manager.
The centered dock pins all six, including applications that are not open.
Click to launch or focus; click the focused application's icon to minimize it.
Running applications have an indicator and the active application is highlighted.
The dock is global: activating an application on another workspace returns to
that workspace and focuses it. Each built-in application still has one window.
Hover feedback, a static wallpaper, rounded dock, window shadows, title controls
and a panel form the desktop chrome. The launcher keeps its short motion
transitions. F4 reduces motion for this boot. Accent preferences still use the
existing versioned file.

## Applications search

Open Applications with the panel button, Escape or either Super key. Type US
letters, digits, spaces or minus to filter the built-in catalog. The query is
bounded to 32 ASCII bytes; excess input is ignored. Search ignores case and
matches every space-separated token against application names, descriptions
and aliases, so `cpu monitor` finds Task Manager and `accent` finds Settings.
Blank search keeps the original catalog order, including the power actions.
Power actions match only their labels when a query is present and retain the
existing capability checks and confirmation.

Up/Down move through the visible results; Enter opens the selected result.
Pointer hover and clicking use the same filtered rows. Backspace removes one
character. Escape clears a nonempty query first, then closes the menu on the
next press. A query with no results displays an empty-state message; Enter
does nothing. Search consumes keyboard input while the menu is open.

## Window manager

| Control | Action |
| --- | --- |
| Title drag | Move, keeping the title below the panel |
| Bottom-right grip drag | Resize the frame, bounded to 256–1024 by 160–512 client pixels |
| Title buttons | Minimize, toggle maximize/restore, close |
| Tab | Cycle applications on the current workspace, including minimized ones |
| F1 / F2 / F3 | Minimize focused / restore current workspace applications / close focused |
| F5 | Open or focus Task Manager |
| F6 | Toggle maximize within the work area |
| F7 / F8 | Tile left / right |
| F9 | Restore a floating cascade and default frame sizes on the current workspace |
| F10 / F11 | Previous / next workspace, wrapping through all four |
| F12 | Move the focused application to the next workspace and follow it |

Maximizing and tiling leave the panel and dock accessible. Restore retains the
original floating rectangle across repeated tiling. Application backing surfaces
remain 480×236 pixels: resizing changes the frame, fills added space with the
application background and clips content when smaller. It does not provide
responsive application layout, additional Files rows or pixel scaling.

## Workspaces

The four numbered controls in the center of the panel switch workspaces with a
click. The active workspace is highlighted and small indicators show its live
window count, including minimized windows. Each workspace remembers its last
focused visible window; switching preserves window geometry and minimized
state. Windows on other workspaces are excluded from composition and input
targets. Switching cancels pointer capture, so a drag cannot cross workspaces.

New applications open on the current workspace. Launching an existing
application from Applications, the dock or Task Manager returns to its existing
workspace. F12 moves a focused application without recreating its surface or
resetting its contents. With no focused window it still advances to the next
workspace. Workspace placement and focus last for this boot; they are not
saved to disk. The existing eight-window limit is shared across workspaces.

## Task Manager

The Apps view lists all six desktop views, their workspace location, and their
focused, running, minimized or not-running state. Click a row or use Up/Down,
then use the Focus, Minimize or Close controls (Enter, M or Delete). Close
requires confirmation; Escape cancels it. Confirmations retain a window ID so a stale confirmation cannot
close a replacement window. Closing Files does not cancel an existing file job.

Click the view header or press P to switch to Processes. It shows real kernel
PIDs, program names, cumulative scheduled CPU milliseconds and handle counts,
with Up/Down scrolling. The header reports process count, thread count and free
physical-frame memory. Data refreshes once per second; unavailable data is
explicitly labelled. CPU time is cumulative, not a utilization percentage;
exited processes may have no remaining scheduler statistics.

Desktop views currently share one trusted shell process. App close ends a view;
it does not kill that process. The process view is read-only. Arbitrary process
termination, per-process resident-memory accounting and independent built-in
application processes are not implemented.

## Boundary and memory

`SYSTEM_SNAPSHOT` (27) is additive to ABI 1 and available only to the root display
role in a shell kernel. It accepts `(output, 800, 0, 0)`. Other programs receive
`UnknownSyscall`. Reserved arguments and wrong lengths fail with
`InvalidArgument`; all output pages must be writable or the call fails with
`BadAddress`. The kernel snapshots bounded tables, releases locks, then copies
bytes through `memory::user`. No raw addresses, mappings, process memory or
termination rights are exposed.

The 800-byte v1 encoding uses a 32-byte header (u32 version/count, u64 uptime/free
bytes, u32 thread count, four zero bytes) and sixteen 48-byte records (u32 PID,
u8 state/name length, u16 handles, u64 CPU ns, 32 zero-padded name bytes).
Unused records are zero. The shared decoder rejects unknown versions, malformed
names, invalid states, oversized counts, reserved data and incorrect lengths.

The display/shell allocator now has a bounded 12 MiB arena, fitting two screen
buffers and all six normal-size window surfaces at the tested 1280×800 mode.
The ELF image remains subject to the existing 16 MiB load limit. Larger modes
can still exhaust memory. Cursor-only presentation and frame pacing from
[desktop.md](desktop.md) are retained; full composition while dragging and
software framebuffer transfers remain performance limits.

## Checks

Run `cargo xtask ci`, `cargo xtask smoke`, and `cargo xtask shell-smoke` in both
debug and release. Host tests cover dock hit targets, app lifecycle, confirmation
cancellation/staleness, work-area geometry, restore and malformed snapshot bytes.
The abuse process checks monitoring denial for ordinary programs. Shell smoke
drives dock launch, Task Manager app/process views, close/cancel/relaunch,
maximize/restore, left/right tiling and real mouse resizing with screenshots,
alongside Files, persistence and hardware power regressions.

Host tests also cover bounded search editing, empty results, filtered catalog
IDs, workspace input/pixel isolation, remembered focus, minimized state and
returning to a remote application from the dock. QMP shell checks search and
launch About, clear and dismiss the menu, move/follow a window, compare retained
workspace pixels, minimize/restore it from the dock, and switch workspaces with
keys and pointer, including wraparound.

Key/button batches emit `SHELL INPUT PRESENTED` after changed scanlines reach
the device and launcher motion settles. These checks wait for that marker
before taking reference screenshots, including a harmless Shift press after
positioning the cursor. The marker records only workspace, menu state and query
length; it does not record search text or establish vsync/atomic presentation.

QEMU, VirtualBox, VMware and hardware compatibility require separate actual
results; command availability alone does not establish compatibility.

## Further development

This milestone adds desktop navigation within the current trusted shell. The
following larger capabilities remain development work:

- Resize application surfaces and provide responsive layouts, rather than
  enlarging only their frames; extend the client protocol with resize events.
- Run built-in applications as separate processes with defined lifecycle,
  resource limits, IPC backpressure handling and display-service recovery.
- Add a bounded notification service and session coordination for application
  startup, exit requests and unsaved documents.
- Version session preferences to persist workspace placement, reduced motion
  and accessibility choices; add keyboard layout, text scaling and focus
  accessibility before presenting those settings as supported.
- Develop a display driver and GPU composition path, frame synchronization,
  multiple-display support and a wider device/input stack with measured VM and
  hardware validation.

Networking, audio and real suspend/resume also need OS services and drivers.
They are not supplied by this desktop and remain unavailable.

## Workspace and search validation — 2026-10-22

Windows, pinned nightly-2026-10-08, rustc 1.101.0-nightly
(`1d81eb4ad`), QEMU 11.1.0 (`v11.1.0-12130-ge470268ff4`), bundled edk2
firmware with the SHA-256 identifiers below, q35, 256 MiB, one CPU, standard
VGA at 1280x800, PS/2 keyboard and mouse. Storage checks use fresh designated
disposable images; no host physical disk is used.

| Command | Actual result |
| --- | --- |
| `cargo xtask ci` | PASS: 179 host tests, formatting, Clippy, guest builds, ELF validation and privacy checks |
| `cargo xtask shell-smoke` | PASS: search/workspaces, desktop controls, Files/Trash, both persistence boots, guest restart and clean shutdown |
| `cargo xtask shell-smoke --release` | PASS: the same guest checks in release |
| `cargo xtask smoke` | PASS: all nine boot, fault, input, process and independent-display-client scenarios |
| `cargo xtask smoke --release` | PASS: all nine scenarios in release |

Command output is retained in `target/desktop-ci.log`,
`target/desktop-shell-debug.log`, `target/desktop-shell-release.log`,
`target/desktop-smoke.log` and `target/desktop-smoke-release.log`.
Desktop serial logs and screenshots are under
`target/shell-test/debug-1792617856640039500/` and
`target/shell-test/release-1792618065414158200/`. Diagnostic evidence is under
`target/smoke/debug/` and `target/smoke/release/`.

Both shell traces report:

```text
desktop workflow PASS: Meta launcher, live search and Escape, workspace move/follow, hidden scene preservation, dock restore, mouse switching and wraparound
shell boot 1: PASS (independent remount, preference bytes; Files checks=true)
shell boot 2: PASS (independent remount, preference bytes; Files checks=true)
```

Two earlier debug harness attempts captured an unfinished presentation or the
old cursor in their reference image. Their traces remain under
`target/shell-test/debug-1792617528239969800/` and
`target/shell-test/debug-1792617745045316100/`; the completed-frame checks above
supersede those attempts. No hosted CI, other hypervisor, physical hardware,
other display mode, crash-recovery or sustained frame-rate result is claimed.

## Earlier local validation (before workspaces and search)

Windows host, pinned nightly-2026-10-08 (rustc 1.101.0-nightly), QEMU 11.1.0
(v11.1.0-12130-ge470268ff4), q35, 256 MiB, one CPU, 1280x800:

| Command | Actual result |
| --- | --- |
| `cargo xtask ci` | PASS: lint, host tests, guest builds, ELF checks and privacy checks |
| `cargo xtask shell-smoke` | PASS: desktop controls, Files, both boots, persistence and power |
| `cargo xtask shell-smoke --release` | PASS: the same checks in release |
| `cargo xtask smoke` | PASS: all nine diagnostic/process/display scenarios |

Bundled firmware SHA-256 identifiers:

- `edk2-x86_64-code.fd`: `33090cc07675baa5190d9f1e84bf5176b33bcbfa9bacac522961150cdb6dbb2a`.
- `edk2-i386-vars.fd`: `5d2ac383371b408398accee7ec27c8c09ea5b74a0de0ceea6513388b15be5d1e`.

The final release serial trace includes:

```text
[user shell/1] SHELL MONITOR VALIDATION PASS
[user shell/1] SHELL MONITOR processes=1 threads=3
[user shell/1] SHELL CASCADE
[user shell/1] SHELL RESIZE 7 560 300
```

Command output is retained in `target/pr-ci.log`, `target/pr-shell-debug.log`,
`target/pr-shell-release.log` and `target/pr-smoke.log`. Guest logs/screenshots
are under `target/shell-test/debug-1792616961388864200/` and
`target/shell-test/release-1792617189777112200/`; diagnostic traces are under
`target/smoke/debug/`. These are local QEMU results; no hosted CI, other
hypervisor, physical hardware or sustained 60 FPS result is claimed.
