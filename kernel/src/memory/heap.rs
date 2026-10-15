//! The kernel heap (`#[global_allocator]`).
//!
//! A [`mm::heap::FreeListHeap`] over the start of the kernel's own address
//! region. It begins with [`INITIAL_SIZE`] mapped and grows on demand, in
//! steps of at least [`GROWTH_STEP`], by mapping fresh frames at its end, up
//! to [`super::HEAP_MAX`]. It never shrinks: freed memory is reused by later
//! allocations but its frames stay mapped.
//!
//! # Alignment and sizes
//!
//! Every block is aligned to `max(layout.align(), 16)` and occupies a
//! multiple of 16 bytes. Any power-of-two alignment works.
//!
//! # Failure
//!
//! - Allocation never waits: if the heap lock is held (only possible when an
//!   exception or panic interrupts the allocator), the request fails.
//! - Growth is refused, without mapping anything, when the request cannot
//!   fit below `HEAP_MAX` or when fewer free frames remain than it needs plus
//!   a reserve for page tables. A huge request therefore fails fast and does
//!   not drain physical memory.
//! - A failed allocation returns null. Fallible APIs (`Vec::try_reserve`,
//!   ...) see an error and the kernel continues. Infallible ones (`Box::new`,
//!   `Vec::push`) call `handle_alloc_error`, which in `no_std` panics with
//!   "memory allocation of N bytes failed": a reported, predictable stop.
//! - A deallocation the heap rejects (pointer outside the heap, misaligned,
//!   double free) is heap corruption and panics with the details.
//! - Exception handlers and the panic handler never allocate.

use core::alloc::{GlobalAlloc, Layout};
use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicU64, Ordering};

use mm::PAGE_SIZE;
use mm::heap::{FreeListHeap, HeapStats};

use super::{Flags, HEAP_MAX, MemError};
use crate::sync::TryLock;

/// Bytes mapped when the heap is created.
pub const INITIAL_SIZE: u64 = 256 * 1024;
/// Minimum growth step.
pub const GROWTH_STEP: u64 = 64 * 1024;
/// Free frames kept back for page tables when growing.
const FRAME_RESERVE: u64 = 16;

struct State {
    heap: FreeListHeap,
    base: u64,
    mapped_end: u64,
    growths: u64,
    refused_growths: u64,
}

/// Allocations refused because the heap lock was held.
static BUSY_FAILURES: AtomicU64 = AtomicU64::new(0);
/// Frees dropped because the heap lock was held.
static LEAKED_FREES: AtomicU64 = AtomicU64::new(0);

static HEAP: TryLock<State> = TryLock::new(State {
    heap: FreeListHeap::new(),
    base: 0,
    mapped_end: 0,
    growths: 0,
    refused_growths: 0,
});

#[global_allocator]
static ALLOCATOR: KernelAllocator = KernelAllocator;

/// Heap counters for diagnostics and tests.
#[derive(Debug, Clone, Copy, Default)]
pub struct Stats {
    pub heap: HeapStats,
    pub mapped_bytes: u64,
    pub growths: u64,
    pub refused_growths: u64,
    pub busy_failures: u64,
    pub leaked_frees: u64,
    pub free_blocks: usize,
    pub largest_free_block: usize,
}

/// Map the initial heap at `base` (page-aligned, inside the kernel region).
pub(super) fn init(base: u64) -> Result<(), MemError> {
    let mut state = HEAP.try_lock().ok_or(MemError::Busy)?;
    if state.mapped_end != 0 {
        return Ok(());
    }
    state.base = base;
    state.mapped_end = base;
    let mapped = map_more(&mut state, INITIAL_SIZE / PAGE_SIZE)?;
    let start = ptr::with_exposed_provenance_mut::<u8>(base as usize);
    // SAFETY: `[base, base + mapped)` was just mapped writable for the heap
    // alone; it is never unmapped (the heap does not shrink).
    unsafe { state.heap.init(start, mapped as usize) };
    Ok(())
}

/// Map `pages` more pages at the heap's end. Returns bytes mapped; stops
/// early (returning what was mapped) if mapping fails part-way.
fn map_more(state: &mut State, pages: u64) -> Result<u64, MemError> {
    let mut mapped = 0;
    for _ in 0..pages {
        let page = super::page(state.mapped_end).ok_or(MemError::NotOwned)?;
        match super::map_new(page, Flags::DATA) {
            Ok(_) => {
                state.mapped_end += PAGE_SIZE;
                mapped += PAGE_SIZE;
            }
            Err(err) if mapped == 0 => return Err(err),
            Err(_) => break,
        }
    }
    Ok(mapped)
}

/// Try to make room for `layout`. Returns whether the heap grew.
fn grow(state: &mut State, layout: Layout) -> bool {
    if state.mapped_end == 0 {
        // Not initialized: there is no span to extend.
        return false;
    }
    // Worst case: the block plus alignment padding plus one granule.
    let needed = (layout.size() as u64)
        .checked_add(layout.align() as u64)
        .and_then(|n| n.checked_add(16))
        .map(|n| n.max(GROWTH_STEP));
    let Some(pages) = needed.map(|n| n.div_ceil(PAGE_SIZE)) else {
        state.refused_growths += 1;
        return false;
    };
    let fits_range = pages
        .checked_mul(PAGE_SIZE)
        .and_then(|bytes| state.mapped_end.checked_add(bytes))
        .is_some_and(|end| end <= state.base + HEAP_MAX);
    let fits_frames = super::free_frames() >= pages.saturating_add(FRAME_RESERVE);
    if !fits_range || !fits_frames {
        state.refused_growths += 1;
        return false;
    }
    let tail = ptr::with_exposed_provenance_mut::<u8>(state.mapped_end as usize);
    let Ok(mapped) = map_more(state, pages) else {
        state.refused_growths += 1;
        return false;
    };
    // SAFETY: the bytes after the old end were just mapped writable for the
    // heap alone and are never unmapped.
    let added = unsafe { state.heap.extend(tail, mapped as usize) };
    state.growths += 1;
    added > 0
}

/// Current heap counters (`None` if the heap is busy).
pub fn stats() -> Option<Stats> {
    let state = HEAP.try_lock()?;
    let shape = state.heap.shape();
    Some(Stats {
        heap: state.heap.stats(),
        mapped_bytes: state.mapped_end - state.base,
        growths: state.growths,
        refused_growths: state.refused_growths,
        busy_failures: BUSY_FAILURES.load(Ordering::Relaxed),
        leaked_frees: LEAKED_FREES.load(Ordering::Relaxed),
        free_blocks: shape.free_blocks,
        largest_free_block: shape.largest_free_block,
    })
}

/// Check the heap's free-list invariants.
pub fn check() -> Result<(), &'static str> {
    HEAP.try_lock().ok_or("heap busy")?.heap.check()
}

#[derive(Debug)]
struct KernelAllocator;

// SAFETY: `alloc` returns null or a block of at least `layout.size()` bytes
// aligned to `layout.align()` that no other live allocation overlaps (the
// free-list heap's guarantees, host-tested in `mm`). `dealloc` hands the
// block back with the same layout, as `GlobalAlloc` callers must.
unsafe impl GlobalAlloc for KernelAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let Some(mut state) = HEAP.try_lock() else {
            BUSY_FAILURES.fetch_add(1, Ordering::Relaxed);
            return ptr::null_mut();
        };
        if let Ok(block) = state.heap.allocate(layout) {
            return block.as_ptr();
        }
        if grow(&mut state, layout)
            && let Ok(block) = state.heap.allocate(layout)
        {
            return block.as_ptr();
        }
        ptr::null_mut()
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let Some(block) = NonNull::new(ptr) else {
            return;
        };
        let Some(mut state) = HEAP.try_lock() else {
            // Only reachable if an exception interrupted the allocator and
            // its handler freed memory, which handlers never do. Leak rather
            // than corrupt.
            LEAKED_FREES.fetch_add(1, Ordering::Relaxed);
            return;
        };
        // SAFETY: the `GlobalAlloc` contract: `ptr` came from `alloc` with
        // this `layout` and has not been freed.
        if let Err(err) = unsafe { state.heap.deallocate(block, layout) } {
            panic!("kernel heap corruption: rejected free: {err:?}");
        }
    }
}
