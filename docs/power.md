# Power and virtual machines

The root graphical shell offers Shut Down and Restart on the supported QEMU
Q35 platform. Select either in Applications, then press Enter to confirm or
Esc to cancel. Sleep stays disabled: RAM retention alone cannot restore CPU
context, page tables, device state, timers, interrupt routing and PS/2 input.
There is no suspend entry or resume trampoline, device suspend protocol, wake
source configuration, or repeated resume evidence. CPU halt, scheduler sleep,
and a paused emulator are not OS suspend.

## Coordination and failure policy

Built-in applications share the trusted shell process; no editable documents
exist yet. Open file dialogs/viewers and failed preference saves veto power
requests. Save preferences successfully and close dialogs first. Confirmation
warns that RAM volume contents will be lost. File jobs receive up to five
seconds to drain, while new user work is refused. Timeout cancels power without
force-killing applications. A hung shell cannot reach the power syscall; the
kernel does not automatically force power off. Independent processes do not yet
implement a cooperative exit protocol: if any remain alive, the kernel refuses
the transition. Adding independent editable applications requires that protocol
before extending this policy.
Deadlines are checked at cooperative boundaries; rendering and an in-flight
device operation can delay the visible response. There is no hard UI latency
guarantee.

Syscall 26 (`POWER`, ABI 1 additive) takes action 0 to query capability bits
(shutdown=1, restart=2), 1 to shut down, 2 to restart; reserved arguments must
be zero. Only the kernel-assigned root shell role is authorized. Sleep has no
action. The kernel inhibits scheduling from the other-process check through
final writeback and the hardware request. No application can spawn or mutate
storage in that interval. The volume cache is the only service with queued
writes; IRQ handlers only enqueue input and update timer/scheduler state.

Final stepped writeback has a 30-second deadline checked between bounded device
requests (each may take up to one second plus a bounded poll loop). Timeout or
I/O failure returns to the shell and never issues a hardware transition.
On timeout, incomplete writeback remains frozen until a subsequent sync
completes; retry power or sync before mutations. I/O failures can poison the
volume read-only. No forced power button is offered. RSFS1 has no journal or
power-loss recovery guarantee; a clean flush is different from crash safety.

## Platform mechanism

The backend checks the [QEMU fw_cfg signature](https://www.qemu.org/docs/master/specs/fw_cfg.html),
Q35 host bridge `8086:29c0`, ICH9 LPC `8086:2918`, firmware
ACPI decode enable and the decoded PCI PMBASE. It requests QEMU's S5 through
PM1a control at PMBASE+4 (SLP_TYP=0, SLP_EN=1). Restart uses ICH9 reset control
port 0xcf9, with a bounded PS/2 reset-command fallback after one second.
Failure to transition within the following second returns an error. Shutdown
has no guessed-port, triple-fault, halt or host-kill fallback.

This is a bounded platform backend, not general ACPI support. Boot services
are already gone; UEFI ResetSystem runtime mappings are not retained. No AML
interpreter or FADT/GAS parser is implemented. Unknown platforms advertise no
power capability. Port semantics were checked against upstream QEMU
[ICH9](https://github.com/qemu/qemu/blob/master/hw/acpi/ich9.c),
[LPC reset](https://github.com/qemu/qemu/blob/master/hw/isa/lpc_ich9.c), and
[ACPI core](https://github.com/qemu/qemu/blob/master/hw/acpi/core.c).
The existing restricted init-only storage test shutdown remains separate.

Interactive shell runs allow guest restart and QEMU exit on shutdown.
Diagnostic smoke retains `-no-reboot -no-shutdown` to catch faults. QEMU
[documents](https://www.qemu.org/docs/master/system/qemu-manpage.html) that
these flags suppress the normal transitions; do not use them to validate power.

## Images and candidate boot configurations

`cargo xtask vm-images --release` builds the shell UEFI image and writes raw,
VDI and VMDK copies plus a candidate VMware `.vmx` into a fresh
`target/vm-images/release-<id>/` directory. It uses external `qemu-img` next to
the detected QEMU binary. Conversion, `qemu-img info` and lossless guest-byte
comparison with the source raw image are packaging checks,
not boot compatibility evidence. These are GPT/EFI disk images, not ISOs.

- QEMU: `cargo xtask run --release`. UEFI pflash, q35, 256 MiB, one CPU,
  default emulated VGA/GOP (1280x800 BGR observed), PS/2 keyboard/mouse, no
  USB tablet, no CPU or accelerator override (Windows default TCG). Boot disk is raw on q35 AHCI
  with snapshot protection. The separate PIIX3 IDE primary-master data image
  uses the required `RUSTOS-DISPOSABLE` serial. Only that data-disk profile is
  accepted by the storage driver.
- VirtualBox candidate: create Other/Unknown 64-bit VM, enable EFI, one CPU,
  256 MiB, attach VDI as SATA boot disk, VBoxVGA without 3D, PS/2 pointing
  device; disable USB tablet and Secure Boot. No BIOS variant exists. Graphics and input are
  unverified. Do not attach a valuable data disk; without the designated
  controller/serial profile, persistent storage is unavailable or rejected.
- VMware candidate: open the generated `.vmx` and its VMDK together, EFI,
  one CPU, 256 MiB, emulated IDE boot disk, default SVGA firmware framebuffer,
  PS/2 input and USB disabled; keep Secure Boot off for the unsigned boot image.
  The candidate virtual hardware version is 14;
  it is not a tested VMware product version. Persistent data and power are
  unverified.

VirtualBox and VMware power require platform discovery evidence before any
new backend or storage driver. Installing those products or adding speculative
drivers is not necessary to build conversion artifacts.
Firmware and input settings are described by the vendors in the
[VirtualBox manual](https://download.virtualbox.org/virtualbox/5.2.16/UserManual.pdf)
and [Workstation firmware guide](https://techdocs2-prod.adobecqms.net/content/dam/broadcom/techdocs/us/en/pdf/vmware/desktop-hypervisors/workstation/vmware-workstation-pro-17-0.pdf).
Those references do not establish compatibility with this guest.

## Verification record

Host: Windows, pinned nightly-2026-10-08 (gnullvm). QEMU executable reports
`11.1.0 (v11.1.0-12130-ge470268ff4)`. Firmware: bundled edk2 UEFI pflash,
`edk2-x86_64-code.fd` SHA256
`33090cc07675baa5190d9f1e84bf5176b33bcbfa9bacac522961150cdb6dbb2a`,
vars template `edk2-i386-vars.fd` SHA256
`5d2ac383371b408398accee7ec27c8c09ea5b74a0de0ceea6513388b15be5d1e`.
The firmware package does not expose a separate version here; hashes identify
the tested bytes. Each test copies the vars template.

`cargo xtask shell-smoke [--release]` exercises actual menu requests, guest
restart into a second firmware/kernel/shell boot in the same emulator process,
then shutdown that must terminate QEMU successfully. Existing Files/Trash
tests and independent host remounts verify exact persisted bytes. A success
serial marker alone is insufficient. Logs and screenshots remain under
`target/shell-test/<profile>-<id>/`.

`cargo xtask power-smoke [--release]` runs the same power sequence with saved
preferences and an independent exact-byte host remount, without the multi-stage
Files/Trash navigation stress. It still checks real pixels, cancellation,
two firmware boots per cycle and emulator exit; it does not replace the broader
shell regression. Earlier debug runs hit application-launch and multi-stage
Files deadlines while progressing. The final full debug run passed after
waiting for expected pixels, allowing 60 seconds for launch, 120 seconds for
a multi-stage file job and 600 seconds for each outer boot/reset cycle. These
are host test limits, not guest latency guarantees. The power harness waits for
expected pixels instead of assuming the first frame completes within 500 ms.

VirtualBox and VMware executables were not found on PATH or standard Program
Files locations on this host: both remain **UNVERIFIED**, including firmware,
disk, graphics, input and power. No product version is recorded as tested.

Local results on 2026-10-20:

| Check | Result |
| --- | --- |
| `cargo xtask ci` | PASS, 158 host tests plus lint/build/ELF/privacy checks |
| `cargo xtask power-smoke` and `--release` | PASS, two cycles per profile: cancel, actual restart, second boot, actual shutdown, exact preference bytes |
| `cargo xtask shell-smoke` and `--release` | PASS, two cycles per profile including Files/Trash and exact-byte remount after actual power actions |
| `cargo xtask storage-smoke` and `--release` | PASS, three boots per profile including corrupt-volume rejection |
| `cargo xtask smoke` and `--release` | PASS, 9/9 per profile before the final fw_cfg guard; normal boot rechecked successfully in both profiles after that guard |
| `cargo xtask vm-images --release` | PASS, VDI/VMDK conversion and guest-byte comparisons; no secondary-VM boot claim |

Focused debug evidence: `target/shell-test/debug-1792445175078183900/`;
focused release: `target/shell-test/release-1792445355549370400/`;
full release: `target/shell-test/release-1792445406515528200/`.
Full debug: `target/shell-test/debug-1792445550131914300/`.
Packaged candidates: `target/vm-images/release-1792445508477430100/`.
The earlier scheduler test failed after a real restart when its fixed 150 ms
window expired before all spinners ran. It now requires actual interleaving with
a three-second hard failure bound; the original pass assertions remain.

Untested: PS/2 reset fallback, live unresponsive-app injection, power-path flush
failure/timeout injection, suspend/resume, other QEMU profiles, secondary VMs
and physical hardware. Host filesystem tests cover I/O/flush-failure poisoning;
they are not a guest power-failure injection test.
