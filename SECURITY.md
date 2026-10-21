# Security status

This experimental OS is not production-ready; it boots to a status screen in
QEMU and runs its own test programs. Kernel mappings follow W^X and use
no-execute and guard pages. User programs run in ring 3 in separate address
spaces; the kernel validates every system-call argument and user pointer and
terminates a faulting process without affecting others (see
[docs/architecture/processes.md](docs/architecture/processes.md)). This has
been tested only with the project's own programs in QEMU. There are no
per-process resource quotas beyond fixed table sizes or coverage-guided
fuzzing of the kernel interface, so it must not be described as safe
for untrusted programs. The shell and built-in views share a trusted ring-3 process. Use disposable VM disk images only.

Contributions must validate external data (boot information, disk metadata,
input bytes, executables, every system-call argument), check ranges and
arithmetic, bound device waits, document unsafe invariants and test
meaningful failure paths. The kernel never dereferences a user pointer; use
`memory::user` for every access to user memory. Never point any tool at a host
physical disk.

Filesystem access is shared among all bundled applications: no UIDs, ACLs
or application-private directories. File handles enforce ownership and
read/write mode, and syscalls validate user buffers. The disk driver accepts
only the designated disposable QEMU disk serial and geometry; malformed
volumes are rejected without formatting. RSFS1 has checksums but no crash
recovery or power-loss guarantee. See [docs/FILESYSTEM.md](docs/FILESYSTEM.md).

A private vulnerability-reporting channel has not been configured yet. Until
the maintainer publishes one here, please do not post exploit details
publicly; open an issue asking for a private contact instead.

The graphical kernel variants authorize only their root bundled display/shell
process to copy RGB scanlines and dequeue input. Window policy, surface buffers
and composition run in ring 3. Channels establish client identities; no client
shared-memory mappings exist. This remains a bounded prototype for bundled
programs, with possible lost client input under channel backpressure and no
service restart. See [review scope and threat model](docs/security-model.md) and
[display boundary](docs/architecture/display.md).

The graphical shell uses the same bounded display boot role as the display demo.
Built-in views share a process and the global filesystem namespace. Shell
preferences are not confidential or protected from other bundled programs.
