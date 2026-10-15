//! Loads a static ELF executable into a new address space.
//!
//! The file is validated by [`exec::elf::parse`] against the user image
//! range of [`os_abi::layout`] before anything is allocated. Each loadable
//! segment gets fresh zeroed frames mapped user-accessible with exactly the
//! segment's permissions (never writable and executable); its file bytes
//! are copied in through the physical-memory window, the rest stays zero
//! (`.bss`). The stack is [`os_abi::layout::STACK_SIZE`] of writable,
//! non-executable pages below `STACK_TOP`, with the page below left
//! unmapped as a guard. On any failure the partial address space is freed.

use exec::elf::{self, ElfError, Image, Limits};
use mm::{PAGE_SIZE, PhysFrame};
use os_abi::layout;

use crate::memory::{self, Flags, MemError, user};

const LIMITS: Limits = Limits {
    start: layout::IMAGE_START,
    end: layout::IMAGE_END,
    max_bytes: layout::IMAGE_MAX_BYTES,
};

/// A loaded program, ready for its first thread.
#[derive(Debug, Clone, Copy)]
pub struct Loaded {
    pub root: PhysFrame,
    pub entry: u64,
    /// Initial stack pointer (16-byte aligned minus 8, as after a `call`).
    pub stack_pointer: u64,
    pub segments: usize,
    /// Bytes mapped for segments (whole pages), excluding the stack.
    pub image_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadError {
    Elf(ElfError),
    Memory(MemError),
}

/// Validate `file` as a user executable without loading it.
pub fn check(file: &[u8]) -> Result<Image, ElfError> {
    elf::parse(file, &LIMITS)
}

pub fn load(file: &[u8]) -> Result<Loaded, LoadError> {
    let image = elf::parse(file, &LIMITS).map_err(LoadError::Elf)?;
    let root = user::create().map_err(LoadError::Memory)?;
    if let Err(err) = populate(root, file, &image) {
        // Not loaded anywhere yet; freeing cannot race with a user.
        let _ = user::destroy(root);
        return Err(LoadError::Memory(err));
    }
    Ok(Loaded {
        root,
        entry: image.entry,
        stack_pointer: layout::STACK_TOP - 8,
        segments: image.segments().len(),
        image_bytes: image.mapped_bytes(),
    })
}

fn populate(root: PhysFrame, file: &[u8], image: &Image) -> Result<(), MemError> {
    for s in image.segments() {
        let flags = Flags {
            writable: s.writable,
            executable: s.executable,
            user: true,
        };
        let file_end = s.vaddr + s.file_size;
        let mut page = s.page_start();
        while page < s.page_end() {
            let frame = user::map_page(root, memory::page(page).ok_or(MemError::NotOwned)?, flags)?;
            // File bytes that fall into this page (the parser checked that
            // the segment's file range lies inside `file`).
            let lo = page.max(s.vaddr);
            let hi = (page + PAGE_SIZE).min(file_end);
            if lo < hi {
                let from = (s.file_offset + (lo - s.vaddr)) as usize;
                let bytes = &file[from..from + (hi - lo) as usize];
                user::fill(frame, (lo - page) as usize, bytes)?;
            }
            page += PAGE_SIZE;
        }
    }
    let mut page = layout::STACK_TOP - layout::STACK_SIZE;
    while page < layout::STACK_TOP {
        user::map_page(
            root,
            memory::page(page).ok_or(MemError::NotOwned)?,
            Flags::DATA,
        )?;
        page += PAGE_SIZE;
    }
    Ok(())
}
