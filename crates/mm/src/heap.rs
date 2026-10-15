//! Reclaiming heap: address-ordered first-fit free list with coalescing.
//!
//! The heap manages one contiguous byte span `[start, end)` that may grow at
//! its end ([`FreeListHeap::extend`]). Free space is a singly linked list of
//! [`FreeBlock`] headers stored *inside* the free space itself, sorted by
//! address, so adjacent free blocks are merged on every free and the list
//! never holds two touching blocks.
//!
//! # Guarantees
//!
//! - **Granularity and alignment:** every block size is a multiple of
//!   [`MIN_ALIGN`] (16 bytes) and at least [`MIN_ALIGN`]; every returned
//!   pointer is aligned to `max(layout.align(), 16)`. Any power-of-two
//!   alignment is supported (padding before an over-aligned block stays a
//!   free block, since it is a multiple of 16).
//! - **Zero-sized requests** get a distinct 16-byte block, so the result is
//!   always a real, unique pointer. (Rust's `GlobalAlloc` never asks for
//!   zero bytes; this keeps the core total.)
//! - **No headers on live blocks:** `deallocate` takes the same layout as
//!   `allocate`, exactly as `GlobalAlloc` provides.
//! - **Failure is a value:** exhaustion returns [`AllocError`]; the heap is
//!   unchanged and keeps working. Size arithmetic is overflow-checked.
//! - **Misuse detection (best effort):** freeing a pointer outside the heap,
//!   misaligned, or overlapping free space (double free) is rejected with
//!   [`FreeError`] and changes nothing. Freeing a live block with the wrong
//!   size cannot always be detected.
//! - Freed memory is not cleared; never assume fresh memory is zeroed.
//!
//! # Safety model
//!
//! The heap owns its span exclusively from [`FreeListHeap::init`] on. It
//! reads and writes free-block headers only inside the span, at addresses
//! that are 16-aligned and at least 16 bytes before the span's end, so every
//! header access is in bounds and aligned for `FreeBlock` (16 bytes, align
//! 8). Live blocks are never read or written by the heap.

use core::alloc::Layout;
use core::ptr::{self, NonNull};

/// Minimum alignment and size granularity of every block.
pub const MIN_ALIGN: usize = 16;

/// Header written at the start of every free block.
#[repr(C)]
struct FreeBlock {
    /// Size of the free block in bytes (multiple of `MIN_ALIGN`).
    size: usize,
    /// Next free block at a higher address, or null.
    next: *mut FreeBlock,
}

const _: () = assert!(core::mem::size_of::<FreeBlock>() <= MIN_ALIGN);
const _: () = assert!(core::mem::align_of::<FreeBlock>() <= MIN_ALIGN);

/// The heap could not satisfy a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AllocError;

/// A deallocation was rejected; the heap is unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FreeError {
    /// The block is not inside the heap span.
    OutOfBounds { addr: usize, size: usize },
    /// The pointer is not aligned to [`MIN_ALIGN`] or the layout's alignment.
    Misaligned { addr: usize },
    /// The block overlaps memory that is already free.
    DoubleFree { addr: usize, size: usize },
}

/// Counters for diagnostics and leak checks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HeapStats {
    /// Total span size.
    pub span_bytes: usize,
    /// Bytes in live blocks (rounded sizes).
    pub used_bytes: usize,
    /// Highest `used_bytes` seen.
    pub peak_used_bytes: usize,
    /// Live allocations.
    pub live_blocks: usize,
    pub total_allocations: u64,
    pub failed_allocations: u64,
}

/// Shape of the free list, computed on demand.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FreeListShape {
    pub free_blocks: usize,
    pub free_bytes: usize,
    pub largest_free_block: usize,
}

/// See the module documentation.
#[derive(Debug)]
pub struct FreeListHeap {
    start: usize,
    end: usize,
    head: *mut FreeBlock,
    stats: HeapStats,
}

// SAFETY: the heap exclusively owns its span (contract of `init`/`extend`);
// the raw pointers point only into that span, so moving the heap to another
// context moves that ownership with it.
unsafe impl Send for FreeListHeap {}

impl Default for FreeListHeap {
    fn default() -> Self {
        Self::new()
    }
}

/// Round `size` up to the block granularity; `None` on overflow.
fn block_size(size: usize) -> Option<usize> {
    let size = size.max(MIN_ALIGN);
    size.checked_add(MIN_ALIGN - 1)
        .map(|s| s & !(MIN_ALIGN - 1))
}

impl FreeListHeap {
    /// An empty heap; every allocation fails until [`Self::init`].
    pub const fn new() -> Self {
        Self {
            start: 0,
            end: 0,
            head: ptr::null_mut(),
            stats: HeapStats {
                span_bytes: 0,
                used_bytes: 0,
                peak_used_bytes: 0,
                live_blocks: 0,
                total_allocations: 0,
                failed_allocations: 0,
            },
        }
    }

    /// Give the heap the span `[start, start + len)`. The span is shrunk
    /// inward to [`MIN_ALIGN`] boundaries. Does nothing if the heap already
    /// has a span or the shrunk span is empty.
    ///
    /// # Safety
    ///
    /// The span must be valid for reads and writes, must not be used by
    /// anything else for as long as the heap (or any block it returned) is
    /// alive, and must not wrap around the address space.
    pub unsafe fn init(&mut self, start: *mut u8, len: usize) {
        if self.end != 0 {
            return;
        }
        let addr = start as usize;
        let Some(first) = addr
            .checked_add(MIN_ALIGN - 1)
            .map(|a| a & !(MIN_ALIGN - 1))
        else {
            return;
        };
        let Some(last) = addr.checked_add(len).map(|e| e & !(MIN_ALIGN - 1)) else {
            return;
        };
        if first >= last {
            return;
        }
        self.start = first;
        self.end = first;
        // `start` is a pointer to the span; derive the block pointer from it
        // to keep its provenance.
        let offset = first - addr;
        // SAFETY: forwarded from this function's contract: the shrunk span
        // `[first, last)` lies inside the caller's span.
        unsafe { self.add_free_tail(start.wrapping_add(offset), last - first) };
    }

    /// Grow the span by `len` bytes at its current end. `len` is rounded
    /// down to [`MIN_ALIGN`]. Returns the number of bytes added.
    ///
    /// # Safety
    ///
    /// `[end, end + len)` must satisfy the [`Self::init`] contract, where
    /// `end` is [`Self::end`] and `tail` points to it (with provenance for
    /// the new bytes).
    pub unsafe fn extend(&mut self, tail: *mut u8, len: usize) -> usize {
        let len = len & !(MIN_ALIGN - 1);
        if self.end == 0 || tail as usize != self.end || len == 0 {
            return 0;
        }
        if self.end.checked_add(len).is_none() {
            return 0;
        }
        // SAFETY: forwarded from this function's contract.
        unsafe { self.add_free_tail(tail, len) };
        len
    }

    /// Exclusive end of the span (0 before `init`).
    pub fn end(&self) -> usize {
        self.end
    }

    pub fn stats(&self) -> HeapStats {
        self.stats
    }

    /// Whether `[addr, addr + size)` lies inside the span.
    pub fn contains(&self, addr: usize, size: usize) -> bool {
        addr >= self.start && addr.checked_add(size).is_some_and(|e| e <= self.end)
    }

    /// Allocate a block for `layout`.
    pub fn allocate(&mut self, layout: Layout) -> Result<NonNull<u8>, AllocError> {
        let result = self.find_fit(layout);
        match result {
            Ok((ptr, size)) => {
                self.stats.used_bytes += size;
                self.stats.peak_used_bytes = self.stats.peak_used_bytes.max(self.stats.used_bytes);
                self.stats.live_blocks += 1;
                self.stats.total_allocations += 1;
                Ok(ptr)
            }
            Err(err) => {
                self.stats.failed_allocations += 1;
                Err(err)
            }
        }
    }

    fn find_fit(&mut self, layout: Layout) -> Result<(NonNull<u8>, usize), AllocError> {
        let size = block_size(layout.size()).ok_or(AllocError)?;
        let align = layout.align().max(MIN_ALIGN);
        // The block before `cur` in the list (null: `cur` is the head).
        let mut prev: *mut FreeBlock = ptr::null_mut();
        let mut cur = self.head;
        while !cur.is_null() {
            // SAFETY: `cur` is a free-block header written by this heap
            // inside its span: aligned and in bounds (module safety model).
            let header = unsafe { cur.read() };
            let block = cur as usize;
            let block_end = block + header.size;
            let fit = block
                .checked_add(align - 1)
                .map(|a| a & !(align - 1))
                .and_then(|a| a.checked_add(size).map(|e| (a, e)));
            if let Some((addr, end)) = fit
                && end <= block_end
            {
                // Both remainders are multiples of MIN_ALIGN, because block,
                // addr, size and block_end all are; so each is either empty
                // or a valid free block.
                let front = addr - block;
                let back = block_end - end;
                let mut rest = header.next;
                if back > 0 {
                    let tail = cur
                        .cast::<u8>()
                        .wrapping_add(end - block)
                        .cast::<FreeBlock>();
                    // SAFETY: `[end, block_end)` is free, inside the span,
                    // 16-aligned and at least 16 bytes long.
                    unsafe {
                        tail.write(FreeBlock {
                            size: back,
                            next: rest,
                        })
                    };
                    rest = tail;
                }
                if front > 0 {
                    // SAFETY: `cur` stays the header of the front remainder.
                    unsafe {
                        cur.write(FreeBlock {
                            size: front,
                            next: rest,
                        })
                    };
                } else {
                    self.set_next(prev, rest);
                }
                let ptr = cur.cast::<u8>().wrapping_add(front);
                return NonNull::new(ptr).map(|p| (p, size)).ok_or(AllocError);
            }
            prev = cur;
            cur = header.next;
        }
        Err(AllocError)
    }

    /// Make `next` follow `prev` in the list (`prev` null: make it the head).
    fn set_next(&mut self, prev: *mut FreeBlock, next: *mut FreeBlock) {
        if prev.is_null() {
            self.head = next;
        } else {
            // SAFETY: `prev` is a valid free-block header (callers pass only
            // headers taken from the list).
            let size = unsafe { prev.read() }.size;
            // SAFETY: as above; rewriting the whole header keeps its size.
            unsafe { prev.write(FreeBlock { size, next }) };
        }
    }

    /// Return a block to the heap.
    ///
    /// # Safety
    ///
    /// `ptr` must have been returned by [`Self::allocate`] on this heap with
    /// the same `layout` and not freed since. Violations that the heap can
    /// detect are reported as [`FreeError`] without changing anything; others
    /// (such as a wrong size on a live block) corrupt the heap.
    pub unsafe fn deallocate(&mut self, ptr: NonNull<u8>, layout: Layout) -> Result<(), FreeError> {
        let addr = ptr.as_ptr() as usize;
        let size = block_size(layout.size()).ok_or(FreeError::OutOfBounds {
            addr,
            size: layout.size(),
        })?;
        if !addr.is_multiple_of(MIN_ALIGN) || !addr.is_multiple_of(layout.align()) {
            return Err(FreeError::Misaligned { addr });
        }
        if !self.contains(addr, size) {
            return Err(FreeError::OutOfBounds { addr, size });
        }
        let end = addr + size;

        // Find the neighbours: `prev` is the last free block below `addr`,
        // `next` the first one at or above it.
        let mut prev: *mut FreeBlock = ptr::null_mut();
        let mut prev_size = 0;
        let mut next = self.head;
        while !next.is_null() && (next as usize) < addr {
            prev = next;
            // SAFETY: `next` is a free-list header (module safety model).
            let header = unsafe { next.read() };
            prev_size = header.size;
            next = header.next;
        }
        let prev_end = (!prev.is_null()).then(|| prev as usize + prev_size);
        if prev_end.is_some_and(|pe| pe > addr) || (!next.is_null() && (next as usize) < end) {
            return Err(FreeError::DoubleFree { addr, size });
        }
        // Absorb `next` if it starts right where this block ends.
        let (size_with_next, link_after) = if !next.is_null() && next as usize == end {
            // SAFETY: `next` is a free-list header.
            let header = unsafe { next.read() };
            (size + header.size, header.next)
        } else {
            (size, next)
        };

        self.stats.used_bytes -= size;
        self.stats.live_blocks -= 1;
        if prev_end == Some(addr) {
            // Grow the previous block over this one (and maybe `next`).
            let merged = FreeBlock {
                size: prev_size + size_with_next,
                next: link_after,
            };
            // SAFETY: `prev` is a valid header; the merged block covers only
            // `prev`, this block and possibly `next`, all owned by the heap.
            unsafe { prev.write(merged) };
        } else {
            let block = ptr.as_ptr().cast::<FreeBlock>();
            let header = FreeBlock {
                size: size_with_next,
                next: link_after,
            };
            // SAFETY: `[addr, end)` is a block this heap handed out
            // (contract) and does not overlap free space (checked), so it
            // may hold a header: 16-aligned, at least 16 bytes.
            unsafe { block.write(header) };
            self.set_next(prev, block);
        }
        Ok(())
    }

    /// Walk the free list and summarize it.
    pub fn shape(&self) -> FreeListShape {
        let mut shape = FreeListShape::default();
        let mut cur = self.head;
        while !cur.is_null() {
            // SAFETY: free-list headers are valid (module safety model).
            let FreeBlock { size, next } = unsafe { cur.read() };
            shape.free_blocks += 1;
            shape.free_bytes += size;
            shape.largest_free_block = shape.largest_free_block.max(size);
            cur = next;
        }
        shape
    }

    /// Check the free list's structural invariants: blocks inside the span,
    /// aligned, sized in granules, strictly ascending, never touching, and
    /// accounting for exactly the bytes not in use.
    pub fn check(&self) -> Result<(), &'static str> {
        let mut cur = self.head;
        let mut last_end: Option<usize> = None;
        let mut free = 0usize;
        while !cur.is_null() {
            let addr = cur as usize;
            if !addr.is_multiple_of(MIN_ALIGN) || !self.contains(addr, MIN_ALIGN) {
                return Err("free block header misplaced");
            }
            // SAFETY: header address checked to be in bounds and aligned.
            let FreeBlock { size, next } = unsafe { cur.read() };
            if size < MIN_ALIGN || !size.is_multiple_of(MIN_ALIGN) || !self.contains(addr, size) {
                return Err("free block size invalid");
            }
            if last_end.is_some_and(|e| addr <= e) {
                return Err("free blocks unsorted, overlapping or not coalesced");
            }
            last_end = Some(addr + size);
            free += size;
            cur = next;
        }
        if free + self.stats.used_bytes != self.stats.span_bytes {
            return Err("free + used bytes do not add up to the span");
        }
        Ok(())
    }

    /// Append `[tail, tail + len)` (16-aligned, len a multiple of 16) as free
    /// space at the end of the span, merging with a free block that ends at
    /// the old end.
    ///
    /// # Safety
    ///
    /// As [`Self::init`] for the new bytes; `tail as usize == self.end`.
    unsafe fn add_free_tail(&mut self, tail: *mut u8, len: usize) {
        let addr = tail as usize;
        debug_assert_eq!(addr, self.end);
        self.end = addr + len;
        self.stats.span_bytes += len;
        // Find the last free block.
        let mut last: *mut FreeBlock = ptr::null_mut();
        let mut last_size = 0;
        let mut cur = self.head;
        while !cur.is_null() {
            last = cur;
            // SAFETY: free-list headers are valid (module safety model).
            let header = unsafe { cur.read() };
            last_size = header.size;
            cur = header.next;
        }
        if !last.is_null() && last as usize + last_size == addr {
            let grown = FreeBlock {
                size: last_size + len,
                next: ptr::null_mut(),
            };
            // SAFETY: `last` is a valid header; the new bytes are owned by
            // the heap now (contract) and directly follow it.
            unsafe { last.write(grown) };
        } else {
            let block = tail.cast::<FreeBlock>();
            let header = FreeBlock {
                size: len,
                next: ptr::null_mut(),
            };
            // SAFETY: the new bytes are owned by the heap (contract),
            // 16-aligned and at least 16 bytes long.
            unsafe { block.write(header) };
            self.set_next(last, block);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;
    use std::vec::Vec;

    /// Backing memory for a test heap: `u128` gives 16-byte alignment.
    struct Arena {
        memory: Vec<u128>,
    }

    impl Arena {
        fn new(bytes: usize) -> Self {
            Self {
                memory: vec![0; bytes / 16],
            }
        }
        fn heap(&mut self, bytes: usize) -> FreeListHeap {
            assert!(bytes <= self.memory.len() * 16);
            let mut heap = FreeListHeap::new();
            // SAFETY: the arena outlives the heap in every test and nothing
            // else touches it while the heap is in use.
            unsafe { heap.init(self.memory.as_mut_ptr().cast(), bytes) };
            heap
        }
        fn tail(&mut self, offset: usize) -> *mut u8 {
            self.memory.as_mut_ptr().cast::<u8>().wrapping_add(offset)
        }
    }

    /// Free through the unsafe API. Every call in these tests passes either a
    /// block allocated with `l` and not yet freed, or a deliberate misuse the
    /// heap must reject before touching memory.
    fn free(heap: &mut FreeListHeap, p: NonNull<u8>, l: Layout) -> Result<(), FreeError> {
        // SAFETY: see above.
        unsafe { heap.deallocate(p, l) }
    }

    fn layout(size: usize, align: usize) -> Layout {
        Layout::from_size_align(size, align).unwrap()
    }

    /// Deterministic xorshift for stress tests.
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

    #[test]
    fn alignment_is_guaranteed_for_all_powers_of_two() {
        let mut arena = Arena::new(1 << 20);
        let mut heap = arena.heap(1 << 20);
        let mut live = Vec::new();
        for shift in 0..=12 {
            let align = 1usize << shift;
            for size in [0, 1, 7, 16, 17, 100, 4096] {
                let l = layout(size, align);
                let p = heap.allocate(l).unwrap();
                assert_eq!(p.as_ptr() as usize % align.max(MIN_ALIGN), 0, "{l:?}");
                live.push((p, l));
            }
        }
        heap.check().unwrap();
        for (p, l) in live {
            free(&mut heap, p, l).unwrap();
        }
        heap.check().unwrap();
        assert_eq!(heap.shape().free_blocks, 1);
        assert_eq!(heap.stats().used_bytes, 0);
    }

    #[test]
    fn blocks_never_overlap_and_contents_survive() {
        let mut arena = Arena::new(256 * 1024);
        let mut heap = arena.heap(256 * 1024);
        let mut blocks: Vec<(NonNull<u8>, Layout, u8)> = Vec::new();
        for i in 0..200usize {
            let l = layout(1 + i * 13 % 700, 1 << (i % 7));
            let p = heap.allocate(l).unwrap();
            let tag = i as u8;
            // SAFETY: `p` is a fresh block of `l.size()` bytes owned by us.
            unsafe { p.as_ptr().write_bytes(tag, l.size()) };
            blocks.push((p, l, tag));
        }
        let mut ranges: Vec<(usize, usize)> = blocks
            .iter()
            .map(|(p, l, _)| (p.as_ptr() as usize, p.as_ptr() as usize + l.size().max(1)))
            .collect();
        ranges.sort();
        for pair in ranges.windows(2) {
            assert!(pair[0].1 <= pair[1].0, "overlap: {pair:?}");
        }
        for (p, l, tag) in &blocks {
            // SAFETY: the block is live and `l.size()` bytes long.
            let bytes = unsafe { core::slice::from_raw_parts(p.as_ptr(), l.size()) };
            assert!(bytes.iter().all(|b| b == tag));
        }
    }

    #[test]
    fn free_coalesces_and_memory_is_reused() {
        let mut arena = Arena::new(4096);
        let mut heap = arena.heap(4096);
        let l = layout(1024, 16);
        let a = heap.allocate(l).unwrap();
        let b = heap.allocate(l).unwrap();
        let c = heap.allocate(l).unwrap();
        let d = heap.allocate(l).unwrap();
        assert_eq!(heap.allocate(layout(16, 16)), Err(AllocError));
        // Free in an order that exercises merge-with-prev, -next and both.
        free(&mut heap, b, l).unwrap();
        free(&mut heap, d, l).unwrap();
        assert_eq!(heap.shape().free_blocks, 2);
        free(&mut heap, c, l).unwrap();
        assert_eq!(heap.shape().free_blocks, 1);
        free(&mut heap, a, l).unwrap();
        let shape = heap.shape();
        assert_eq!((shape.free_blocks, shape.largest_free_block), (1, 4096));
        // The whole span is one block again: a 4 KiB request fits.
        let all = heap.allocate(layout(4096, 16)).unwrap();
        assert_eq!(all, a);
        heap.check().unwrap();
    }

    #[test]
    fn exhaustion_is_reported_and_harmless() {
        let mut arena = Arena::new(8192);
        let mut heap = arena.heap(8192);
        assert_eq!(heap.allocate(layout(8193, 16)), Err(AllocError));
        assert_eq!(
            heap.allocate(layout(isize::MAX as usize - 64, 16)),
            Err(AllocError)
        );
        assert_eq!(heap.allocate(layout(16, 1 << 40)), Err(AllocError));
        let mut held = Vec::new();
        while let Ok(p) = heap.allocate(layout(100, 8)) {
            held.push(p);
        }
        assert_eq!(held.len(), 8192 / 112);
        assert_eq!(heap.stats().failed_allocations, 4);
        heap.check().unwrap();
        for p in held {
            free(&mut heap, p, layout(100, 8)).unwrap();
        }
        assert!(heap.allocate(layout(8192, 16)).is_ok());
    }

    #[test]
    fn invalid_frees_are_rejected_without_damage() {
        let mut arena = Arena::new(4096);
        let mut heap = arena.heap(4096);
        let l = layout(64, 16);
        let a = heap.allocate(l).unwrap();
        let b = heap.allocate(l).unwrap();
        free(&mut heap, a, l).unwrap();
        let addr = a.as_ptr() as usize;
        assert_eq!(
            free(&mut heap, a, l),
            Err(FreeError::DoubleFree { addr, size: 64 })
        );
        let inside = NonNull::new(a.as_ptr().wrapping_add(16)).unwrap();
        assert!(matches!(
            free(&mut heap, inside, layout(16, 16)),
            Err(FreeError::DoubleFree { .. })
        ));
        let odd = NonNull::new(b.as_ptr().wrapping_add(8)).unwrap();
        assert!(matches!(
            free(&mut heap, odd, layout(8, 8)),
            Err(FreeError::Misaligned { .. })
        ));
        let mut outside = [0u128; 4];
        let foreign = NonNull::new(outside.as_mut_ptr().cast::<u8>()).unwrap();
        assert!(matches!(
            free(&mut heap, foreign, l),
            Err(FreeError::OutOfBounds { .. })
        ));
        heap.check().unwrap();
        assert_eq!(heap.stats().live_blocks, 1);
    }

    #[test]
    fn heap_grows_contiguously() {
        let mut arena = Arena::new(8192);
        let mut heap = arena.heap(4096);
        let first = heap.allocate(layout(4096, 16)).unwrap();
        assert_eq!(heap.allocate(layout(2048, 16)), Err(AllocError));
        // Wrong tail address is refused.
        let wrong = arena.tail(4096 + 16);
        // SAFETY: refused before any access.
        assert_eq!(unsafe { heap.extend(wrong, 1024) }, 0);
        let tail = arena.tail(4096);
        // SAFETY: the arena's second half is unused and outlives the heap.
        assert_eq!(unsafe { heap.extend(tail, 4096) }, 4096);
        let second = heap.allocate(layout(4096, 16)).unwrap();
        assert_eq!(second.as_ptr(), tail);
        free(&mut heap, second, layout(4096, 16)).unwrap();
        free(&mut heap, first, layout(4096, 16)).unwrap();
        assert_eq!(heap.shape().largest_free_block, 8192);
        heap.check().unwrap();
    }

    #[test]
    fn randomized_stress_keeps_invariants() {
        const SLOTS: usize = 64;
        let mut arena = Arena::new(1 << 20);
        let mut heap = arena.heap(1 << 20);
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        let mut slots: Vec<Option<(NonNull<u8>, Layout, u8)>> = vec![None; SLOTS];
        for round in 0..20_000u32 {
            let i = rng.below(SLOTS as u64) as usize;
            if let Some((p, l, tag)) = slots[i].take() {
                // SAFETY: the block is live and `l.size()` bytes long.
                let bytes = unsafe { core::slice::from_raw_parts(p.as_ptr(), l.size()) };
                assert!(
                    bytes.iter().all(|b| *b == tag),
                    "corruption in round {round}"
                );

                free(&mut heap, p, l).unwrap();
            } else {
                let size = if rng.below(10) == 0 {
                    rng.below(32 * 1024)
                } else {
                    rng.below(512)
                };
                let l = layout(size as usize, 1 << rng.below(8));
                if let Ok(p) = heap.allocate(l) {
                    let tag = round as u8;
                    // SAFETY: fresh block of `l.size()` bytes.
                    unsafe { p.as_ptr().write_bytes(tag, l.size()) };
                    slots[i] = Some((p, l, tag));
                }
            }
            if round % 1000 == 0 {
                heap.check().unwrap();
            }
        }
        for (p, l, _) in slots.into_iter().flatten() {
            free(&mut heap, p, l).unwrap();
        }
        heap.check().unwrap();
        assert_eq!(heap.stats().used_bytes, 0);
        assert_eq!(heap.shape().free_blocks, 1);
    }
}
