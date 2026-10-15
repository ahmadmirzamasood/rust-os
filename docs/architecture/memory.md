# Memory management

How the kernel manages physical memory, page tables, stacks and the heap,
which rules the code enforces, and what is not done yet. Source:
[`crates/mm`](../../crates/mm) (host-tested core) and
[`kernel/src/memory`](../../kernel/src/memory) (the parts that touch the
machine).

## Boot hand-off

The kernel's `BootloaderConfig` asks `bootloader` 0.11.17 for:

- a **physical-memory window**: all physical memory from 0 up to at least
  `max(highest memory-map address, 4 GiB)`, mapped writable and
  no-execute at an offset the bootloader chooses;
- a **128 KiB boot stack**, which the bootloader maps with an unmapped guard
  page directly below it.

The bootloader also enables `EFER.NXE` and `CR0.WP` and maps the kernel's ELF
segments with their own permissions. The kernel checks these at start-up
(and turns them on if they are off), and enables SMEP and SMAP when CPUID
reports them and no user-accessible page exists at boot. With QEMU's default
CPU model SMEP/SMAP are reported unsupported; with `-cpu max` the kernel
enables both, and user processes still work because the kernel never touches
user pages through user addresses (see
[processes.md](processes.md#user-memory-access-by-the-kernel)).

All of the bootloader's chosen mappings are placed in the upper half
(`dynamic_range_start`), leaving the lower half to user processes; see
ADR-015.

## CPU tables and faults

Before touching memory the kernel installs its own GDT, TSS and IDT
([`kernel/src/arch/x86_64`](../../kernel/src/arch/x86_64)):

- every CPU exception vector (0–31) has an entry stub that saves all
  registers and calls a Rust dispatcher;
- the double fault uses IST slot 1, so a kernel stack overflow (which faults
  again while delivering the page fault) still reaches a handler on a known
  good stack. That stack is first a static bootstrap stack and, once memory
  management is up, an 8-page stack with its own guard page;
- a fatal exception prints a report on the serial port (exception, decoded
  page-fault error code, CR2, registers, and the name of the guard page
  involved, if any), then panics, which draws the red banner and halts;
- external interrupts stay masked; vectors 32+ are outside the IDT.

**Recoverable probes.** Self-tests need to show that unmapped, read-only and
no-execute pages really fault. Three tiny assembly routines read, write or
call an address. When the dispatcher sees a page fault (or #GP) at exactly
the probe's access instruction, or an instruction-fetch fault at the armed
call target, it records the fault and resumes the probe on an error path.
Every other fault stays fatal.

## Physical frames

`mm::frames::FrameBitmap` keeps two bits per 4 KiB frame:

- **managed**: the frame may ever be handed out;
- **free**: managed and not allocated.

Invariant: `free ⊆ managed`. A frame is managed only if **all** of these
hold:

1. it lies in a region the firmware map marks *usable*;
2. it is at or above 1 MiB (the first MiB is never allocated);
3. it is inside the bitmap's span (4 GiB above 1 MiB);
4. nothing found at boot uses it.

For rule 4 the kernel walks the **live page tables** (every top-level slot
except the physical-memory window). It withdraws every page-table frame and
every frame any boot-time mapping points to: kernel image, boot stack, boot
information and memory map, framebuffer, and the bootloader's
identity-mapped trampolines. It also withdraws the kernel ELF file's
physical range. In QEMU this finds two frames that the firmware map calls
usable but that are still mapped: the bootloader's context-switch trampoline
lives in loader-code memory, which is reported as usable after boot. Those
frames are withdrawn too.

Consequences the code relies on:

- reserved memory is never allocated, and freeing a reserved or unknown
  frame fails with `NotManaged` instead of corrupting the allocator;
- freeing a free frame fails with `DoubleFree`;
- exhaustion returns `Exhausted`. Allocation is next-fit over 64-frame
  words;
- frames are not zeroed by the allocator. The kernel zeroes every frame it
  maps.

## Kernel address space and page tables

**Ownership.** The bootloader's mappings are foreign: the kernel never
edits them. At start-up it claims the first unused upper-half PML4 slot
(512 GiB) and only ever maps, unmaps or changes permissions inside it (plus
the lower halves of process address spaces, which it owns completely; see
[processes.md](processes.md#address-spaces)):

| Offset in the slot | Use |
| --- | --- |
| 0 – 1 GiB | Kernel heap (grows upward) |
| 256 GiB – 384 GiB | Kernel stacks (double-fault stack, one per thread): each is a guard page plus its pages; frames are freed when a thread is reaped, address space is never reused |
| 384 GiB – 512 GiB | Scratch pages for self-tests; never reused |

All page-table edits go through `mm::paging::Mapper` while holding the
memory manager's lock. The mapper:

- refuses writable + executable leaves (W^X);
- refuses to map an already-mapped page and to unmap an unmapped one;
- only creates and edits 4 KiB leaves; it translates through 2 MiB and 1 GiB
  pages but refuses to edit inside them;
- creates intermediate tables as present + writable + executable and lets
  leaves decide permissions. It refuses a leaf whose existing parent would
  silently weaken it;
- does not free intermediate tables that become empty.

Page-table memory is reached only through the physical-memory window, with
volatile accesses (the CPU writes accessed/dirty bits concurrently). No other
kernel code gets raw physical access.

**TLB.** Every map, unmap and permission change returns a `#[must_use]`
flush token; the kernel executes `invlpg` for that page before releasing the
lock. There is one CPU, so no other TLB needs a shootdown. Multiprocessor
support will need one.

## Stacks

`memory::allocate_stack(pages, label)` maps `pages` zeroed, writable,
non-executable pages above an unmapped guard page and registers the guard
under `label`. Stack address space is never reused, so the page above a stack
is the next stack's guard or unmapped. The fault report uses the guard
registry to name the guard page a fault hit, for example "guard page below
the boot kernel stack".

## Heap

The kernel's `#[global_allocator]` is `mm::heap::FreeListHeap`, an
address-ordered first-fit free list with coalescing, over the start of the
kernel's slot.

- **Alignment:** every block is aligned to `max(layout.align(), 16)` and is
  a multiple of 16 bytes; any power-of-two alignment works.
- **Growth:** starts with 256 KiB mapped; grows at its end in steps of at
  least 64 KiB, up to 1 GiB of address space. Growth is refused without
  mapping anything if the request cannot fit, or if fewer free frames remain
  than it needs plus 16 for page tables. The heap never shrinks; freed space
  is reused.
- **Failure:** a failed allocation returns null. Fallible APIs
  (`try_reserve`) get an error and the kernel continues. Infallible ones
  (`Box::new`, `Vec::push`) panic with "memory allocation of N bytes
  failed". A rejected free (outside the heap, misaligned, overlapping free
  space) is reported as heap corruption and panics.
- **Locks:** allocation never waits. If the heap lock is held, which only
  happens when an exception or panic interrupts the allocator, the request
  fails. Exception and panic handlers never allocate. Lock order is heap,
  then memory manager; the memory manager never allocates.

## Tests

| Where | What |
| --- | --- |
| `cargo xtask test` (`crates/mm`, 25 tests) | Address validation; bitmap allocation, uniqueness, exhaustion, double and reserved frees, reserve semantics; mapper map/unmap/protect/translate/walk, W^X, huge pages, restrictive parents, running out of table frames; heap alignment for every power of two up to 4 KiB, non-overlap, coalescing, growth, invalid frees, exhaustion, 20 000-operation randomized stress with invariant checks |
| Every boot (`memory::selftest`, 81 checks) | Audit of the live page tables: no page-table or mapped frame is free, no W+X mapping. Kernel file, first MiB and non-usable regions are not free. Frame allocation, uniqueness, reuse, rejected frees. Map/translate/unmap. Real page faults recovered for read-only writes, NX fetches, unmapped reads and writes, and guard pages. W^X and edits outside the kernel's slot refused. Heap alignment, non-overlap, growth and reuse. Three identical 6 000-operation stress rounds with no leak and no growth after round one. Out-of-memory requests refused without mapping. Allocation of every free frame until exhaustion, with mapping and heap growth failing cleanly, then everything returned |
| `cargo xtask smoke` | `boot` requires the self-test pass marker; `page-fault` checks the report of a write into a guard page (cause, CR2 equals the written address, guard named); `stack-overflow` checks that a boot-stack overflow is reported as a double fault on the boot stack's guard page |

## Known limitations

- One CPU; no TLB shootdown, no per-CPU stacks or IST.
- The bitmap tracks 4 GiB above 1 MiB. Usable RAM beyond that is reported
  and left unused.
- Bootloader-owned memory (boot page tables, boot information, boot stack,
  kernel ELF file copy) is never reclaimed.
- The heap never returns pages; intermediate page tables are never freed;
  stack and scratch address space is never reused.
- Only 4 KiB pages are created. The physical-memory window maps everything,
  including device ranges, with default caching; it is used only for RAM
  frames and page tables.
- User pages exist only in process address spaces (lower half); every
  kernel mapping is supervisor-only.
- The kernel's own image permissions come from the bootloader's ELF loading
  and are audited (no W+X found) but not re-mapped by the kernel.
