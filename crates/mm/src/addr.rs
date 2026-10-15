//! Typed addresses.
//!
//! Physical and virtual addresses are distinct types so one cannot be passed
//! where the other is expected. Constructors validate: physical addresses fit
//! the 52-bit x86_64 limit, virtual addresses are canonical (48-bit, 4-level
//! paging), frames and pages are 4 KiB aligned. Arithmetic is checked.

#![forbid(unsafe_code)]

use core::fmt;

/// Size of a frame and of a (small) page.
pub const PAGE_SIZE: u64 = 4096;

/// Round `value` down to a multiple of `align` (a power of two).
pub const fn align_down(value: u64, align: u64) -> u64 {
    debug_assert!(align.is_power_of_two());
    value & !(align - 1)
}

/// Round `value` up to a multiple of `align` (a power of two); `None` on
/// overflow.
pub const fn align_up(value: u64, align: u64) -> Option<u64> {
    debug_assert!(align.is_power_of_two());
    match value.checked_add(align - 1) {
        Some(v) => Some(v & !(align - 1)),
        None => None,
    }
}

/// A physical address below 2^52.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[repr(transparent)]
pub struct PhysAddr(u64);

impl PhysAddr {
    /// Highest physical address x86_64 page tables can express, plus one.
    pub const LIMIT: u64 = 1 << 52;

    pub const fn new(addr: u64) -> Option<Self> {
        if addr < Self::LIMIT {
            Some(Self(addr))
        } else {
            None
        }
    }

    pub const fn as_u64(self) -> u64 {
        self.0
    }

    pub const fn is_aligned(self, align: u64) -> bool {
        self.0 & (align - 1) == 0
    }

    pub fn checked_add(self, bytes: u64) -> Option<Self> {
        self.0.checked_add(bytes).and_then(Self::new)
    }
}

impl fmt::Debug for PhysAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PhysAddr({:#x})", self.0)
    }
}

/// A canonical 48-bit virtual address: bits 63..47 all equal bit 47.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[repr(transparent)]
pub struct VirtAddr(u64);

impl VirtAddr {
    pub const fn new(addr: u64) -> Option<Self> {
        // Sign-extend from bit 47 and compare.
        let extended = ((addr << 16) as i64 >> 16) as u64;
        if extended == addr {
            Some(Self(addr))
        } else {
            None
        }
    }

    /// Address of a pointer. Pointers on x86_64 are always canonical.
    pub fn from_ptr<T: ?Sized>(ptr: *const T) -> Self {
        let addr = ptr as *const u8 as usize as u64;
        Self::new(addr).expect("x86_64 pointers are canonical")
    }

    pub const fn as_u64(self) -> u64 {
        self.0
    }

    pub const fn is_aligned(self, align: u64) -> bool {
        self.0 & (align - 1) == 0
    }

    /// Add `bytes`, failing on overflow or if the result is not canonical
    /// (the non-canonical hole cannot be crossed).
    pub fn checked_add(self, bytes: u64) -> Option<Self> {
        self.0.checked_add(bytes).and_then(Self::new)
    }

    pub fn checked_sub(self, bytes: u64) -> Option<Self> {
        self.0.checked_sub(bytes).and_then(Self::new)
    }

    /// Page-table index at `level` (4 = PML4 .. 1 = page table).
    pub const fn table_index(self, level: u8) -> usize {
        debug_assert!(level >= 1 && level <= 4);
        ((self.0 >> (12 + 9 * (level as u64 - 1))) & 0x1ff) as usize
    }

    pub const fn page_offset(self) -> u64 {
        self.0 & (PAGE_SIZE - 1)
    }
}

impl fmt::Debug for VirtAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VirtAddr({:#x})", self.0)
    }
}

/// A 4 KiB-aligned physical frame.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PhysFrame(PhysAddr);

impl PhysFrame {
    /// The frame starting at `addr`; `None` unless `addr` is page-aligned.
    pub const fn from_start(addr: PhysAddr) -> Option<Self> {
        if addr.is_aligned(PAGE_SIZE) {
            Some(Self(addr))
        } else {
            None
        }
    }

    pub const fn containing(addr: PhysAddr) -> Self {
        Self(PhysAddr(align_down(addr.0, PAGE_SIZE)))
    }

    pub const fn start(self) -> PhysAddr {
        self.0
    }

    /// Frame number (start / 4096).
    pub const fn number(self) -> u64 {
        self.0.0 / PAGE_SIZE
    }
}

impl fmt::Debug for PhysFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PhysFrame({:#x})", self.0.0)
    }
}

/// A 4 KiB-aligned virtual page.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Page(VirtAddr);

impl Page {
    pub const fn from_start(addr: VirtAddr) -> Option<Self> {
        if addr.is_aligned(PAGE_SIZE) {
            Some(Self(addr))
        } else {
            None
        }
    }

    pub const fn containing(addr: VirtAddr) -> Self {
        Self(VirtAddr(align_down(addr.0, PAGE_SIZE)))
    }

    pub const fn start(self) -> VirtAddr {
        self.0
    }

    /// The page `count` pages after this one, if still canonical.
    pub fn checked_add(self, count: u64) -> Option<Self> {
        let bytes = count.checked_mul(PAGE_SIZE)?;
        self.0.checked_add(bytes).map(Self)
    }
}

impl fmt::Debug for Page {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Page({:#x})", self.0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_virtual_addresses() {
        assert!(VirtAddr::new(0).is_some());
        assert!(VirtAddr::new(0x0000_7fff_ffff_ffff).is_some());
        assert!(VirtAddr::new(0xffff_8000_0000_0000).is_some());
        assert!(VirtAddr::new(0x0000_8000_0000_0000).is_none());
        assert!(VirtAddr::new(0xffff_7fff_ffff_ffff).is_none());
        // Crossing the hole is refused rather than wrapping silently.
        let top_low = VirtAddr::new(0x0000_7fff_ffff_f000).unwrap();
        assert!(top_low.checked_add(PAGE_SIZE).is_none());
        assert!(VirtAddr::new(u64::MAX).unwrap().checked_add(1).is_none());
    }

    #[test]
    fn physical_limit_and_alignment() {
        assert!(PhysAddr::new(PhysAddr::LIMIT - 1).is_some());
        assert!(PhysAddr::new(PhysAddr::LIMIT).is_none());
        assert!(PhysFrame::from_start(PhysAddr::new(0x1000).unwrap()).is_some());
        assert!(PhysFrame::from_start(PhysAddr::new(0x1001).unwrap()).is_none());
        let frame = PhysFrame::containing(PhysAddr::new(0x2fff).unwrap());
        assert_eq!(frame.start().as_u64(), 0x2000);
        assert_eq!(frame.number(), 2);
    }

    #[test]
    fn table_indices() {
        let addr = VirtAddr::new(0xffff_8123_4567_8abc).unwrap();
        assert_eq!(addr.table_index(4), 0x102);
        assert_eq!(addr.table_index(3), 0x8d);
        assert_eq!(addr.table_index(2), 0x2b);
        assert_eq!(addr.table_index(1), 0x78);
        assert_eq!(addr.page_offset(), 0xabc);
    }

    #[test]
    fn alignment_helpers() {
        assert_eq!(align_down(0x1fff, 0x1000), 0x1000);
        assert_eq!(align_up(0x1001, 0x1000), Some(0x2000));
        assert_eq!(align_up(0x1000, 0x1000), Some(0x1000));
        assert_eq!(align_up(u64::MAX, 0x1000), None);
    }
}
