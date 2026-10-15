# Security status

This experimental OS is not production-ready; it boots to a status screen in
QEMU and runs its own test programs. Kernel mappings follow W^X and use
no-execute and guard pages. User programs run in ring 3 in separate address
spaces; the kernel validates every system-call argument and user pointer and
terminates a faulting process without affecting others (see
[docs/architecture/processes.md](docs/architecture/processes.md)). This has
been tested only with the project's own programs in QEMU. There are no
per-process resource quotas beyond fixed table sizes, no security review
and no fuzzing of the kernel interface, so it must not be described as safe
for untrusted programs. The desktop model still runs as trusted kernel
code. Use disposable VM disk images only.

Contributions must validate external data (boot information, disk metadata,
input bytes, executables, every system-call argument), check ranges and
arithmetic, bound device waits, document unsafe invariants and test
meaningful failure paths. The kernel never dereferences a user pointer; use
`memory::user` for every access to user memory. Never point any tool at a host
physical disk.

A private vulnerability-reporting channel has not been configured yet. Until
the maintainer publishes one here, please do not post exploit details
publicly; open an issue asking for a private contact instead.
