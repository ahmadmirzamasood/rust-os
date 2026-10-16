//! x86_64 4-level page tables.
//!
//! [`Mapper`] walks and edits a page-table tree rooted at a PML4 frame. It
//! never dereferences physical memory itself: every entry read or write goes
//! through a [`TableAccess`] implementation. The kernel implements it over
//! the bootloader's physical-memory window; tests implement it over a map of
//! fake frames. All rules below are therefore host-tested.
//!
//! Rules enforced here:
//!
//! - **W^X:** a leaf is never both writable and executable
//!   ([`MapError::WriteExecute`]).
//! - **No silent replacement:** mapping an already-present page fails
//!   ([`MapError::AlreadyMapped`]); so does unmapping an absent one.
//! - **Huge pages are foreign:** the mapper only creates and edits 4 KiB
//!   leaves; it translates through 2 MiB / 1 GiB leaves but refuses to map
//!   or unmap inside them ([`MapError::HugePage`]).
//! - **Parents never widen permissions:** new intermediate tables are
//!   present + writable + executable (permissions are decided by leaves);
//!   an existing parent that is read-only or no-execute makes a
//!   writable/executable leaf beneath it fail ([`MapError::ParentRestricts`])
//!   rather than silently producing a weaker mapping.
//!
//! TLB maintenance is the caller's job. Every edit returns a `#[must_use]`
//! [`TlbFlush`] naming the page whose cached translation is now stale.
//! Intermediate tables are not freed when they become empty.

#![forbid(unsafe_code)]

use crate::addr::{PAGE_SIZE, Page, PhysAddr, PhysFrame, VirtAddr};

/// Entries per table.
pub const ENTRIES: usize = 512;

/// Page-table entry bits used by this kernel.
pub mod bits {
    pub const PRESENT: u64 = 1 << 0;
    pub const WRITABLE: u64 = 1 << 1;
    pub const USER: u64 = 1 << 2;
    pub const HUGE: u64 = 1 << 7;
    pub const GLOBAL: u64 = 1 << 8;
    pub const NO_EXECUTE: u64 = 1 << 63;
    /// Physical address bits 51..12.
    pub const ADDRESS: u64 = 0x000f_ffff_ffff_f000;
}

/// Read and write access to page-table entries stored in physical frames.
pub trait TableAccess {
    /// Entry `index` (< 512) of the table in `table`.
    fn read(&mut self, table: PhysFrame, index: usize) -> u64;
    /// Store `value` into entry `index` (< 512) of the table in `table`.
    fn write(&mut self, table: PhysFrame, index: usize, value: u64);
}

/// Somewhere to get frames for new intermediate tables.
pub trait FrameSource {
    fn allocate_frame(&mut self) -> Option<PhysFrame>;
}

/// Permissions of a leaf mapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Flags {
    pub writable: bool,
    pub executable: bool,
    /// Accessible from ring 3. Only ever set in a process's lower half.
    pub user: bool,
}

impl Flags {
    /// Read-only, no-execute.
    pub const READ: Self = Self {
        writable: false,
        executable: false,
        user: false,
    };
    /// Read/write, no-execute: data, stacks, heap.
    pub const DATA: Self = Self {
        writable: true,
        executable: false,
        user: false,
    };
    /// Read/execute, not writable: code.
    pub const CODE: Self = Self {
        writable: false,
        executable: true,
        user: false,
    };

    /// The same permissions, accessible from user mode.
    pub const fn for_user(self) -> Self {
        Self { user: true, ..self }
    }

    fn entry_bits(self) -> u64 {
        let mut bits = bits::PRESENT;
        if self.writable {
            bits |= bits::WRITABLE;
        }
        if !self.executable {
            bits |= bits::NO_EXECUTE;
        }
        if self.user {
            bits |= bits::USER;
        }
        bits
    }
}

/// Why a mapping operation failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapError {
    /// Requested writable and executable at once.
    WriteExecute,
    /// The page is already mapped.
    AlreadyMapped,
    /// The page is not mapped.
    NotMapped,
    /// A 2 MiB or 1 GiB mapping covers the page.
    HugePage,
    /// An existing parent entry forbids the requested permissions.
    ParentRestricts,
    /// No frame for a new intermediate table.
    OutOfFrames,
}

/// A page whose TLB entry must be invalidated after an edit.
#[must_use = "the TLB still caches the old translation until it is flushed"]
#[derive(Debug, PartialEq, Eq)]
pub struct TlbFlush(Page);

impl TlbFlush {
    pub fn page(&self) -> Page {
        self.0
    }

    /// Discard the obligation. Only correct for tables that are not loaded
    /// in CR3 (for example host models).
    pub fn ignore(self) {}
}

/// Effective permissions and target of a translation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Translation {
    pub phys: PhysAddr,
    /// Size of the leaf mapping: 4 KiB, 2 MiB or 1 GiB.
    pub page_size: u64,
    /// Writable at every level.
    pub writable: bool,
    /// Executable at every level (no NX bit anywhere on the path).
    pub executable: bool,
    pub user: bool,
}

/// One item reported by [`Mapper::walk`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WalkItem {
    /// A page-table frame (including the root).
    Table { frame: PhysFrame, level: u8 },
    /// A leaf mapping of `size` bytes at `virt`.
    Leaf {
        virt: VirtAddr,
        size: u64,
        translation: Translation,
    },
}

/// Editor for one page-table tree.
#[derive(Debug)]
pub struct Mapper<'a, A> {
    root: PhysFrame,
    access: &'a mut A,
}

/// Bytes covered by one entry at `level`.
const fn entry_span(level: u8) -> u64 {
    PAGE_SIZE << (9 * (level as u64 - 1))
}

fn entry_frame(entry: u64) -> PhysFrame {
    // ADDRESS keeps bits 51..12: page-aligned and below 2^52 by construction.
    PhysFrame::containing(PhysAddr::new(entry & bits::ADDRESS).expect("masked to 52 bits"))
}

impl<'a, A: TableAccess> Mapper<'a, A> {
    pub fn new(root: PhysFrame, access: &'a mut A) -> Self {
        Self { root, access }
    }

    pub fn root(&self) -> PhysFrame {
        self.root
    }

    /// Map `page` to `frame` with `flags`, creating intermediate tables from
    /// `frames` as needed.
    pub fn map(
        &mut self,
        page: Page,
        frame: PhysFrame,
        flags: Flags,
        frames: &mut impl FrameSource,
    ) -> Result<TlbFlush, MapError> {
        if flags.writable && flags.executable {
            return Err(MapError::WriteExecute);
        }
        let addr = page.start();
        let mut table = self.root;
        for level in (2..=4).rev() {
            let index = addr.table_index(level);
            let entry = self.access.read(table, index);
            table = if entry & bits::PRESENT == 0 {
                let new = frames.allocate_frame().ok_or(MapError::OutOfFrames)?;
                for i in 0..ENTRIES {
                    self.access.write(new, i, 0);
                }
                // A table created for a user page is user-accessible itself
                // (the leaf still decides); kernel tables never are.
                let user = if flags.user { bits::USER } else { 0 };
                self.access.write(
                    table,
                    index,
                    new.start().as_u64() | bits::PRESENT | bits::WRITABLE | user,
                );
                new
            } else if entry & bits::HUGE != 0 {
                return Err(MapError::HugePage);
            } else {
                if (flags.writable && entry & bits::WRITABLE == 0)
                    || (flags.executable && entry & bits::NO_EXECUTE != 0)
                    || (flags.user && entry & bits::USER == 0)
                {
                    return Err(MapError::ParentRestricts);
                }
                entry_frame(entry)
            };
        }
        let index = addr.table_index(1);
        if self.access.read(table, index) & bits::PRESENT != 0 {
            return Err(MapError::AlreadyMapped);
        }
        self.access
            .write(table, index, frame.start().as_u64() | flags.entry_bits());
        Ok(TlbFlush(page))
    }

    /// Remove the mapping of `page` and return the frame it pointed to. The
    /// frame is not freed; the caller owns it again.
    pub fn unmap(&mut self, page: Page) -> Result<(PhysFrame, TlbFlush), MapError> {
        let (table, index) = self.leaf_slot(page)?;
        let entry = self.access.read(table, index);
        self.access.write(table, index, 0);
        Ok((entry_frame(entry), TlbFlush(page)))
    }

    /// Change the permissions of an existing 4 KiB mapping.
    pub fn protect(&mut self, page: Page, flags: Flags) -> Result<TlbFlush, MapError> {
        if flags.writable && flags.executable {
            return Err(MapError::WriteExecute);
        }
        let (table, index) = self.leaf_slot(page)?;
        let entry = self.access.read(table, index);
        let keep = entry & bits::ADDRESS;
        self.access.write(table, index, keep | flags.entry_bits());
        Ok(TlbFlush(page))
    }

    /// Translate `addr`, combining permissions over every level.
    pub fn translate(&mut self, addr: VirtAddr) -> Option<Translation> {
        let mut table = self.root;
        let mut writable = true;
        let mut executable = true;
        let mut user = true;
        for level in (1..=4).rev() {
            let entry = self.access.read(table, addr.table_index(level));
            if entry & bits::PRESENT == 0 {
                return None;
            }
            writable &= entry & bits::WRITABLE != 0;
            executable &= entry & bits::NO_EXECUTE == 0;
            user &= entry & bits::USER != 0;
            let leaf = level == 1 || (entry & bits::HUGE != 0 && level <= 3);
            if leaf {
                let size = entry_span(level);
                let base = entry & bits::ADDRESS & !(size - 1);
                let phys = PhysAddr::new(base | (addr.as_u64() & (size - 1)))?;
                return Some(Translation {
                    phys,
                    page_size: size,
                    writable,
                    executable,
                    user,
                });
            }
            table = entry_frame(entry);
        }
        None
    }

    /// Visit every table frame and every leaf mapping reachable from the
    /// root, in address order. `skip_pml4(index)` excludes whole top-level
    /// slots from the walk (their PML4 entry itself is still part of the
    /// root table). Depth is bounded by the four paging levels, so a
    /// corrupted or self-referencing tree cannot loop forever.
    pub fn walk(&mut self, skip_pml4: impl Fn(usize) -> bool, mut visit: impl FnMut(WalkItem)) {
        visit(WalkItem::Table {
            frame: self.root,
            level: 4,
        });
        for index in 0..ENTRIES {
            if skip_pml4(index) {
                continue;
            }
            // Upper-half addresses sign-extend from bit 47.
            let raw = (index as u64) << 39;
            let base = if index >= 256 {
                raw | 0xffff_0000_0000_0000
            } else {
                raw
            };
            let base = VirtAddr::new(base).expect("sign-extended PML4 base is canonical");
            let entry = self.access.read(self.root, index);
            self.walk_entry(entry, 4, base, true, true, true, &mut visit);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn walk_entry(
        &mut self,
        entry: u64,
        level: u8,
        base: VirtAddr,
        writable: bool,
        executable: bool,
        user: bool,
        visit: &mut impl FnMut(WalkItem),
    ) {
        if entry & bits::PRESENT == 0 {
            return;
        }
        let writable = writable && entry & bits::WRITABLE != 0;
        let executable = executable && entry & bits::NO_EXECUTE == 0;
        let user = user && entry & bits::USER != 0;
        if level == 1 || (entry & bits::HUGE != 0 && level <= 3) {
            let size = entry_span(level);
            let phys = entry & bits::ADDRESS & !(size - 1);
            if let Some(phys) = PhysAddr::new(phys) {
                visit(WalkItem::Leaf {
                    virt: base,
                    size,
                    translation: Translation {
                        phys,
                        page_size: size,
                        writable,
                        executable,
                        user,
                    },
                });
            }
            return;
        }
        let table = entry_frame(entry);
        visit(WalkItem::Table {
            frame: table,
            level: level - 1,
        });
        for index in 0..ENTRIES {
            let child = self.access.read(table, index);
            let Some(child_base) = base.checked_add(index as u64 * entry_span(level - 1)) else {
                break;
            };
            self.walk_entry(
                child,
                level - 1,
                child_base,
                writable,
                executable,
                user,
                visit,
            );
        }
    }

    /// Tear down everything mapped under the root entries `slots` (for a
    /// process: its whole lower half). `free` receives the frame of every
    /// 4 KiB leaf, then every table frame below the root; the root entries
    /// are cleared. Huge leaves are cleared without freeing their target
    /// (this mapper never creates them). The root frame itself is kept.
    ///
    /// No TLB flush is returned: only release a tree that is not loaded in
    /// CR3. Returns the number of frames passed to `free`.
    pub fn release(
        &mut self,
        slots: core::ops::Range<usize>,
        mut free: impl FnMut(PhysFrame),
    ) -> u64 {
        let mut freed = 0;
        for index in slots.start.min(ENTRIES)..slots.end.min(ENTRIES) {
            let entry = self.access.read(self.root, index);
            if entry & bits::PRESENT == 0 {
                continue;
            }
            if entry & bits::HUGE == 0 {
                let table = entry_frame(entry);
                freed += self.release_table(table, 3, &mut free);
                free(table);
                freed += 1;
            }
            self.access.write(self.root, index, 0);
        }
        freed
    }

    fn release_table(
        &mut self,
        table: PhysFrame,
        level: u8,
        free: &mut impl FnMut(PhysFrame),
    ) -> u64 {
        let mut freed = 0;
        for index in 0..ENTRIES {
            let entry = self.access.read(table, index);
            if entry & bits::PRESENT == 0 {
                continue;
            }
            if level == 1 {
                free(entry_frame(entry));
                freed += 1;
            } else if entry & bits::HUGE == 0 {
                let child = entry_frame(entry);
                freed += self.release_table(child, level - 1, free);
                free(child);
                freed += 1;
            }
        }
        freed
    }

    /// The level-1 table and index holding `page`'s present leaf.
    fn leaf_slot(&mut self, page: Page) -> Result<(PhysFrame, usize), MapError> {
        let addr = page.start();
        let mut table = self.root;
        for level in (2..=4).rev() {
            let entry = self.access.read(table, addr.table_index(level));
            if entry & bits::PRESENT == 0 {
                return Err(MapError::NotMapped);
            }
            if entry & bits::HUGE != 0 {
                return Err(MapError::HugePage);
            }
            table = entry_frame(entry);
        }
        let index = addr.table_index(1);
        if self.access.read(table, index) & bits::PRESENT == 0 {
            return Err(MapError::NotMapped);
        }
        Ok((table, index))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::vec::Vec;

    /// Fake physical memory: only frames that hold tables exist.
    #[derive(Default)]
    struct Model {
        tables: BTreeMap<u64, [u64; ENTRIES]>,
    }

    impl TableAccess for Model {
        fn read(&mut self, table: PhysFrame, index: usize) -> u64 {
            self.tables
                .entry(table.start().as_u64())
                .or_insert([0; ENTRIES])[index]
        }
        fn write(&mut self, table: PhysFrame, index: usize, value: u64) {
            self.tables
                .entry(table.start().as_u64())
                .or_insert([0; ENTRIES])[index] = value;
        }
    }

    struct Frames {
        next: u64,
        left: usize,
    }

    impl FrameSource for Frames {
        fn allocate_frame(&mut self) -> Option<PhysFrame> {
            if self.left == 0 {
                return None;
            }
            self.left -= 1;
            self.next += PAGE_SIZE;
            Some(frame(self.next))
        }
    }

    fn frame(addr: u64) -> PhysFrame {
        PhysFrame::from_start(PhysAddr::new(addr).unwrap()).unwrap()
    }

    fn page(addr: u64) -> Page {
        Page::from_start(VirtAddr::new(addr).unwrap()).unwrap()
    }

    const ROOT: u64 = 0x10_0000;
    const KERNEL: u64 = 0xffff_9000_0000_0000;

    fn frames(left: usize) -> Frames {
        Frames {
            next: 0x20_0000,
            left,
        }
    }

    #[test]
    fn map_translate_unmap() {
        let mut model = Model::default();
        let mut mapper = Mapper::new(frame(ROOT), &mut model);
        let mut src = frames(16);
        let p = page(KERNEL + 0x5000);
        let f = frame(0x80_0000);
        assert_eq!(mapper.map(p, f, Flags::DATA, &mut src).unwrap().page(), p);
        // Three intermediate tables were created (PDPT, PD, PT).
        assert_eq!(src.left, 13);
        let t = mapper
            .translate(VirtAddr::new(KERNEL + 0x5123).unwrap())
            .unwrap();
        assert_eq!(t.phys.as_u64(), 0x80_0123);
        assert_eq!(t.page_size, PAGE_SIZE);
        assert!(t.writable && !t.executable && !t.user);
        // Neighbouring page in the same table needs no new tables.
        mapper
            .map(
                page(KERNEL + 0x6000),
                frame(0x90_0000),
                Flags::CODE,
                &mut src,
            )
            .unwrap()
            .ignore();
        assert_eq!(src.left, 13);
        let code = mapper
            .translate(VirtAddr::new(KERNEL + 0x6000).unwrap())
            .unwrap();
        assert!(!code.writable && code.executable);

        let (back, flush) = mapper.unmap(p).unwrap();
        assert_eq!((back, flush.page()), (f, p));
        assert_eq!(mapper.translate(p.start()), None);
        assert_eq!(mapper.unmap(p), Err(MapError::NotMapped));
        // The other page is untouched.
        assert!(
            mapper
                .translate(VirtAddr::new(KERNEL + 0x6000).unwrap())
                .is_some()
        );
    }

    #[test]
    fn refuses_write_execute_and_double_map() {
        let mut model = Model::default();
        let mut mapper = Mapper::new(frame(ROOT), &mut model);
        let mut src = frames(16);
        let p = page(KERNEL);
        let wx = Flags {
            writable: true,
            executable: true,
            user: false,
        };
        assert_eq!(
            mapper.map(p, frame(0x80_0000), wx, &mut src),
            Err(MapError::WriteExecute)
        );
        mapper
            .map(p, frame(0x80_0000), Flags::DATA, &mut src)
            .unwrap()
            .ignore();
        assert_eq!(
            mapper.map(p, frame(0x81_0000), Flags::DATA, &mut src),
            Err(MapError::AlreadyMapped)
        );
        assert_eq!(mapper.protect(p, wx), Err(MapError::WriteExecute));
        // The original mapping survived both refusals.
        assert_eq!(
            mapper.translate(p.start()).unwrap().phys.as_u64(),
            0x80_0000
        );
    }

    #[test]
    fn protect_changes_permissions_only() {
        let mut model = Model::default();
        let mut mapper = Mapper::new(frame(ROOT), &mut model);
        let mut src = frames(16);
        let p = page(KERNEL);
        mapper
            .map(p, frame(0x80_0000), Flags::DATA, &mut src)
            .unwrap()
            .ignore();
        mapper.protect(p, Flags::READ).unwrap().ignore();
        let t = mapper.translate(p.start()).unwrap();
        assert!(!t.writable && !t.executable);
        assert_eq!(t.phys.as_u64(), 0x80_0000);
        assert_eq!(
            mapper.protect(page(KERNEL + PAGE_SIZE), Flags::READ),
            Err(MapError::NotMapped)
        );
    }

    #[test]
    fn running_out_of_table_frames_fails_cleanly() {
        let mut model = Model::default();
        let mut mapper = Mapper::new(frame(ROOT), &mut model);
        let mut src = frames(2);
        let p = page(KERNEL);
        assert_eq!(
            mapper.map(p, frame(0x80_0000), Flags::DATA, &mut src),
            Err(MapError::OutOfFrames)
        );
        assert_eq!(mapper.translate(p.start()), None);
        // With frames available again the same page maps fine (the partial
        // tables are reused, so only one more frame is needed).
        let mut more = frames(1);
        more.next = 0x40_0000;
        mapper
            .map(p, frame(0x80_0000), Flags::DATA, &mut more)
            .unwrap()
            .ignore();
        assert_eq!(more.left, 0);
    }

    #[test]
    fn huge_pages_translate_but_are_not_edited() {
        let mut model = Model::default();
        // PML4[0] -> PDPT at 0x1000; PDPT[1] = 1 GiB page at 0x4000_0000 (RW);
        // PDPT[0] -> PD at 0x2000; PD[3] = 2 MiB page at 0x60_0000 (RO, NX).
        model.write(frame(ROOT), 0, 0x1000 | bits::PRESENT | bits::WRITABLE);
        model.write(
            frame(0x1000),
            1,
            0x4000_0000 | bits::PRESENT | bits::WRITABLE | bits::HUGE,
        );
        model.write(frame(0x1000), 0, 0x2000 | bits::PRESENT | bits::WRITABLE);
        model.write(
            frame(0x2000),
            3,
            0x60_0000 | bits::PRESENT | bits::HUGE | bits::NO_EXECUTE,
        );
        let mut mapper = Mapper::new(frame(ROOT), &mut model);
        let giant = mapper
            .translate(VirtAddr::new(0x4123_4567).unwrap())
            .unwrap();
        assert_eq!(giant.phys.as_u64(), 0x4123_4567);
        assert_eq!(giant.page_size, 1 << 30);
        let big = mapper.translate(VirtAddr::new(0x65_4321).unwrap()).unwrap();
        assert_eq!(big.phys.as_u64(), 0x65_4321);
        assert_eq!(big.page_size, 2 << 20);
        assert!(!big.writable && !big.executable);
        let mut src = frames(4);
        assert_eq!(
            mapper.map(page(0x60_0000), frame(0x80_0000), Flags::DATA, &mut src),
            Err(MapError::HugePage)
        );
        assert_eq!(mapper.unmap(page(0x4000_0000)), Err(MapError::HugePage));
    }

    #[test]
    fn restrictive_parents_are_respected() {
        let mut model = Model::default();
        // PML4[0] present but read-only and NX.
        model.write(frame(ROOT), 0, 0x1000 | bits::PRESENT | bits::NO_EXECUTE);
        let mut mapper = Mapper::new(frame(ROOT), &mut model);
        let mut src = frames(4);
        assert_eq!(
            mapper.map(page(0x5000), frame(0x80_0000), Flags::DATA, &mut src),
            Err(MapError::ParentRestricts)
        );
        assert_eq!(
            mapper.map(page(0x5000), frame(0x80_0000), Flags::CODE, &mut src),
            Err(MapError::ParentRestricts)
        );
        mapper
            .map(page(0x5000), frame(0x80_0000), Flags::READ, &mut src)
            .unwrap()
            .ignore();
        let t = mapper.translate(VirtAddr::new(0x5000).unwrap()).unwrap();
        assert!(!t.writable && !t.executable);
    }

    #[test]
    fn user_pages_get_user_parents_and_kernel_parents_refuse_them() {
        let mut model = Model::default();
        let mut mapper = Mapper::new(frame(ROOT), &mut model);
        let mut src = frames(16);
        mapper
            .map(
                page(0x40_0000),
                frame(0x80_0000),
                Flags::CODE.for_user(),
                &mut src,
            )
            .unwrap()
            .ignore();
        let t = mapper.translate(VirtAddr::new(0x40_0000).unwrap()).unwrap();
        assert!(t.user && t.executable && !t.writable);
        // Kernel page next to it: parents are user, the leaf is not.
        mapper
            .map(page(0x40_1000), frame(0x81_0000), Flags::DATA, &mut src)
            .unwrap()
            .ignore();
        assert!(
            !mapper
                .translate(VirtAddr::new(0x40_1000).unwrap())
                .unwrap()
                .user
        );
        // A user page under kernel-only parents is refused.
        mapper
            .map(page(KERNEL), frame(0x82_0000), Flags::DATA, &mut src)
            .unwrap()
            .ignore();
        assert_eq!(
            mapper.map(
                page(KERNEL + PAGE_SIZE),
                frame(0x83_0000),
                Flags::DATA.for_user(),
                &mut src
            ),
            Err(MapError::ParentRestricts)
        );
        // W^X holds for user pages too.
        let wx = Flags {
            writable: true,
            executable: true,
            user: true,
        };
        assert_eq!(
            mapper.map(page(0x40_2000), frame(0x84_0000), wx, &mut src),
            Err(MapError::WriteExecute)
        );
    }

    #[test]
    fn release_frees_every_frame_in_the_slots_only() {
        let mut model = Model::default();
        let mut mapper = Mapper::new(frame(ROOT), &mut model);
        let mut src = frames(64);
        let mut leaves = Vec::new();
        // Pages in two different PML4 slots of the lower half and across
        // two page tables, plus one kernel page that must survive.
        for (i, addr) in [0x40_0000u64, 0x40_1000, 0x60_0000, 0x80_0000_0000]
            .into_iter()
            .enumerate()
        {
            let f = frame(0x100_0000 + i as u64 * PAGE_SIZE);
            mapper
                .map(page(addr), f, Flags::DATA.for_user(), &mut src)
                .unwrap()
                .ignore();
            leaves.push(f);
        }
        mapper
            .map(page(KERNEL), frame(0x200_0000), Flags::DATA, &mut src)
            .unwrap()
            .ignore();
        let tables_before = 64 - src.left;
        let mut freed = Vec::new();
        let count = mapper.release(0..256, |f| freed.push(f));
        assert_eq!(count as usize, freed.len());
        for leaf in &leaves {
            assert!(freed.contains(leaf), "leaf {leaf:?} freed");
        }
        // Tables: slot 0 has PDPT, PD, 2 PTs; slot 1 has PDPT, PD, PT.
        assert_eq!(freed.len(), leaves.len() + 7);
        assert_eq!(tables_before, 7 + 3);
        freed.sort();
        freed.dedup();
        assert_eq!(freed.len(), leaves.len() + 7, "nothing freed twice");
        assert_eq!(mapper.translate(VirtAddr::new(0x40_0000).unwrap()), None);
        assert!(mapper.translate(VirtAddr::new(KERNEL).unwrap()).is_some());
        assert!(!freed.contains(&frame(0x200_0000)));
        // Releasing again finds nothing.
        assert_eq!(mapper.release(0..256, |_| panic!("already freed")), 0);
    }

    #[test]
    fn walk_reports_tables_and_leaves() {
        let mut model = Model::default();
        let mut mapper = Mapper::new(frame(ROOT), &mut model);
        let mut src = frames(16);
        mapper
            .map(page(0x7000), frame(0x80_0000), Flags::CODE, &mut src)
            .unwrap()
            .ignore();
        mapper
            .map(page(KERNEL), frame(0x81_0000), Flags::DATA, &mut src)
            .unwrap()
            .ignore();
        let mut tables = Vec::new();
        let mut leaves = Vec::new();
        mapper.walk(
            |_| false,
            |item| match item {
                WalkItem::Table { frame, .. } => tables.push(frame.start().as_u64()),
                WalkItem::Leaf {
                    virt, translation, ..
                } => leaves.push((
                    virt.as_u64(),
                    translation.phys.as_u64(),
                    translation.writable,
                )),
            },
        );
        assert_eq!(tables.len(), 1 + 3 + 3);
        assert_eq!(tables[0], ROOT);
        assert_eq!(
            leaves,
            [(0x7000, 0x80_0000, false), (KERNEL, 0x81_0000, true)]
        );
        // Skipping the upper half hides the kernel page and its tables.
        let mut count = 0;
        mapper.walk(|i| i >= 256, |_| count += 1);
        assert_eq!(count, 1 + 3 + 1);
    }
}
