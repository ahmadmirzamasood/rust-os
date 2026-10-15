//! Memory-management self-test, run on every boot after [`super::init`].
//!
//! These checks run on the real machine state (actual firmware map, actual
//! page tables, actual faults), complementing the host tests of the `mm`
//! crate. Each group prints one summary line; the run ends with a pass or
//! fail marker that `cargo xtask smoke` checks. The test leaves memory as it
//! found it: every frame, mapping and heap block it takes is returned, which
//! is itself checked.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::alloc::Layout;

use boot_data::markers;
use bootloader_api::info::{MemoryRegion, MemoryRegionKind};
use mm::frames::FrameError;
use mm::paging::MapError;
use mm::{PAGE_SIZE, PhysAddr, PhysFrame};

use super::{Flags, MIN_ALLOCATABLE, MemError};
use crate::arch::x86_64::probe::{self, Fault};
use crate::kprintln;

/// Result of a self-test run.
#[derive(Debug, Clone, Copy)]
pub struct Outcome {
    pub checks: u32,
    pub failures: u32,
    pub first_failure: Option<&'static str>,
}

impl Outcome {
    pub fn passed(&self) -> bool {
        self.failures == 0
    }
}

struct Checker {
    outcome: Outcome,
}

impl Checker {
    fn check(&mut self, name: &'static str, ok: bool) -> bool {
        self.outcome.checks += 1;
        if !ok {
            self.outcome.failures += 1;
            self.outcome.first_failure.get_or_insert(name);
            kprintln!("{} {}", markers::MEMORY_SELFTEST_CHECK_FAILED, name);
        }
        ok
    }
}

/// What the test needs to know about the boot environment.
#[derive(Debug)]
pub struct Environment<'a> {
    pub regions: &'a [MemoryRegion],
    /// Physical range of the kernel ELF file.
    pub kernel_file: (u64, u64),
}

/// Run every group and print the verdict.
pub fn run(env: &Environment<'_>) -> Outcome {
    let mut c = Checker {
        outcome: Outcome {
            checks: 0,
            failures: 0,
            first_failure: None,
        },
    };
    let frames_at_start = super::free_frames();
    let heap_at_start = super::heap::stats().map_or(0, |s| s.mapped_bytes);
    reservations(&mut c, env);
    frame_allocator(&mut c, env);
    paging(&mut c);
    guard_pages(&mut c);
    heap(&mut c);
    exhaustion(&mut c);
    // Everything taken was given back, except pages the heap grew by (it
    // never shrinks) and a few page tables created for the scratch and stack
    // areas.
    let frames_at_end = super::free_frames();
    let heap_growth = super::heap::stats().map_or(0, |s| s.mapped_bytes) - heap_at_start;
    let heap_frames = heap_growth / PAGE_SIZE;
    let kept = frames_at_start.saturating_sub(frames_at_end);
    let unexplained = kept.saturating_sub(heap_frames);
    c.check(
        "frames returned after self-test",
        kept >= heap_frames && unexplained <= 8,
    );
    kprintln!(
        "[rust-os] memory self-test: {} frames free before, {} after: {} hold heap growth, \
         {} are new page tables",
        frames_at_start,
        frames_at_end,
        heap_frames,
        unexplained
    );

    let outcome = c.outcome;
    if outcome.passed() {
        kprintln!(
            "{} ({} checks)",
            markers::MEMORY_SELFTEST_PASS,
            outcome.checks
        );
    } else {
        kprintln!(
            "{} {} of {} checks failed, first: {}",
            markers::MEMORY_SELFTEST_FAIL,
            outcome.failures,
            outcome.checks,
            outcome.first_failure.unwrap_or("?")
        );
    }
    outcome
}

fn frame(addr: u64) -> Option<PhysFrame> {
    PhysFrame::from_start(PhysAddr::new(addr)?)
}

/// Reserved memory is never free: boot mappings, page tables, the kernel
/// file, non-usable firmware regions and the first MiB.
fn reservations(c: &mut Checker, env: &Environment<'_>) {
    match super::audit_reservations() {
        Ok(audit) => {
            c.check(
                "no page-table or mapped frame is free",
                audit.free_but_in_use == 0,
            );
            c.check(
                "no writable+executable mapping",
                audit.writable_executable == 0,
            );
            kprintln!(
                "[rust-os] memory self-test: reservations: {} page tables and {} mapped frames \
                 audited, {} free (must be 0), {} W+X",
                audit.page_tables,
                audit.mapped_frames,
                audit.free_but_in_use,
                audit.writable_executable
            );
        }
        Err(_) => {
            c.check("audit ran", false);
        }
    }
    let (kstart, klen) = env.kernel_file;
    c.check(
        "kernel file not free",
        !super::any_frame_free_in(kstart, kstart + klen),
    );
    c.check(
        "first MiB not free",
        !super::any_frame_free_in(0, MIN_ALLOCATABLE),
    );
    let non_usable = env
        .regions
        .iter()
        .filter(|r| r.kind != MemoryRegionKind::Usable)
        .all(|r| !super::any_frame_free_in(r.start, r.end));
    c.check("non-usable firmware regions not free", non_usable);
}

fn in_usable(env: &Environment<'_>, addr: u64) -> bool {
    env.regions
        .iter()
        .any(|r| r.kind == MemoryRegionKind::Usable && addr >= r.start && addr + PAGE_SIZE <= r.end)
}

/// Allocation, uniqueness, alignment, free, reuse and invalid frees.
fn frame_allocator(c: &mut Checker, env: &Environment<'_>) {
    const N: usize = 64;
    let before = super::free_frames();
    let mut taken = [None::<PhysFrame>; N];
    for slot in taken.iter_mut() {
        *slot = super::allocate_frame().ok();
    }
    let all = taken.iter().all(Option::is_some);
    c.check("64 frames allocated", all);
    let frames: Vec<PhysFrame> = taken.iter().flatten().copied().collect();
    let mut sorted = frames.clone();
    sorted.sort();
    sorted.dedup();
    c.check("allocated frames unique", sorted.len() == frames.len());
    c.check(
        "allocated frames aligned, usable, above 1 MiB, not free",
        frames.iter().all(|f| {
            let a = f.start().as_u64();
            a % PAGE_SIZE == 0
                && a >= MIN_ALLOCATABLE
                && in_usable(env, a)
                && !super::frame_is_free(*f)
        }),
    );
    let (kstart, klen) = env.kernel_file;
    c.check(
        "allocated frames outside the kernel file",
        frames.iter().all(|f| {
            let a = f.start().as_u64();
            a + PAGE_SIZE <= kstart || a >= kstart + klen
        }),
    );
    c.check(
        "free count dropped by 64",
        super::free_frames() + N as u64 == before,
    );
    for f in &frames {
        let _ = super::free_frame(*f);
    }
    c.check("free count restored", super::free_frames() == before);

    if let Some(&first) = frames.first() {
        c.check(
            "double free rejected",
            super::free_frame(first) == Err(MemError::Frames(FrameError::DoubleFree(first))),
        );
    }
    if let Some(kernel) = frame(kstart & !(PAGE_SIZE - 1)) {
        c.check(
            "freeing a kernel frame rejected",
            super::free_frame(kernel) == Err(MemError::Frames(FrameError::NotManaged(kernel))),
        );
    }
    if let Some(low) = frame(0x1000) {
        c.check(
            "freeing a frame below 1 MiB rejected",
            super::free_frame(low) == Err(MemError::Frames(FrameError::NotManaged(low))),
        );
    }
    c.check(
        "rejected frees changed nothing",
        super::free_frames() == before,
    );

    // Reuse: many allocate/free rounds keep the count exact.
    let mut reuse_ok = true;
    for _ in 0..1000 {
        match (super::allocate_frame(), super::allocate_frame()) {
            (Ok(a), Ok(b)) => {
                reuse_ok &= a != b;
                reuse_ok &= super::free_frame(a).is_ok() && super::free_frame(b).is_ok();
            }
            _ => reuse_ok = false,
        }
    }
    c.check(
        "1000 allocate/free rounds",
        reuse_ok && super::free_frames() == before,
    );
    kprintln!(
        "[rust-os] memory self-test: frames: 64 unique aligned frames from usable RAM, freed, \
         reused 1000x; double free and reserved/low frees rejected"
    );
}

fn expect_fault(
    c: &mut Checker,
    name: &'static str,
    result: Result<impl Sized, Fault>,
    addr: u64,
    present: bool,
) {
    let ok = match result {
        Err(fault) => {
            fault.is_page_fault() && fault.address == addr && fault.page_present() == present
        }
        Ok(_) => false,
    };
    c.check(name, ok);
}

/// Map, translate, permissions (read-only, NX, W^X), unmap, ownership.
fn paging(c: &mut Checker) {
    let Ok(scratch) = super::reserve_scratch(4) else {
        c.check("scratch space reserved", false);
        return;
    };
    let p0 = scratch;
    let addr = p0.start().as_u64();
    let Ok(frame) = super::map_new(p0, Flags::DATA) else {
        c.check("scratch page mapped", false);
        return;
    };
    let t = super::translate(addr + 0x123);
    c.check(
        "translation matches new mapping",
        t.is_some_and(|t| {
            t.phys.as_u64() == frame.start().as_u64() + 0x123
                && t.page_size == PAGE_SIZE
                && t.writable
                && !t.executable
        }),
    );
    let ptr = core::ptr::with_exposed_provenance_mut::<u8>(addr as usize);
    // SAFETY: `p0` was just mapped writable and is owned by this test.
    unsafe { ptr.write_volatile(0xA5) };
    // SAFETY: mapped RAM.
    c.check(
        "mapped page readable",
        unsafe { probe::read_u8(addr) } == Ok(0xA5),
    );
    c.check(
        "double map refused",
        super::map_new(p0, Flags::DATA) == Err(MemError::Map(MapError::AlreadyMapped)),
    );

    // Read-only: writes fault (present, write), reads still work.
    c.check("protect read-only", super::protect(p0, Flags::READ).is_ok());
    // SAFETY: the page is read-only, so a successful write would be a bug
    // the check reports; the byte is ours either way.
    let write = unsafe { probe::write_u8(addr, 0x5A) };
    expect_fault(c, "write to read-only page faults", write, addr, true);
    c.check(
        "fault was a write",
        write.err().is_some_and(|f| f.was_write()),
    );
    // SAFETY: mapped RAM.
    c.check(
        "read-only page readable",
        unsafe { probe::read_u8(addr) } == Ok(0xA5),
    );

    // No-execute: fetching from a data page faults.
    // SAFETY: the page is non-executable (the fetch faults and is recovered).
    let exec = unsafe { probe::execute(addr) };
    expect_fault(c, "execute from NX page faults", exec, addr, true);
    c.check(
        "fault was an instruction fetch",
        exec.err().is_some_and(|f| f.was_instruction_fetch()),
    );

    // W^X transition: write code while writable, then make it executable.
    c.check("protect writable", super::protect(p0, Flags::DATA).is_ok());
    // SAFETY: mapped writable, owned by this test. 0xC3 is `ret`.
    unsafe { ptr.write_volatile(0xC3) };
    c.check(
        "protect executable",
        super::protect(p0, Flags::CODE).is_ok(),
    );
    // SAFETY: the page now holds a single `ret`.
    c.check("execute code page", unsafe { probe::execute(addr) }.is_ok());
    // SAFETY: the page is read/execute only; a successful write is a bug.
    let write = unsafe { probe::write_u8(addr, 0) };
    expect_fault(c, "write to code page faults", write, addr, true);
    c.check(
        "writable+executable refused",
        super::protect(
            p0,
            Flags {
                writable: true,
                executable: true,
            },
        ) == Err(MemError::Map(MapError::WriteExecute)),
    );

    // Unmap: the frame comes back, the page faults as not present.
    c.check("unmap returns frame", super::unmap(p0) == Ok(frame));
    c.check(
        "unmapped page untranslated",
        super::translate(addr).is_none(),
    );
    // SAFETY: unmapped; the read faults and is recovered.
    expect_fault(
        c,
        "read of unmapped page faults",
        unsafe { probe::read_u8(addr) },
        addr,
        false,
    );
    // SAFETY: unmapped; the write faults and is recovered.
    expect_fault(
        c,
        "write to unmapped page faults",
        unsafe { probe::write_u8(addr, 1) },
        addr,
        false,
    );
    c.check(
        "unmap of unmapped page refused",
        super::unmap(p0) == Err(MemError::Map(MapError::NotMapped)),
    );
    c.check("frame freed", super::free_frame(frame).is_ok());

    // A second page in the same table: exact frame accounting.
    if let Some(p1) = p0.checked_add(1) {
        let before = super::free_frames();
        let mapped = super::map_new(p1, Flags::DATA).is_ok();
        let after_map = super::free_frames();
        let freed = super::unmap_and_free(p1).is_ok();
        c.check(
            "map/unmap accounting exact",
            mapped && freed && after_map + 1 == before && super::free_frames() == before,
        );
    }

    // Foreign (bootloader-owned) pages are never edited.
    let code = super::page(paging as *const () as u64 & !(PAGE_SIZE - 1));
    if let Some(code) = code {
        c.check(
            "map in foreign region refused",
            super::map_new(code, Flags::DATA) == Err(MemError::NotOwned),
        );
        c.check(
            "unmap of kernel code refused",
            super::unmap(code) == Err(MemError::NotOwned),
        );
        c.check(
            "protect of kernel code refused",
            super::protect(code, Flags::DATA) == Err(MemError::NotOwned),
        );
        c.check(
            "kernel code still mapped",
            super::translate(code.start().as_u64()).is_some(),
        );
    }
    kprintln!(
        "[rust-os] memory self-test: paging: map/translate/unmap at {:#x}; read-only write, \
         NX fetch and unmapped access fault and are recovered; W^X and foreign edits refused",
        addr
    );
}

/// Guard pages below the boot stack, the double-fault stack and a new
/// stack are unmapped and fault on access.
fn guard_pages(c: &mut Checker) {
    let mut checked = 0;
    for label in [markers::BOOT_STACK_GUARD, markers::DOUBLE_FAULT_STACK_GUARD] {
        match super::find_guard(label) {
            Some(guard) => {
                checked += 1;
                c.check("guard page unmapped", super::translate(guard).is_none());
                // SAFETY: the guard page is unmapped; the read faults.
                let read = unsafe { probe::read_u8(guard + 8) };
                expect_fault(c, "guard page read faults", read, guard + 8, false);
                c.check(
                    "fault names the guard",
                    super::describe_guard(guard + 8) == Some(label),
                );
            }
            None => {
                c.check("guard page registered", false);
            }
        }
    }

    let before = super::free_frames();
    match super::allocate_stack(4, "guard page below a self-test stack") {
        Ok(stack) => {
            c.check(
                "new stack mapped writable",
                super::translate(stack.bottom).is_some_and(|t| t.writable && !t.executable),
            );
            c.check("new stack top aligned", stack.top % 16 == 0);
            c.check(
                "new stack guard unmapped",
                super::translate(stack.guard).is_none(),
            );
            let top_word = core::ptr::with_exposed_provenance_mut::<u64>((stack.top - 8) as usize);
            // SAFETY: the top word of the freshly mapped stack, owned by us.
            unsafe { top_word.write_volatile(0x1234_5678) };
            // SAFETY: unmapped guard; the read faults.
            let read = unsafe { probe::read_u8(stack.bottom - 1) };
            expect_fault(
                c,
                "stack overflow into guard faults",
                read,
                stack.bottom - 1,
                false,
            );
            // SAFETY: nothing runs on or references this stack.
            c.check("stack freed", unsafe { super::free_stack(stack) }.is_ok());
            c.check(
                "freed stack unmapped",
                super::translate(stack.bottom).is_none(),
            );
            let kept = before.saturating_sub(super::free_frames());
            c.check("stack frames returned", kept <= 3);
        }
        Err(_) => {
            c.check("stack allocated", false);
        }
    }
    kprintln!(
        "[rust-os] memory self-test: guard pages: {} registered stack guards and a new 4-page \
         stack fault on access and are named in diagnostics",
        checked
    );
}

/// Deterministic xorshift64 for the stress test.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Whether all `len` bytes at `ptr` equal `tag`, compared in chunks so the
/// work happens in the optimized `memcmp` even in debug builds.
///
/// # Safety
///
/// `[ptr, ptr + len)` is a live allocation owned by the caller.
unsafe fn filled_with(ptr: *const u8, len: usize, tag: u8) -> bool {
    const CHUNK: usize = 256;
    let pattern = [tag; CHUNK];
    // SAFETY: per the contract above.
    let bytes = unsafe { core::slice::from_raw_parts(ptr, len) };
    bytes
        .chunks(CHUNK)
        .all(|chunk| chunk == &pattern[..chunk.len()])
}

struct StressResult {
    ok: bool,
    allocations: u64,
    peak: usize,
}

/// One stress round: random allocate/verify/free over `SLOTS` slots, then
/// free everything.
fn stress_round(seed: u64, operations: u32) -> StressResult {
    const SLOTS: usize = 128;
    let mut rng = Rng(seed);
    let mut slots: [Option<(*mut u8, Layout, u8)>; SLOTS] = [None; SLOTS];
    let mut ok = true;
    let mut allocations = 0;
    let mut peak = 0;
    for op in 0..operations {
        let i = rng.below(SLOTS as u64) as usize;
        if let Some((ptr, layout, tag)) = slots[i].take() {
            // SAFETY: a live block of `layout.size()` bytes we filled.
            ok &= unsafe { filled_with(ptr, layout.size(), tag) };
            // SAFETY: allocated below with `layout`, freed once.
            unsafe { alloc::alloc::dealloc(ptr, layout) };
        } else {
            let size = if rng.below(16) == 0 {
                1 + rng.below(16 * 1024)
            } else {
                1 + rng.below(512)
            };
            let align = 1usize << rng.below(8);
            let Ok(layout) = Layout::from_size_align(size as usize, align) else {
                ok = false;
                continue;
            };
            // SAFETY: `layout` has non-zero size.
            let ptr = unsafe { alloc::alloc::alloc(layout) };
            if ptr.is_null() || !(ptr as usize).is_multiple_of(align) {
                ok = false;
                continue;
            }
            let tag = op as u8;
            // SAFETY: fresh block of `layout.size()` bytes.
            unsafe { ptr.write_bytes(tag, layout.size()) };
            slots[i] = Some((ptr, layout, tag));
            allocations += 1;
        }
        if op % 512 == 0
            && let Some(stats) = super::heap::stats()
        {
            peak = peak.max(stats.heap.used_bytes);
        }
    }
    for (ptr, layout, tag) in slots.into_iter().flatten() {
        // SAFETY: live block we filled; freed once.
        ok &= unsafe { filled_with(ptr, layout.size(), tag) };
        // SAFETY: allocated with `layout`, freed once.
        unsafe { alloc::alloc::dealloc(ptr, layout) };
    }
    StressResult {
        ok,
        allocations,
        peak,
    }
}

/// Alignment, non-overlap, stress with leak and reuse checks, OOM.
fn heap(c: &mut Checker) {
    let Some(baseline) = super::heap::stats() else {
        c.check("heap available", false);
        return;
    };

    // Alignment and non-overlap across every power of two up to 4 KiB.
    let mut blocks: Vec<(usize, usize, Layout)> = Vec::with_capacity(13 * 6);
    let mut aligned = true;
    for shift in 0..=12 {
        for size in [1usize, 7, 16, 100, 1000, 5000] {
            let Ok(layout) = Layout::from_size_align(size, 1 << shift) else {
                aligned = false;
                continue;
            };
            // SAFETY: non-zero size.
            let ptr = unsafe { alloc::alloc::alloc(layout) };
            aligned &= !ptr.is_null()
                && (ptr as usize).is_multiple_of(layout.align())
                && (ptr as usize).is_multiple_of(16);
            if !ptr.is_null() {
                blocks.push((ptr as usize, size, layout));
            }
        }
    }
    let mut ranges: Vec<(usize, usize)> = blocks.iter().map(|&(p, s, _)| (p, p + s)).collect();
    ranges.sort_unstable();
    let disjoint = ranges.windows(2).all(|w| w[0].1 <= w[1].0);
    for &(p, _, layout) in &blocks {
        // SAFETY: allocated above with `layout`, freed once.
        unsafe { alloc::alloc::dealloc(p as *mut u8, layout) };
    }
    drop(ranges);
    drop(blocks);
    c.check("heap alignment 1..4096", aligned);
    c.check("heap blocks disjoint", disjoint);

    // Ordinary collections.
    let mut v: Vec<u64> = Vec::new();
    for i in 0..10_000u64 {
        v.push(i);
    }
    let sum: u64 = v.iter().sum();
    drop(v);
    let boxed = Box::new([7u8; 3000]);
    c.check(
        "Vec and Box work",
        sum == 49_995_000 && boxed.iter().all(|&b| b == 7),
    );
    drop(boxed);

    // Growth: hold well over the initial mapping, then free it and check
    // that a later large allocation reuses the space instead of growing.
    const CHUNK: usize = 64 * 1024;
    const CHUNKS: usize = 16;
    let before_growth = super::heap::stats().unwrap_or_default();
    let mut chunks: Vec<Vec<u8>> = Vec::with_capacity(CHUNKS);
    let mut reserved = true;
    for i in 0..CHUNKS {
        let mut chunk = Vec::new();
        if chunk.try_reserve_exact(CHUNK).is_ok() {
            chunk.resize(CHUNK, i as u8);
            chunks.push(chunk);
        } else {
            reserved = false;
        }
    }
    let grown = super::heap::stats().unwrap_or_default();
    let intact = chunks.iter().enumerate().all(|(i, chunk)| {
        // SAFETY: a live Vec buffer of `chunk.len()` initialized bytes.
        unsafe { filled_with(chunk.as_ptr(), chunk.len(), i as u8) }
    });
    drop(chunks);
    let mut again: Vec<u8> = Vec::new();
    let reused = again.try_reserve_exact(CHUNK * CHUNKS - 4096).is_ok();
    let after_reuse = super::heap::stats().unwrap_or_default();
    drop(again);
    c.check(
        "heap grew on demand",
        reserved && grown.growths > before_growth.growths,
    );
    c.check(
        "heap mapping grew past the initial size",
        grown.mapped_bytes >= before_growth.mapped_bytes + (CHUNK * CHUNKS) as u64 / 2,
    );
    c.check("grown memory intact", intact);
    c.check(
        "freed growth reused without growing",
        reused && after_reuse.mapped_bytes == grown.mapped_bytes,
    );
    kprintln!(
        "[rust-os] memory self-test: heap growth: {} KiB held in {} chunks; mapping {} -> {} KiB \
         in {} growth steps; freed space reused for one {} KiB block",
        CHUNK * CHUNKS / 1024,
        CHUNKS,
        before_growth.mapped_bytes / 1024,
        grown.mapped_bytes / 1024,
        grown.growths - before_growth.growths,
        (CHUNK * CHUNKS - 4096) / 1024
    );

    // Repeatable stress: identical rounds, no leaks, no growth after round 1.
    const ROUNDS: usize = 3;
    const OPERATIONS: u32 = 6_000;
    let mut used_after = [0usize; ROUNDS];
    let mut mapped_after = [0u64; ROUNDS];
    let mut peaks = [0usize; ROUNDS];
    let mut allocations = 0;
    let mut stress_ok = true;
    for round in 0..ROUNDS {
        let result = stress_round(0x2545_f491_4f6c_dd1d, OPERATIONS);
        stress_ok &= result.ok;
        allocations += result.allocations;
        peaks[round] = result.peak;
        if let Some(stats) = super::heap::stats() {
            used_after[round] = stats.heap.used_bytes;
            mapped_after[round] = stats.mapped_bytes;
        }
        c.check(
            "heap invariants after stress round",
            super::heap::check().is_ok(),
        );
    }
    c.check("stress contents intact, alignment held", stress_ok);
    c.check(
        "no leak: used bytes back to baseline after each round",
        used_after.iter().all(|&u| u == baseline.heap.used_bytes),
    );
    c.check(
        "memory reused: no growth after round 1",
        mapped_after[1] == mapped_after[0] && mapped_after[2] == mapped_after[0],
    );
    c.check(
        "rounds repeatable",
        peaks[0] == peaks[1] && peaks[1] == peaks[2],
    );

    // Out of memory: requests beyond the heap's range or the free frames
    // fail without mapping anything, and the heap keeps working.
    let frames_before = super::free_frames();
    let mapped_before = super::heap::stats().map(|s| s.mapped_bytes);
    let mut huge: Vec<u8> = Vec::new();
    let beyond_range = huge
        .try_reserve_exact(2 * super::HEAP_MAX as usize)
        .is_err();
    let beyond_frames_bytes = (frames_before + 1024) * PAGE_SIZE;
    let beyond_frames = beyond_frames_bytes < super::HEAP_MAX
        && huge
            .try_reserve_exact(beyond_frames_bytes as usize)
            .is_err();
    c.check("allocation beyond heap range fails", beyond_range);
    c.check("allocation beyond free frames fails", beyond_frames);
    c.check(
        "failed allocations mapped nothing",
        super::free_frames() == frames_before
            && super::heap::stats().map(|s| s.mapped_bytes) == mapped_before,
    );
    let small = Box::new(0xfeed_u32);
    c.check("heap usable after OOM", *small == 0xfeed);
    drop(small);

    let stats = super::heap::stats().unwrap_or_default();
    kprintln!(
        "[rust-os] memory self-test: heap: alignment 1..4096 ok; {} x {} ops ({} allocations) \
         repeatable, no leak; mapped {} KiB in {} free blocks, peak use {} KiB, {} growths, \
         {} refused; OOM reported; {} busy failures, {} leaked frees",
        ROUNDS,
        OPERATIONS,
        allocations,
        stats.mapped_bytes / 1024,
        stats.free_blocks,
        stats.heap.peak_used_bytes / 1024,
        stats.growths,
        stats.refused_growths,
        stats.busy_failures,
        stats.leaked_frees
    );
    c.check(
        "no busy failures or leaked frees",
        stats.busy_failures == 0 && stats.leaked_frees == 0,
    );
}

/// Allocate every free frame, check that mapping and heap growth fail
/// cleanly while memory is exhausted, then return everything.
fn exhaustion(c: &mut Checker) {
    let before = super::free_frames();
    // Chain the frames through their first word: no extra memory needed.
    let mut head: u64 = 0;
    let mut count = 0u64;
    let exhausted = loop {
        match super::allocate_frame() {
            Ok(frame) => {
                // SAFETY: just allocated, owned by this test.
                let _ = unsafe { super::write_frame_word(frame, head) };
                head = frame.start().as_u64();
                count += 1;
            }
            Err(err) => break err,
        }
    };
    c.check(
        "allocation until exhausted",
        exhausted == MemError::Frames(FrameError::Exhausted) && count == before,
    );
    c.check("no frame left", super::free_frames() == 0);
    c.check(
        "allocate on empty reports exhaustion",
        super::allocate_frame() == Err(MemError::Frames(FrameError::Exhausted)),
    );
    match super::reserve_scratch(1) {
        Ok(page) => {
            let result = super::map_new(page, Flags::DATA);
            c.check(
                "mapping without frames fails cleanly",
                result == Err(MemError::Frames(FrameError::Exhausted))
                    && super::translate(page.start().as_u64()).is_none(),
            );
        }
        Err(_) => {
            c.check("scratch space reserved", false);
        }
    }
    let largest = super::heap::stats().map_or(0, |s| s.largest_free_block);
    let mut big: Vec<u8> = Vec::new();
    c.check(
        "heap growth refused without frames",
        big.try_reserve_exact(largest + 64 * 1024).is_err(),
    );
    let small = Box::new([1u8; 64]);
    c.check("heap serves from free space without frames", small[63] == 1);
    drop(small);

    // Return the chain.
    let mut returned = 0u64;
    let mut next = head;
    while next != 0 {
        let Some(frame) = frame(next) else { break };
        // SAFETY: a frame of the chain, still owned by this test.
        next = unsafe { super::read_frame_word(frame) }.unwrap_or(0);
        if super::free_frame(frame).is_ok() {
            returned += 1;
        }
    }
    c.check(
        "all frames returned",
        returned == count && super::free_frames() == before,
    );
    kprintln!(
        "[rust-os] memory self-test: exhaustion: {} frames ({} MiB) allocated until exhausted; \
         mapping and heap growth refused, small heap allocations still served; all returned",
        count,
        count * PAGE_SIZE / (1024 * 1024)
    );
}
