# Interrupts, timer and input

This page describes how the kernel takes external interrupts, keeps time,
and turns PS/2 keyboard and mouse traffic into on-screen events. Statements
marked *verified* were checked by `cargo xtask smoke` in QEMU 11.1.0 with
its bundled edk2 firmware on a Windows host. Nothing else has been tested.

## Supported devices and machine

| Item | Supported | Notes |
| --- | --- | --- |
| Interrupt controller | Legacy 8259A pair (master + slave) | Remapped to vectors 32–47. Local APIC left in the firmware's virtual-wire mode; I/O APIC unused |
| Timer | 8254 PIT channel 0, IRQ 0, requested 1000 Hz | Wakes the event loop and counts timer interrupts |
| Clock | TSC, calibrated against PIT channel 2 | Monotonic uptime; falls back to the tick count if calibration fails |
| Keyboard | PS/2 keyboard on i8042 port 1 | Scancode set 2 with controller translation, so set 1 is decoded; US layout |
| Mouse | PS/2 mouse on i8042 port 2 | Standard 3-byte packets: movement and left/middle/right buttons. No wheel |
| USB keyboard, mouse or tablet | **Not supported** | There is no USB stack. QEMU's `-device usb-tablet` and VirtualBox/VMware USB input will not work |
| VM | QEMU `q35`, one CPU | VirtualBox and VMware are untested |

## Vectors and the IDT

The IDT has all 256 gates:

- 0–31: CPU exceptions (unchanged from Phase 02; double faults on IST1).
- 32–255: one stub per vector, generated with `.irp` in
  `kernel/src/arch/x86_64/exceptions.rs` at a fixed 16-byte stride. Before
  the IDT uses them, `interrupt_entry_points` checks that the block has
  exactly that size; if not, the gates stay absent and interrupts are never
  enabled.

Every stub pushes a zero error code and its vector and runs the common
save/dispatch/restore path. The dispatcher sends vectors ≥ 32 to
`interrupts::handle`:

- 32–47 (8259 lines): spurious IRQ 7/15 detection through the in-service
  register (no EOI for a spurious IRQ 7; master-only EOI for IRQ 15), a
  per-line counter, the driver hook, then the EOI.
- Any other vector: counted as "stray" with the last vector number, no EOI
  (it did not come from the 8259). The heartbeat reports it.

The kernel target is soft-float, so the fifteen general-purpose registers
saved by the stub are the complete interrupted state.

## Synchronization rules

These hold for every interrupt handler (also documented in
`kernel/src/arch/x86_64/interrupts.rs`):

1. Handlers enter through interrupt gates, so `RFLAGS.IF` is clear while
   they run. They never nest. There is one CPU, so a handler only races with
   the code it interrupted.
2. Handlers do bounded work: acknowledge the device, move at most four bytes
   into a byte ring or bump a counter, send the EOI. Decoding, drawing and
   logging happen in the main loop.
3. Handlers never take a lock (not even a `TryLock`), never allocate and
   never print. They share state with normal code only through atomics:
   single-producer/single-consumer rings and counters.
4. Normal code reads shared values one atomic at a time and tolerates them
   changing between reads. Statistics take one snapshot of each counter;
   an earlier version read a counter twice and underflowed when a tick
   arrived in between, which the stack-overflow smoke scenario caught.
5. The main loop sleeps with `sti; hlt` only after finding no pending input
   with interrupts disabled. `sti` takes effect after the next instruction,
   so a wake-up between the check and the halt is not lost.

CPU exceptions keep the Phase 02 policy (report, then panic; the panic
handler disables interrupts first).

## Timer and clock

`time::init` first calibrates the TSC against a 50 ms PIT channel-2 interval
by polling port 0x61, then programs channel 0 (mode 2, divisor 1193 =
1000.152 Hz). IRQ 0 increments one atomic tick counter.

Ticks are a wake-up source, not a clock. QEMU drops PIT periods it cannot
deliver on time. On the Windows test host it delivered between about 85 and
700 timer interrupts per second, depending on load, instead of 1000.
Uptime therefore comes from the TSC, which QEMU's TCG derives from the
host's TSC. *Verified:* over 18–19 s of host time, the guest heartbeat
advanced at a ratio of 0.98–1.00 to host time, in debug and release. The
design assumes the TSC rate is constant; CPUID's "invariant TSC" bit is
reported at boot but not required (QEMU's default CPU does not set it).

## Input path

```text
IRQ 1 / IRQ 12 handler            main loop (event_loop.rs)
  read 0x64 status, 0x60 data       Pipeline::poll
  AUX bit? -> MOUSE_BYTES (512)       drop counter changed? -> decoder resync
           -> KEYBOARD_BYTES (256)    keyboard::Decoder  -> KeyEvent
                                      mouse::PacketDecoder -> Packet -> Pointer
                                    EventQueue (64)   -> Ui + serial log
```

The pure parts are in the host-tested `crates/input` crate: rings, both
decoders, pointer state and the event queue. It has no `unsafe` code.

### Bounded queues and overflow behaviour

| Queue | Capacity | When full |
| --- | --- | --- |
| Keyboard byte ring (IRQ → main loop) | 256 bytes | The new byte is dropped and counted. The keyboard decoder then drops any half-received prefix |
| Mouse byte ring (IRQ → main loop) | 512 bytes | The new byte is dropped and counted. The packet decoder drops its partial packet and resynchronizes on bit 3 of the first byte |
| Event queue (main loop only) | 64 events | Consecutive pointer moves are always merged into the newest one. Otherwise the oldest event is dropped and counted |

The rings drop the new byte rather than overwrite old ones because the
producer must never write a slot the consumer may be reading. Each poll
decodes at most 256 bytes per ring, so a burst cannot starve rendering.

*Verified (`input` smoke scenario, debug and release):* F9 stops the event
loop for 8 s while interrupts stay enabled. Keyboard and mouse traffic
injected during that time overflowed both rings: 104 keyboard bytes and 88
mouse bytes were dropped and counted. The decoders resynchronized, 193
events were dropped from the full event queue, and input sent after the
backlog drained worked (sentinel key, third click). No panic, exception or
hang occurred, and the timer heartbeat continued.

QEMU has its own small PS/2 queue and drops input that does not fit before
the guest sees it. In the QEMU-paced burst only 24 of 60 key presses
arrived, while the guest's counters showed no loss.

## Screen

`ui.rs` keeps a heap back buffer with the framebuffer's exact layout. The
welcome card and the input-debug panel are drawn there; only changed
rectangles are copied to the framebuffer, and the pointer is drawn on top.
Moving the pointer costs two small copies. The panel shows uptime, timer
and device interrupt counts, pointer position and buttons, the last key
(name, scancode, character, modifiers), the last eight key events, and ring
and event-queue usage. A "Click me" push button (press and release inside
counts as a click) proves hit testing. The first frame is drawn before
`BOOT OK`.

## Serial log

The main loop logs every key press/release, mouse-button change and test
button click. It also logs a timer heartbeat each second for 5 s, then
every 10 s. Pointer motion is shown on screen only, so serial output is
never the bottleneck. The exact markers are in `boot_data::markers`.

## Exception diagnostics with interrupts on

Every fatal exception report now includes a context line: which IRQ handler
was running (or "normal kernel code"), whether interrupts were enabled in
the interrupted code, and the tick count. Selector error codes of
#TS/#NP/#SS/#GP are decoded (GDT/LDT/IDT index, external event). The
`interrupt-fault` smoke scenario faults deliberately inside the timer
handler. It checks that the report names "IRQ 0 handler (PIT timer)", says
interrupts were disabled, gives the right CR2 and guard page, and that the
banner is drawn.

## Known limitations

- 8259 only: no APIC, MSI or SMP. The local APIC is assumed to be in
  virtual-wire mode, which is how OVMF leaves it. This is verified on QEMU
  `q35` only.
- Timer interrupts are lossy under emulation (see above); nothing should
  count ticks as time.
- US keyboard layout. Caps Lock and Num Lock are tracked but the LEDs are
  not driven. No mouse wheel or extra buttons; no pointer acceleration.
- The PS/2 initialization assumes a controller is present. It has bounded
  timeouts and reports failure, but has only run on QEMU's i8042.
- F9 stalls the UI for 8 s by design (a debug aid for the overflow test).
