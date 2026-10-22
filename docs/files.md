# Files and Trash

The shell's Files and Trash views run in the trusted ring-3 shell process.
Computer shows the single mounted `/` volume: RSFS1 persistent data or RAM
explicitly labelled temporary. Enter or click in Computer opens Files. Desktop
icons and the launcher open the same views. Only the mounted guest data volume
is available; the firmware boot partition and host files are not mounted.

## Controls

The fixed-size view shows three entries at a time. Up/Down select and scroll
through all entries, or scroll a text/help viewer. A row click selects; Enter
opens a directory or a UTF-8 text file. Backspace navigates to the parent or
closes a viewer. Click the location row to go up. F5 refreshes; L opens an
absolute location prompt. Esc cancels a prompt or viewer; otherwise it toggles
the launcher. O shows the full selected path (original path in Trash), with
scrolling for long names. H opens the scrollable controls. Names/types and file
byte sizes appear in the listing; unavailable sizes are omitted. Directory
sizes are not calculated.

| Key | Action |
| --- | --- |
| N | New folder: enter a name, then Enter |
| T | Save a new sample text file (`New text file.` plus newline), exclusively |
| R | Rename selection: enter a new basename |
| C / X | Copy / cut selection, including directory trees |
| V | Paste into current directory |
| Delete | Move selection into this volume's Trash |
| U (Trash) | Restore to the recorded original path |
| D | Permanently delete selection and descendants; type `delete` and Enter |

Name entry currently supports lowercase US letters, digits, spaces, dash,
equals, dot and slash (slash is only valid in the location prompt). Empty names,
dot/dot-dot basenames, slashes, backslashes and control characters are rejected.
Existing UTF-8 paths are supported by storage, but the bitmap display replaces
non-ASCII characters with `?`. O shows the entire path in wrapped lines.
Text is read-only: there is no truncate/replace contract suitable for reliable
editing. Non-UTF-8 or binary control data reports an error instead of editing.

## Operations and errors

Create, rename, copy, move and restore never overwrite an existing destination.
A restore collision leaves both versions intact in their respective locations;
rename or move the conflicting original with Files, then retry U. A missing
original parent must be recreated first. Copy recursively creates a new tree;
move uses the volume's existing subtree rename. Moving a tree inside itself
is rejected. Source data is retained if a copy fails. A failed multi-file copy
can leave a partial destination, clearly reported for explicit review/removal.
Failed operations stop their remaining steps; they never silently switch from
Trash to permanent deletion. Errors include AlreadyExists, InvalidArgument,
PermissionDenied, ReadOnly, NoSpace, TooLarge, NotFound, WouldBlock and Io.
Permission failures are propagated; the OS still has no UIDs or ACLs.

Successful operations finish only after explicit sync. Until `Complete`, keep
the VM running. RAM completion is temporary. Copy/delete jobs execute in bounded
steps, and Files sync polls additive syscall 25 (`FS_SYNC_STEP`): each call does
at most one sector transfer or a device flush. Reads remain available, while
mutations return WouldBlock during shared writeback. A later caller can resume
an interrupted writeback; synchronous FS_SYNC also completes it. The shell
handles input and composes frames between batches (at most 64 steps or 4 ms
of cooperative work, plus any single in-flight device timeout) and avoids repainting
unchanged progress text. Closing a window does not cancel its operation.
A failing device can still delay one call by its existing bounded PIO timeout;
this is cooperative I/O, not an asynchronous driver or a latency guarantee.

## Trash format and interruption behavior

`/.trash` is reserved in Files (hidden at root and rejected by its location
field). Each item has a collision-free `t00`..`t63` directory containing
`origin` (UTF-8 `RST1\n` followed by canonical original absolute path) and
`data` (the file or directory tree). Trash displays original paths and payload
types/sizes, rather than treating the container as the user's file.
Metadata is synced before moving the payload. Restore renames without overwrite,
syncs the restored path, then removes its metadata/container and syncs again.

A process interruption between completed syncs can leave a pending container
with no payload, either before trashing or after restoration. Trash labels it
PENDING; O shows its original path. Check that original in Files before using
confirmed D to remove the pending record. Invalid records are labelled INVALID;
restoration reports an error. Cleaning a pending record never deletes a file
at its original path. Out-of-space failure during Trash preparation preserves
the source and may leave a record requiring cleanup. Trash uses up to three
additional filesystem entries (shared root, container, metadata).

Permanent deletion is a distinct action and always requires typed confirmation,
including in Trash. It deletes only the selected item, not the whole Trash.
Deletion is not secure erasure. An interrupted confirmed recursive deletion may
leave part of the tree, which can be selected and deleted again.

Currently only one volume is exposed. RAM and RSFS1 support the directory-based
Trash; RAM Trash lasts only for this boot. If creating Trash fails (unsupported,
read-only, permission denied or full), report the error and keep the original;
users may explicitly choose confirmed permanent deletion. Cross-volume copy,
move and restore are unavailable, since there is no second mount or mount
routing API. No cross-volume move is emulated by deleting a source.

## Resource and durability limits

The storage contract remains 64 non-root entries, 4096 bytes per file and
120 bytes per absolute path. Directories and Trash records consume entries.
An 8 KiB listing buffer holds the maximum directory; all entries are selectable
through three-row pages. Tree planning is bounded to 64 nodes and checks path
expansion before copying/trashing. The shell retains its bounded 12 MiB arena
and original fixed surfaces. These limits are intentionally small; this is not
a general-purpose file manager.

RSFS1 still has no journal or power-loss/crash consistency guarantee. Metadata
ordering helps between completed syncs; it cannot make an interrupted on-disk
writeback atomic. A sync failure is visible, poisons the mount read-only, and
can leave an unmountable disk. Preserve the disposable image for investigation;
there is no automatic format, rollback or recovery. See [FILESYSTEM.md](FILESYSTEM.md).

## Verification

`cargo xtask ci` covers host policy/failure tests and guest lint/build checks.
`cargo xtask shell-smoke [--release]` drives actual PS/2 shell controls to create
a folder and text file, rename, copy, Trash, reboot, restore, Trash again and
confirm permanent deletion. It independently remounts the disposable image
after both boots and verifies exact bytes, original-path metadata and removal.
It retains launcher/window/pointer and saved-accent regression checks and stores
screenshots and serial logs under `target/shell-test/`.
`cargo xtask storage-smoke [--release]` retains storage and corrupt-volume tests;
`cargo xtask smoke` retains the diagnostic/process/display regressions.

Host tests exercise collisions, capacity/permission failures, pending records,
sync errors, recursive copy/move, navigation/viewing/scrolling and confirmation.
They model process interruption; they do not simulate physical power loss.

## Recorded results (2026-10-19)

Windows, nightly-2026-10-08 (rustc 1.101.0-nightly), QEMU 11.1.0
(v11.1.0-12130-ge470268ff4), bundled edk2 firmware, q35, 256 MiB, one CPU:

| Check | Actual result |
| --- | --- |
| `cargo xtask ci` | PASS: 157 host tests, lint, guest builds/ELF validation and privacy checks |
| `cargo xtask shell-smoke` | PASS: both boots, exact-byte independent remount and focused Trash screenshot |
| `cargo xtask shell-smoke --release` | PASS: both boots and the same checks |
| `cargo xtask storage-smoke` and `--release` | PASS: all three boots in each profile, including unchanged corrupt-volume rejection |
| `cargo xtask smoke` and `--release` | PASS: all nine existing scenarios in each profile |

Final Files evidence directories are
`target/shell-test/debug-1792427120519268500/` and
`target/shell-test/release-1792427247877268800/`, including boot logs and
`1-files.png` / `2-files.png` screenshots. This is local QEMU evidence;
no hosted CI, other hypervisor or physical-hardware result is claimed.

| Acceptance gate | Status and scope |
| --- | --- |
| Folder, save/copy, rename, Trash, restore, permanent deletion | PASS: actual GUI input in both profiles; host tests also cover cut/move and trees |
| Correct changes after reboot | PASS: synced Trash original path/payload survive a fresh VM boot, then restore/delete and independent remount checks |
| Useful failures without silent overwrite/removal | PASS for tested collision, invalid-name, capacity, permission, missing-parent, corruption and I/O cases; partial work is reported/retained |
| Large directories usable within limits | PASS at the 64-entry limit in host policy tests, including selection through the final page; no larger-volume or VM stress claim |

These gates apply to completed syncs and the bounded storage contract above.
Interrupted device writeback and physical power loss still have no recovery or
atomicity guarantee; this phase does not extend RSFS1's durability contract.
