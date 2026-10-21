# Experimental security model

Supported environment: single-CPU x86_64 QEMU q35, UEFI, PS/2 and a designated disposable RSFS1 data disk. Firmware, hypervisor, build host and boot bundle are trusted. No secure boot, networking or arbitrary hostile-program support.

| Boundary | Control | Limit |
| --- | --- | --- |
| User/kernel memory | Separate page tables, supervisor kernel pages, W^X/NX, guarded stacks, IOPL zero | One CPU; no FPU state support |
| Syscalls | Checked ranges, full caller page/permission walk before physical-window copy | Single-threaded processes, no runtime unmapping |
| ELF loader | Bounded static segments, checked offsets, no overlap or W+X, zeroed frames | No dynamic linker or runtime installation |
| IPC | Owned generation handles, bounded queues, endpoint transfer | Global tables; no per-app quotas |
| Graphics | Boot-root device role, copied scanlines, channel-owned windows, bounded buffers | Built-in views share shell; input may drop under backpressure |
| Filesystem | Canonical bounded paths, checksums, complete mount validation, mode-checked owned handles | Global namespace; no ACLs, authentication or crash recovery |
| Power | Root shell only, live-peer veto, final sync with preemption inhibited | QEMU-specific; no general ACPI or suspend |

Unsafe invariants rely on one CPU, interrupt/preemption exclusion where documented, fresh frame ownership and no remaining references when freeing stacks. Existing process roots share the stable kernel upper-half PML4 entry.

Review found monotonic kernel-stack reservations retaining VA/page tables under sequential process churn. A reusable 64-slot pool now bounds retention, with up to eight mapped pages plus a guard per slot, exceeding the 32-thread scheduler capacity. Rollback and teardown return reservations only after leaf unmapping. Retained tables remain shared. Boot regression checks 1,024 reuse cycles and frame accounting. This does not provide quotas.

Review covered syscall/user-copy, loading, IPC, graphics buffers/ownership, RSFS parsing, CPU entry and scheduler integration. It was not a complete dependency or hardware audit. Deterministic mutation testing is not coverage-guided fuzzing, formal verification or a security guarantee. See [SECURITY.md](../SECURITY.md) for reporting.
