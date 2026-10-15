# Security status

This experimental OS is not production-ready; it boots to a status screen in
QEMU and does nothing else yet. Kernel mappings follow W^X and use no-execute
and guard pages, but these are defences against kernel bugs, not isolation:
there is no user mode yet. The
initial design runs the desktop and applications as trusted code sharing
kernel privileges; it must not be described as isolating untrusted programs.
Use disposable VM disk images only.

Contributions must validate external data (boot information, disk metadata,
input bytes), check ranges and arithmetic, bound device waits, document unsafe
invariants and test meaningful failure paths. Never point any tool at a host
physical disk.

A private vulnerability-reporting channel has not been configured yet. Until
the maintainer publishes one here, please do not post exploit details
publicly; open an issue asking for a private contact instead.
