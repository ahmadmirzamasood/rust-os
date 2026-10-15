//! Access to physical memory through the bootloader's physical-memory window.
//!
//! The bootloader maps all physical memory from 0 up to at least
//! `max(highest memory-map address, 4 GiB)` at a fixed virtual offset,
//! writable and non-executable (requested in the kernel's
//! `BootloaderConfig`). This module is the only code that turns physical
//! addresses into pointers, and only for three purposes: page-table
//! entries, whole frames the memory manager owns, and copies to and from
//! user pages (after the caller checked the user mapping). General kernel
//! code never gets raw physical access.

use mm::PhysFrame;
use mm::paging::{ENTRIES, TableAccess};

use mm::PAGE_SIZE;

/// The physical-memory window: `[0, limit)` is mapped at `offset`.
#[derive(Debug)]
pub struct PhysWindow {
    offset: u64,
    limit: u64,
}

impl PhysWindow {
    /// Describe the window the bootloader created.
    ///
    /// # Safety
    ///
    /// `[offset, offset + limit)` must map physical `[0, limit)` writable
    /// for the rest of the kernel's life, and `offset + limit` must not
    /// overflow. Nothing may hold Rust references into memory reached
    /// through the window while this module accesses it (page tables and
    /// frames owned by the memory manager are only reached here).
    pub unsafe fn new(offset: u64, limit: u64) -> Self {
        Self { offset, limit }
    }

    /// Virtual address of physical `[phys, phys + len)`, checked to lie in
    /// the window.
    fn virt(&self, phys: u64, len: u64) -> u64 {
        let end = phys.checked_add(len).expect("physical range overflows");
        assert!(
            end <= self.limit,
            "physical address {phys:#x} outside the window"
        );
        self.offset + phys
    }

    /// Virtual address at which `frame` is visible in the window.
    pub fn frame_virt(&self, frame: PhysFrame) -> u64 {
        self.virt(frame.start().as_u64(), PAGE_SIZE)
    }

    pub fn offset(&self) -> u64 {
        self.offset
    }

    pub fn limit(&self) -> u64 {
        self.limit
    }

    /// Fill `frame` with zeros.
    ///
    /// # Safety
    ///
    /// The caller owns `frame` (allocated from the frame allocator and not
    /// in use by anything else).
    pub unsafe fn zero_frame(&mut self, frame: PhysFrame) {
        let ptr = core::ptr::with_exposed_provenance_mut::<u8>(self.frame_virt(frame) as usize);
        // SAFETY: the window maps the frame writable (constructor contract);
        // the caller owns it, so no other access overlaps.
        unsafe { ptr.write_bytes(0, PAGE_SIZE as usize) };
    }

    /// Store `value` in the first 8 bytes of `frame`.
    ///
    /// # Safety
    ///
    /// As [`Self::zero_frame`].
    pub unsafe fn write_u64(&mut self, frame: PhysFrame, value: u64) {
        let ptr = core::ptr::with_exposed_provenance_mut::<u64>(self.frame_virt(frame) as usize);
        // SAFETY: as in `zero_frame`; frames are 4 KiB aligned.
        unsafe { ptr.write_volatile(value) };
    }

    /// Load the first 8 bytes of `frame`.
    ///
    /// # Safety
    ///
    /// As [`Self::zero_frame`].
    pub unsafe fn read_u64(&mut self, frame: PhysFrame) -> u64 {
        let ptr = core::ptr::with_exposed_provenance::<u64>(self.frame_virt(frame) as usize);
        // SAFETY: as in `zero_frame`; frames are 4 KiB aligned.
        unsafe { ptr.read_volatile() }
    }

    /// Copy `data` into `frame` at byte `offset`.
    ///
    /// # Safety
    ///
    /// No Rust reference may alias the frame's bytes (true for frames the
    /// memory manager owns: page tables and user pages, which only the CPU
    /// and this module access). `offset + data.len()` must not exceed a
    /// page (asserted).
    pub unsafe fn write_bytes(&mut self, frame: PhysFrame, offset: usize, data: &[u8]) {
        assert!(
            offset + data.len() <= PAGE_SIZE as usize,
            "copy crosses a frame"
        );
        let base = self.frame_virt(frame) as usize + offset;
        let ptr = core::ptr::with_exposed_provenance_mut::<u8>(base);
        // SAFETY: inside the window (frame_virt checked) and inside the
        // frame (asserted); `data` is kernel memory, so no overlap.
        unsafe { ptr.copy_from_nonoverlapping(data.as_ptr(), data.len()) };
    }

    /// Copy bytes from `frame` at `offset` into `out`.
    ///
    /// # Safety
    ///
    /// As [`Self::write_bytes`].
    pub unsafe fn read_bytes(&mut self, frame: PhysFrame, offset: usize, out: &mut [u8]) {
        assert!(
            offset + out.len() <= PAGE_SIZE as usize,
            "copy crosses a frame"
        );
        let base = self.frame_virt(frame) as usize + offset;
        let ptr = core::ptr::with_exposed_provenance::<u8>(base);
        // SAFETY: as in `write_bytes`.
        unsafe { ptr.copy_to_nonoverlapping(out.as_mut_ptr(), out.len()) };
    }

    fn entry_ptr(&self, table: PhysFrame, index: usize) -> *mut u64 {
        assert!(index < ENTRIES, "page-table index out of range");
        let virt = self.frame_virt(table) + index as u64 * 8;
        core::ptr::with_exposed_provenance_mut(virt as usize)
    }
}

/// Page-table entries are read and written with volatile accesses: the CPU
/// itself updates accessed/dirty bits in them concurrently, so the compiler
/// must neither cache nor elide these accesses.
impl TableAccess for PhysWindow {
    fn read(&mut self, table: PhysFrame, index: usize) -> u64 {
        let ptr = self.entry_ptr(table, index);
        // SAFETY: `entry_ptr` checked the entry lies in the window, which
        // maps it (constructor contract); 8-byte aligned. Callers only pass
        // page-table frames, which no Rust reference aliases.
        unsafe { ptr.read_volatile() }
    }

    fn write(&mut self, table: PhysFrame, index: usize, value: u64) {
        let ptr = self.entry_ptr(table, index);
        // SAFETY: as in `read`. The memory manager is the only writer of
        // page tables and holds `&mut self` (behind its lock).
        unsafe { ptr.write_volatile(value) };
    }
}
