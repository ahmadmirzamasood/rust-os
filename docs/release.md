# 0.1.0-experimental.1 review candidate

Experimental Rust OS with its own graphical shell; not production-ready. Packaging does not certify acceptance. Publication requires explicit approval. MIT owner: Masood Ahmad. Preserve dependency notices. Firmware and hypervisors are external and not redistributed.

## Build and validate

Follow [building.md](building.md), [primary VM](power.md), [architecture](architecture/overview.md), [desktop](desktop.md), [Files/Trash](files.md), and [security model](security-model.md).

```text
cargo xtask ci
cargo xtask robustness
cargo xtask smoke
cargo xtask smoke --release
cargo xtask stress --release
cargo xtask storage-smoke --release
cargo xtask shell-smoke --release
cargo xtask release --release
```

Robustness uses seed 0x525553544f53: 100,000 mutations each of ELF, bundle and display packets; 20,000 RSFS mutations against populated metadata and file data (half the metadata cases resealed); 10,000 write/rename/sync/remount/remove cycles. It runs host code. Stress repeats five bounded process/input VM cycles, each with memory and scheduler tests and separate evidence. Neither is a multi-day soak.

Release packaging creates a fresh target/releases/0.1.0-experimental.1 directory and refuses overwrite. It stages the shell raw UEFI image, public source, exact dependency lockfiles, original crate source archives and notices, Rust license material, build identity and SHA256SUMS. Packaging fetches the exact locked workspace and UEFI dependency sources, including optional packages. Missing material or a fetch failure leaves INCOMPLETE.txt. Never publish incomplete output. Hashes establish integrity, not signatures. Working-tree changes are recorded; bit-identical rebuilding is not claimed because image metadata may differ.

The source export excludes Git history, private notes and test output. For a clean-source check, initialize Git in the exported source directory, add public files and run ci and shell-smoke there. No commit or remote is needed for those checks; packaging provenance requires an approved Git revision. Installed toolchain/cached dependencies are allowed; this does not test first-ever network installation on every host.

## Public repository/release checklist

- [x] Candidate source passes CI, fresh-source build and graphical boot.
- [x] Process faults/syscall rejection and independent graphical clients pass.
- [x] Files/Trash bytes survive real restart/shutdown.
- [x] Bounded stress/mutation results recorded for candidate.
- [ ] Exact inventory, sources, copyright notices and font OFL included.
- [ ] Index, source, reachable history and package inspected for private material/secrets.
- [ ] Maintainer configures and tests private vulnerability reporting.
- [ ] Review final diff, select/tag approved commit and regenerate artifacts.
- [ ] Review checksums, provenance, release notes and compatibility matrix.
- [ ] Obtain explicit authorization before pushing or publishing.

Files/Trash/Settings views share one trusted process. Separate diagnostic and graphical demo programs test isolation; normal shell does not launch each view as a process. Limits: 64 entries, 4 KiB/file, no journaling/power-loss guarantee, ACLs, networking, USB input, audio, SMP or suspend. VirtualBox, VMware and physical hardware unverified. See [validation record](release-validation.md) for actual evidence and blockers.
