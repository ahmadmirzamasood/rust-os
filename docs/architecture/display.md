# Software display and window service

`cargo xtask desktop --release` boots the graphical shell described in [desktop.md](../desktop.md). The two-client display demo below remains a separate smoke-test variant.
`cargo xtask diagnostics` retains the diagnostic screen and process test driver.
Both use the custom Rust kernel; no hosted window system or Linux guest is used.
The desktop variant is built separately with kernel feature `desktop`.

The `display` executable runs in ring 3 and starts `colors` and `counter` as
separate processes with separate bidirectional channels. Each channel is its
client identity; packet bytes cannot select an owner. A bounded `no_std` core
in `userspace/desktop/src/display.rs` validates requests, owns surfaces and
composes windows. The kernel retains only GOP framebuffer access, PS/2 decoding,
a bounded input handoff and validated copies through the caller's page tables.
There are no new shared mappings or user physical addresses.

## Using the slice

Click to focus and raise a window. Drag its title to move it; drag the bottom
right 12-pixel corner to resize its viewport. Title controls minimize, maximize
and close. Click the bottom task strip to restore and raise a minimized window.
F1 minimizes, F2 restores previous geometry/all minimized windows, F3 maximizes.
F4 exits the display service and returns to kernel diagnostics (a development aid).
Typing or clicking changes the demo color and logs its input count. F intentionally
panics the focused demo; its channel closes and only its windows are removed.
Closing a window sends a close event; the demo exits normally.

Resize preserves the client's fixed-size backing surface, clipping a smaller
viewport and clearing newly exposed space in a larger one. There is no resize
negotiation or client buffer reallocation yet. Controls and task labels are
original Rust primitives and use the existing Noto bitmap font. License texts
are in `licenses/`, with attribution in `THIRD_PARTY_NOTICES.md`.

## Client protocol v1

All integers are little-endian. No Rust structs, pointers, padding or native
`usize` values cross the channel. The 8-byte request header is version u8 = 1,
opcode u8, reserved u16 = 0, window ID u32 (0 only for Create).

| Opcode | Exact payload after header |
| --- | --- |
| 1 Create | width u32, height u32, 1..32 printable ASCII title bytes |
| 2 Fill | x u32, y u32, width u32, height u32, red/green/blue u8 |
| 3 Present | empty |
| 4 Close | empty |
| 5 Text | x u32, y u32, red/green/blue u8, 1..128 printable ASCII bytes |

Reply: 8 bytes, window ID u32 (0 on failure), status u8 (0 success, 1 rejected),
three zero reserved bytes. Replies follow request order. Events may interleave
with replies: a client must distinguish their lengths. An event is 16 bytes:
window u32, kind u8 (1 key, 2 pointer button, 3 closed, 4 pointer motion), pressed u8 (0/1),
reserved u16 = 0, first i32/u32, second i32. Keys carry PS/2 set-1 scancode in
first, zero second; pointer buttons carry content-local x/y. Coordinates may be
outside the surface during pointer capture. Only the left button is currently
routed; motion goes to the captured or topmost hit window.
Unknown versions/opcodes, lengths, reserved bits, IDs, foreign ownership,
invalid text, geometry and overflowing rectangles fail before mutation.

There are at most eight windows, dimensions 1..1024, at most 524288 pixels per
surface, and an 8 MiB process-local reclaiming arena. All reservations are
fallible. IDs increase without reuse; exhaustion refuses creation. Two
service-owned RGB buffers per surface separate staging writes and published
content. Present copies staging into published content; the client cannot
modify either through a shared mapping. Text clips at the backing surface.
No shared-memory access is permitted by this protocol; the existing process
page-copy validation remains the only memory crossing the kernel boundary.

Only eight messages per client and 64 device events are handled per iteration.
Channel sends are bounded/nonblocking. A slow client may lose notifications;
there is no reliable event acknowledgment or resynchronization protocol yet.
Input queue overflow emits a reset which cancels manager drag/capture, preventing
a lost release from leaving a permanent drag. Peer closure releases the client's
windows, surfaces, focus and capture without affecting other clients.

## Device boundary

Additive ABI v1 calls 22..24 exist only in the desktop kernel for the root
bundled `display` process, or `shell` in the shell variant (not a child spawned with the same executable name).
Other callers receive UnknownSyscall. This is a boot-service role, not a general
capability delegation mechanism.

- DISPLAY_INFO (22): zero arguments; returns width in low u32, height in high
  u32, and claims the device. Maximum accepted mode is 1365x1024.
- DISPLAY_ROW (23): y, pointer, length, zero; length must be exactly width*3
  and <=4096. RGB bytes are copied from validated readable user pages before
  conversion into the GOP format/stride. No user pointer is dereferenced.
- DISPLAY_INPUT (24): output pointer, 16, zero, zero; nonblocking 16-byte
  packet. All output pages must be writable before dequeue. Kind 1 = key,
  2 = pointer motion, 3 = button, 4 = reset. Byte 1 is pressed; byte 2 is button
  (0 left/1 right/2 middle); x/scancode at bytes 4..8, y/character at 8..12;
  other bytes zero.

The compositor clears and reconstructs damaged scenes in a process-local
back buffer, then compares composed RGB rows with the last presented buffer.
Only changed rows cross the device boundary. Pointer composition uses a temporary
row, leaving the scene buffer clean. An idle scene sends no scanlines. This is
software double buffering with scanline damage suppression, not synchronized
page flipping: tearing is possible, and there is no GPU acceleration or vsync.
A display process failure releases device ownership; the diagnostic UI resumes
on its next refresh. Automatic service restart is not implemented.

## Validation and limits

`cargo xtask ci` includes protocol, ownership, focus/capture, lifecycle and
fresh-composition equivalence host checks, plus bare-metal builds/lints.
`cargo xtask smoke --scenario desktop [--release]` boots QEMU/UEFI, checks two
independent window colors, injects focus/input and move/resize gestures, checks
exposed pixels, deliberately panics one client and verifies the surviving
client receives more input. It also exits the display service and checks that
the complete diagnostic scene is restored. Evidence is under `target/smoke/<profile>/desktop/`.
Read actual command results before claiming a gate passed. `cargo xtask smoke`
also retains all previous boot, fault, input and process scenarios.

The demos are bundled examples; dynamic client registration, reliable delivery,
Unicode/layout/shaping, alpha blending, arbitrary pixel uploads, a Files GUI,
multiple displays, USB input and GPU acceleration remain outside this slice.
The 12 MiB arena can reject a mode or surface combination below the individual
geometry ceilings. The demonstrated mode is QEMU GOP 1280x800, RGB/BGR conversion;
no VirtualBox/VMware or physical-device compatibility is implied.


Verified on Windows on 2026-10-19: `cargo xtask ci` passed 143 host tests and
all lint/build/privacy checks. `cargo xtask smoke` and `cargo xtask smoke
--release` each passed all nine scenarios with QEMU 11.1.0
(v11.1.0-12130-ge470268ff4), its bundled edk2 code/variable templates, q35,
256 MiB and one CPU. Toolchain: nightly-2026-10-08. The five acceptance gates
for this bounded demo slice are met using host and VM evidence. Minimize,
maximize and close have host lifecycle tests; their on-screen controls have
not been driven by the VM automation. Other VM platforms remain untested.
