# Experimental release validation — 2026-10-21

Candidate: 0.1.0-experimental.1, based on 4f8593e plus the source snapshot in
its review bundle before the PR commit. Images remain local. No release was
published and no history was rewritten. This record describes bounded experimental
support, not production readiness or an exhaustive security certification.

## Host and primary VM

Windows, x86_64-pc-windows-gnullvm; nightly-2026-10-08, rustc
1.101.0-nightly (1d81eb4ad 2026-10-07), LLVM23.1.3.
QEMU11.1.0 (v11.1.0-12130-ge470268ff4), q35, 256MiB, one CPU, bundled edk2
UEFI, standard VGA1280x800, PS/2 keyboard/mouse. Boot disk raw with snapshot
protection; storage tests use a fresh designated PIIX3 IDE disposable data disk.
Exact configuration and controls: [power.md](power.md), [building.md](building.md).

Firmware SHA-256 (rechecked for this validation):

- edk2-x86_64-code.fd: 33090cc07675baa5190d9f1e84bf5176b33bcbfa9bacac522961150cdb6dbb2a
- edk2-i386-vars.fd: 5d2ac383371b408398accee7ec27c8c09ea5b74a0de0ceea6513388b15be5d1e

## Evidence and acceptance

| Check | Actual result | Scope |
| --- | --- | --- |
| cargo xtask ci | PASS,160 host tests, formatting, Clippy, kernel and user builds, ELF and privacy checks | Windows; new CI step configured, hosted execution of these changes not run |
| cargo xtask robustness | PASS,300000 ELF/bundle/display decode mutations,20000 populated RSFS mutations,10000 write/rename/sync/remount/remove cycles | Fixed seed0x525553544f53; host harness, no coverage-guided engine or sanitizer run |
| cargo xtask smoke | PASS9/9 | Boot pixels, panic/no-display/fault diagnostics, input, processes, independent graphical clients |
| cargo xtask smoke --release | PASS9/9 | Same scenarios in release profile |
| cargo xtask stress --release | PASS5 process/input cycles,10 VM launches | Per process run:17 processes,9 contained faults,39 init checks; each boot also tests memory/scheduler |
| Memory and scheduling on target | PASS85 memory checks,17 input checks,9 scheduler checks per diagnostic boot | Includes1024 stack reuse cycles,128 partial OOM rollback cycles, heap stress, exhaustion, W^X/guard faults |
| cargo xtask storage-smoke --release | PASS3/3 boots | Exact persisted bytes after clean exit, corruption rejected without mutation |
| cargo xtask shell-smoke --release | PASS2 cycles | Files/Trash operations, cancellation, actual guest restart and S5 exit, exact-byte independent remount |
| Fresh public-source export | CI PASS; release shell2 cycles PASS; updated process scenario PASS | Initially empty build output, installed toolchain/cached crates; UEFI dependency fetched when sandbox registry access failed |
| License owner and notices | MIT owner Masood Ahmad preserved; exact lockfile inventory staged by release command | Full crate sources, ucs2 MPL source, font OFL, Rust copyright/licenses; firmware not bundled |
| Privacy/history inspection | No declared private path in index/current export;19 reachable commits checked, no common credential signatures found | Path/signature and source review, not proof of universal secret absence; no history rewrite |

The raw shell image, source snapshot, lockfile-derived source archives/notices,
BUILD.txt and SHA256SUMS are staged by `cargo xtask release --release`.
Successful output has SHA256SUMS and no INCOMPLETE.txt; checksum verification
and review of the bundle are required before publication. Hashes are not
signatures. No bit-identical image rebuild claim is made.

Two defects were fixed: sequential process churn retaining kernel-stack VA and
page tables, and inconsistent private-path export filtering. Release provenance
queries now use the packaged workspace directory. The process regression test
also coordinates READY/GO/START/STOP instead of assuming that slow child loading
fits inside a300ms wall-clock interval; measured workers never yield or block,
and their ten-second hard deadline preserves bounded failure.

Earlier failed attempts are retained locally: debug preemption overlap failed
under simultaneous fresh compilation before coordination was corrected; the
fresh-image attempt initially failed on sandbox registry access and succeeded
with access to the pinned bootloader; packaging initially rejected an omitted
public rustfmt configuration and missing optional crate cache entries. Final
results above supersede those attempts, without erasing their evidence.

## Limits and remaining publication gates

The primary QEMU gate is met for the bounded bundled-program scope. Independent
diagnostic and two-client graphical programs demonstrate process isolation.
Files/Trash/Settings built-in views share one trusted shell process; there is
no per-view isolation, ACL or hostile-program guarantee. Filesystem limits are
64 entries and4KiB per file, with no journal or power-loss recovery.

VirtualBox, VMware, physical hardware, USB, networking, audio, SMP, suspend,
third-party application installation and other firmware remain unsupported or
untested. Tests are bounded cycles, not a multi-day memory/device soak.
Security review is partial; all dependency unsafe code and every hardware CPU
state have not been exhaustively audited.

Before a public release, the maintainer must review the final diff/source
snapshot, configure and test a private vulnerability-reporting channel, choose
an approved immutable commit/tag, regenerate/review artifacts and checksums,
and explicitly authorize publication. See [release checklist](release.md).
