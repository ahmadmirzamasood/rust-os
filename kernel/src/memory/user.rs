//! Process address spaces and access to user memory.
//!
//! # Layout
//!
//! Every process has its own PML4. Entries 256..512 (the upper, kernel
//! half) are copied from the kernel's PML4 when the address space is
//! created, so every address space maps the kernel identically, and none of
//! those mappings has the user bit: ring 3 cannot touch them. This relies on
//! one invariant: after [`super::init`] the kernel never adds or removes an
//! upper-half PML4 entry (it only edits tables below its own, already
//! present, slot), so the copies never go stale. The bootloader is told to
//! put all of its dynamic mappings in the upper half for the same reason.
//!
//! Entries 0..256 (the lower half) belong to the process alone and only
//! ever contain user mappings, created by [`map_page`]. [`destroy`] frees
//! every frame and table under them, then the PML4 itself.
//!
//! # Copying to and from user memory
//!
//! The kernel never dereferences a user virtual address. [`copy_from`] and
//! [`copy_to`] translate each page through the *process's* page tables,
//! require it to be present and user-accessible (and writable for
//! [`copy_to`]), and copy through the physical-memory window. Hence:
//!
//! - a kernel, non-canonical, unmapped, overflowing or read-only range is
//!   rejected before any byte is copied ([`check`]), never faulted on;
//! - SMAP can stay enabled (no `stac`/`clac` needed);
//! - it works whichever address space is loaded in CR3.
//!
//! Processes are single-threaded and nothing unmaps user pages while a
//! process lives, so a range checked at the start of a system call stays
//! valid until the call returns.

use mm::paging::{Mapper, TableAccess};
use mm::{PAGE_SIZE, Page, PhysAddr, PhysFrame, VirtAddr};
use os_abi::layout::USER_END;

use super::{Flags, MemError, Tables, with_manager};

/// PML4 entries of the user half.
const USER_SLOTS: core::ops::Range<usize> = 0..256;
/// PML4 entries of the kernel half.
const KERNEL_SLOTS: core::ops::Range<usize> = 256..512;

/// Physical address of the kernel's own PML4 (the one loaded at boot), for
/// kernel threads.
pub fn kernel_root() -> Result<PhysFrame, MemError> {
    with_manager(|m| m.root)
}

/// Create an empty address space: a new PML4 with the kernel half shared.
pub fn create() -> Result<PhysFrame, MemError> {
    with_manager(|m| {
        let root = m.frames.allocate().map_err(MemError::Frames)?;
        // SAFETY: just allocated, so owned exclusively by this function.
        unsafe { m.window.zero_frame(root) };
        for index in KERNEL_SLOTS {
            let entry = m.window.read(m.root, index);
            m.window.write(root, index, entry);
        }
        Ok(root)
    })?
}

/// Map a fresh zeroed frame at `page` in the address space `root`, user
/// accessible with `flags`' write/execute permissions. Returns the frame
/// so the caller can fill it with [`fill`].
pub fn map_page(root: PhysFrame, page: Page, flags: Flags) -> Result<PhysFrame, MemError> {
    if page.start().as_u64() >= USER_END {
        return Err(MemError::NotOwned);
    }
    with_manager(|m| {
        let frame = m.frames.allocate().map_err(MemError::Frames)?;
        // SAFETY: just allocated; zeroed so no stale data reaches the
        // process.
        unsafe { m.window.zero_frame(frame) };
        let mut mapper = Mapper::new(root, &mut m.window);
        match mapper.map(page, frame, flags.for_user(), &mut Tables(&mut m.frames)) {
            // Not the active address space while it is being built, and a
            // new mapping replaces nothing: no TLB entry can be stale.
            Ok(flush) => {
                flush.ignore();
                Ok(frame)
            }
            Err(err) => {
                let _ = m.frames.free(frame);
                Err(MemError::Map(err))
            }
        }
    })?
}

/// Copy `data` into a frame returned by [`map_page`], at byte `offset`.
pub fn fill(frame: PhysFrame, offset: usize, data: &[u8]) -> Result<(), MemError> {
    if offset
        .checked_add(data.len())
        .is_none_or(|end| end > PAGE_SIZE as usize)
    {
        return Err(MemError::NotOwned);
    }
    // SAFETY: user frames are only accessed by the CPU (through user
    // mappings) and by this module; no Rust reference aliases them.
    with_manager(|m| unsafe { m.window.write_bytes(frame, offset, data) })
}

/// Free everything in the lower half of `root`, then `root` itself.
/// Returns the number of frames freed.
///
/// `root` must not be loaded in CR3 (checked) and no thread may run in it
/// again.
pub fn destroy(root: PhysFrame) -> Result<u64, MemError> {
    if crate::arch::x86_64::read_cr3() & mm::paging::bits::ADDRESS == root.start().as_u64() {
        return Err(MemError::Busy);
    }
    with_manager(|m| {
        let mut first_error = None;
        let frames = &mut m.frames;
        let mut freed = Mapper::new(root, &mut m.window).release(USER_SLOTS, |frame| {
            if let Err(err) = frames.free(frame) {
                first_error.get_or_insert(err);
            }
        });
        match m.frames.free(root) {
            Ok(()) => freed += 1,
            Err(err) => {
                first_error.get_or_insert(err);
            }
        }
        match first_error {
            None => Ok(freed),
            Some(err) => Err(MemError::Frames(err)),
        }
    })?
}

/// Why a user range is not accessible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserAccessError {
    /// Wraps around, leaves the user half, or touches a page that is not
    /// mapped user-accessible (and writable, for writes).
    BadAddress,
    /// The memory manager is busy (only possible from exception context).
    Busy,
}

/// Check that `[addr, addr + len)` is user memory of `root`, writable if
/// `write`. An empty range is always valid.
pub fn check(root: PhysFrame, addr: u64, len: u64, write: bool) -> Result<(), UserAccessError> {
    walk(root, addr, len, write, |_, _, _| {})
}

/// Copy `out.len()` bytes from user address `addr` of `root` into `out`.
pub fn copy_from(root: PhysFrame, addr: u64, out: &mut [u8]) -> Result<(), UserAccessError> {
    let mut done = 0usize;
    walk(
        root,
        addr,
        out.len() as u64,
        false,
        |window, frame, (offset, len)| {
            // SAFETY: the frame is a present user page of `root` (checked by
            // `walk`); user pages are never referenced by Rust code.
            unsafe { window.read_bytes(frame, offset, &mut out[done..done + len]) };
            done += len;
        },
    )
}

/// Copy `data` to user address `addr` of `root` (which must be writable).
pub fn copy_to(root: PhysFrame, addr: u64, data: &[u8]) -> Result<(), UserAccessError> {
    let mut done = 0usize;
    walk(
        root,
        addr,
        data.len() as u64,
        true,
        |window, frame, (offset, len)| {
            // SAFETY: as in `copy_from`; the page is user-writable.
            unsafe { window.write_bytes(frame, offset, &data[done..done + len]) };
            done += len;
        },
    )
}

/// Validate the range page by page; for each page call `visit` with the
/// window, the backing frame and the `(offset, length)` inside it. Nothing
/// is visited unless the whole range is valid (two passes: check, then
/// visit).
fn walk(
    root: PhysFrame,
    addr: u64,
    len: u64,
    write: bool,
    mut visit: impl FnMut(&mut super::phys::PhysWindow, PhysFrame, (usize, usize)),
) -> Result<(), UserAccessError> {
    if len == 0 {
        return Ok(());
    }
    let end = addr
        .checked_add(len)
        .filter(|&end| end <= USER_END)
        .ok_or(UserAccessError::BadAddress)?;
    let result = with_manager(|m| {
        for pass in 0..2 {
            let mut at = addr;
            while at < end {
                let page_end = (at & !(PAGE_SIZE - 1)) + PAGE_SIZE;
                let chunk_end = page_end.min(end);
                let virt = VirtAddr::new(at).ok_or(UserAccessError::BadAddress)?;
                let t = Mapper::new(root, &mut m.window)
                    .translate(virt)
                    .ok_or(UserAccessError::BadAddress)?;
                if !t.user || (write && !t.writable) || t.page_size != PAGE_SIZE {
                    return Err(UserAccessError::BadAddress);
                }
                if pass == 1 {
                    let frame = PhysFrame::containing(
                        PhysAddr::new(t.phys.as_u64() & !(PAGE_SIZE - 1))
                            .ok_or(UserAccessError::BadAddress)?,
                    );
                    let offset = (at & (PAGE_SIZE - 1)) as usize;
                    visit(&mut m.window, frame, (offset, (chunk_end - at) as usize));
                }
                at = chunk_end;
            }
        }
        Ok(())
    });
    result.map_err(|_| UserAccessError::Busy)?
}
