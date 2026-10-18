# Storage and filesystem interface

The guest has a bounded `no_std` VFS (`crates/storage`) with a temporary RAM
filesystem and **RSFS1**, a project-owned experimental disk format. The code
is MIT-licensed and adds no third-party guest dependencies. RSFS1 was chosen
for a small, auditable first slice; it is not FAT, ext, POSIX or an
interchange format. The UEFI boot image's FAT partition is not mounted by
this driver. There is no file manager or Trash implementation yet.

## Supported devices and volumes

The driver supports QEMU's PIIX3 IDE controller (PCI 8086:7010), bus 0,
compatibility mode, primary master, 512-byte sectors, LBA28, polling PIO.
It verifies IDENTIFY's LBA and FLUSH CACHE support, exact volume geometry,
and serial `RUSTOS-DISPOSABLE`. IRQs and DMA are disabled. Polling has both
a one-second deadline and a two-million-poll ceiling per wait. No AHCI,
NVMe, virtio, ATAPI, partition discovery, hotplug or real hardware support
is claimed. The boot disk remains on q35's separate AHCI controller.

With no PIIX3 controller, `/` is an empty, temporary RAM filesystem. With a
designated data disk, `/` is RSFS1. A failed probe or invalid volume reports
an error; it does not silently substitute RAM or format the disk. The whole
filesystem is loaded into bounded kernel memory before use. Maximums are
64 non-root entries, 4,096 bytes per file, and 120 bytes per absolute path.
Directories also consume entries. Each file reserves its own 4 KiB extent;
unused capacity cannot be borrowed by another file. This is intentionally
too limited for general desktop storage.

## VFS, paths and handles

`BlockDevice` exclusively owns a device and exposes sector count, exact
512-byte reads/writes, and flush. `Vfs` exposes lookup, create, positional
read/write, list, rename, remove and sync. RAM and disk implement the same
interface. Disk mutations are buffered until sync.

Paths are absolute UTF-8, case-sensitive, with no Unicode normalization.
Repeated `/`, `.` and trailing `/` collapse lexically. `..` removes one
component; attempting to escape `/` fails. Control characters, NUL and
backslash are forbidden. Limits apply to the input and normalized path.
There are no relative paths, current directories, symlinks or hard links.
Lexical normalization precedes lookup (including `file/..`). A trailing
slash does not impose a directory requirement; callers request directory
operations explicitly. The root directory is implicit and cannot be removed
or renamed. Every other parent must exist and be a directory.

`FILE_OPEN` returns a handle in the caller's existing process-local handle
table. Read-only and read/write modes are explicit; exclusive create
requires write mode. File handles are not inherited or transferable, and
close on process exit or fault. `ulib::fs::File` owns its handle and closes
it on Drop. Reads and writes have explicit byte offsets, no shared cursor.
Renaming preserves open node IDs. Deleting a file invalidates its open IDs:
later access returns `NotFound`; closing still succeeds. Recreating the
same path does not revive those IDs. IDs never repeat within a mount.
Closed numeric handles fail with `BadHandle` using the existing generation
table (its pre-existing 24-bit wrap limitation remains).

Listing returns all immediate names, separated by newline; directories have
a trailing slash. Order is unspecified. A buffer too small for the complete
listing fails with `TooLarge` and is untouched. The syscall limit is 8 KiB,
which holds a maximum-size listing. File transfers and syscall offsets are
at most 4 KiB; larger offsets return `TooLarge`.
Reading at/beyond EOF returns zero. A write can extend a file and zero-fills
the gap. There is no truncate operation yet; shorter writes preserve the
tail. Writes are all-or-error in the in-memory state. Empty writes do not
extend files. Rename does not overwrite a destination, moves directory
descendants, and rejects cycles. Removal requires an empty directory.

Errors distinguish `NotFound`, `AlreadyExists`, `NotDirectory`,
`IsDirectory`, `NotEmpty`, `NoSpace`, `TooLarge`, `Io`, `Corrupt`, `ReadOnly`,
`PermissionDenied`, invalid paths/flags (`InvalidArgument`), invalid user
memory (`BadAddress`), invalid/wrong-type handles, and allocation exhaustion
(`NoResources`). Bounds and additions are checked before mutation; table or
file exhaustion leaves existing contents unchanged. All user buffers are
validated against the calling address space before use and copied through
the existing kernel physical window, never dereferenced as user pointers.

## Initial access model

Only OS-bundled programs run. All such applications share the single volume
and may open any path for reading or writing, create directories, rename,
delete, and sync. There are no UIDs, ACLs, per-application directories, quotas
or confidentiality between applications. Read-only handles restrict that
handle, not the application's ability to open a writable handle. Neither
paths nor syscalls expose raw block devices. This is not a sandbox for
untrusted applications. Filesystem access is serialized by a kernel lock
that disables preemption; IRQ handlers never access storage. Long PIO or
full-volume sync operations can delay other processes.

## RSFS1 disk format

Exactly 577 sectors (295,424 bytes), no partition table. Integers are little
endian. All unspecified metadata bytes must be zero. No packed structs or
unchecked casts are used to decode disk bytes.

| Location | Contents |
| --- | --- |
| Sector 0, bytes 0..8 | ASCII `RSFS0001` |
| Sector 0, bytes 8..16 | u64 sector count, exactly 577 |
| Sector 0, bytes 16..508 | Zero |
| Sector 0, bytes 508..512 | FNV-1a-32 of bytes 0..508 |
| Sectors 1..65 (exclusive end) | 64 entry records, one sector per record |
| Sectors 65..577 | 64 fixed eight-sector data extents |

An unused entry record is entirely zero. An occupied entry record contains:

| Byte range | Contents |
| --- | --- |
| 0 | Kind: 1=file, 2=directory |
| 1 | Absolute canonical path byte length, 1..120 |
| 2..4 | Zero |
| 4..8 | u32 logical byte length, at most 4096; directories must be zero |
| 8..12 | FNV-1a-32 of the logical data bytes (including empty directories) |
| 12..132 | UTF-8 path, zero-padded |
| 132..508 | Zero |
| 508..512 | FNV-1a-32 of bytes 0..508 |

Entry `i` uses sectors `65 + i*8` through `65 + i*8 + 7`. Extents cannot
overlap or point outside the disk because there are no stored block
pointers. FNV-1a starts at 2166136261; each byte is XORed then multiplied by
16777619 modulo 2^32. It detects accidental corruption, not malicious
tampering. Mount validates the complete header, every occupied entry,
checksums, path uniqueness and canonical spelling, sizes and directory
parents before exposing any file. Non-logical data padding is ignored.
Deleted extents can retain old bytes: deletion is not secure erasure.

## Flush and durability

`FS_SYNC` writes every current file extent and entry record, clears unused
entry records, then issues ATA FLUSH CACHE and checks completion. Successful
return means the device reported its flush complete; actual persistence
depends on QEMU and the host storage stack honoring that request. Close and
Drop do not flush. RAM sync succeeds but offers no persistence.

**No crash consistency, atomic disk rename, journaling, rollback, recovery,
or power-loss durability is promised.** Writes can overwrite prior data
before later metadata writes complete. A failed sync can leave a partially
updated disk. That mount becomes read-only until reboot/remount, while
buffered reads remain available. Checksums may reject a torn image but
cannot recover it and do not detect every possible inconsistency. Do not
retry a failed sync as a recovery mechanism. Keep disposable images only.

## Reproducible checks

```text
cargo xtask ci
cargo xtask smoke
cargo xtask storage-smoke
cargo xtask storage-smoke --release
```

`storage-smoke` accepts no disk path. It exclusively creates a new regular
`target/storage-test/<profile>-<unique-id>/DISPOSABLE-rsfs.img`, formats it
with Rust code, and attaches it to a separate PIIX3 IDE controller. Boot
media uses QEMU snapshot mode, so the source boot image is unchanged.

The first VM boot runs the existing process tests plus user-space file
tests, creates persistent files and a directory, syncs, and powers off.
The second boot remounts the same data image and verifies bytes and listing,
then repeats the file tests and powers off. The host independently remounts
the disk after each successful exit. A third boot uses the same disposable
image after intentional header corruption: mount must reject it, and the
host checks that every image byte stayed unchanged. Serial logs and QEMU
stderr remain alongside that explicitly disposable image. Host tests also
cover every header/record byte corruption, oversized metadata with a valid
checksum, duplicate paths, missing parents, capacity exhaustion and I/O
error poisoning.

For this test only, `FS_TEST_SHUTDOWN` is restricted to init and a mounted
designated disk. It holds off preemption through sync and writes Q35's S5
control at port 0x604. This is a machine-specific test power-off path, not
general ACPI discovery, restart support, or a desktop power menu. Other
machines and firmware are unverified. Normal `run`/`smoke` use RAM storage
and retain the existing interactive desktop behavior.

## Recorded verification

On 2026-10-18, Windows host, pinned `nightly-2026-10-08` (rustc
1.101.0-nightly), QEMU 11.1.0 (`v11.1.0-12130-ge470268ff4`) and bundled
edk2 firmware:

- `cargo xtask ci`: passed, 136 host tests including nine storage tests.
- `cargo xtask storage-smoke`: all three boots passed (debug).
- `cargo xtask storage-smoke --release`: all three boots passed.
- `cargo xtask smoke` and `cargo xtask smoke --release`: all eight existing
  scenarios passed in each profile, including input and process isolation.
- Both storage profiles verified clean guest power-off, persistence after
  reboot, ring-3 file operations, capacity/validation failures and unchanged
  corrupt media. No real-host power-loss test was performed.

The firmware code file was `edk2-x86_64-code.fd`, SHA-256
`33090cc07675baa5190d9f1e84bf5176b33bcbfa9bacac522961150cdb6dbb2a`;
variable template `edk2-i386-vars.fd`, SHA-256
`5d2ac383371b408398accee7ec27c8c09ea5b74a0de0ceea6513388b15be5d1e`.
GitHub CI has been configured to run storage smoke; this change has not
been published or run there. Linux, macOS, VirtualBox and VMware remain
unverified for this storage implementation.
