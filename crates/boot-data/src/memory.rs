//! Validation and preservation of the bootloader's physical memory map.
//!
//! The map handed over in `BootInfo` lives in memory the bootloader
//! allocated, which later phases will want to reclaim. [`MemoryMap::load`]
//! copies it into fixed-capacity, kernel-owned storage (no heap), sorts it by
//! address and checks it before anything trusts it.

use bootloader_api::info::{MemoryRegion, MemoryRegionKind};

/// Size of the frames future allocators will hand out.
pub const PAGE_SIZE: u64 = 4096;

/// Totals derived from a validated memory map.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemorySummary {
    /// Regions in the map, including empty ones.
    pub regions: usize,
    /// Regions with `start == end`; ignored.
    pub empty_regions: usize,
    pub usable_regions: usize,
    pub usable_bytes: u64,
    /// Memory the bootloader still uses (page tables, boot info, kernel
    /// stack). Must not be reused until those are no longer needed.
    pub bootloader_bytes: u64,
    /// Exclusive end of the highest region.
    pub highest_address: u64,
}

/// Why a memory map was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryMapError {
    /// The map has no entries.
    Empty,
    /// More entries than the kernel's fixed storage can hold.
    TooManyRegions { count: usize, capacity: usize },
    /// A region ends before it starts.
    InvertedRange { start: u64, end: u64 },
    /// A usable region is not aligned to [`PAGE_SIZE`].
    UnalignedUsable { start: u64, end: u64 },
    /// Two non-empty regions overlap; `at` is the start of the later one.
    Overlap { at: u64 },
    /// No usable memory at all.
    NoUsableMemory,
}

/// A validated, address-sorted copy of the memory map with room for `N`
/// regions.
#[derive(Clone, Debug)]
pub struct MemoryMap<const N: usize> {
    regions: [MemoryRegion; N],
    len: usize,
}

impl<const N: usize> MemoryMap<N> {
    pub const fn new() -> Self {
        Self {
            regions: [MemoryRegion::empty(); N],
            len: 0,
        }
    }

    /// Copy `source`, sort it by start address and validate it. On error the
    /// stored map is left empty, so nothing can use a partially checked map.
    pub fn load(&mut self, source: &[MemoryRegion]) -> Result<MemorySummary, MemoryMapError> {
        self.len = 0;
        if source.is_empty() {
            return Err(MemoryMapError::Empty);
        }
        if source.len() > N {
            return Err(MemoryMapError::TooManyRegions {
                count: source.len(),
                capacity: N,
            });
        }
        let copy = &mut self.regions[..source.len()];
        copy.copy_from_slice(source);
        // Insertion sort: no allocation, and the input is usually sorted.
        for i in 1..copy.len() {
            let mut j = i;
            while j > 0 && copy[j - 1].start > copy[j].start {
                copy.swap(j - 1, j);
                j -= 1;
            }
        }
        let summary = summarize(copy)?;
        self.len = source.len();
        Ok(summary)
    }

    /// The validated regions in address order (empty until a successful
    /// [`Self::load`]).
    pub fn regions(&self) -> &[MemoryRegion] {
        &self.regions[..self.len]
    }
}

impl<const N: usize> Default for MemoryMap<N> {
    fn default() -> Self {
        Self::new()
    }
}

/// Validate address-sorted regions and total them.
fn summarize(sorted: &[MemoryRegion]) -> Result<MemorySummary, MemoryMapError> {
    let mut summary = MemorySummary {
        regions: sorted.len(),
        ..MemorySummary::default()
    };
    let mut previous_end: Option<u64> = None;
    for region in sorted {
        let (start, end) = (region.start, region.end);
        if end < start {
            return Err(MemoryMapError::InvertedRange { start, end });
        }
        if end == start {
            summary.empty_regions += 1;
            continue;
        }
        if previous_end.is_some_and(|prev| start < prev) {
            return Err(MemoryMapError::Overlap { at: start });
        }
        previous_end = Some(end);
        summary.highest_address = summary.highest_address.max(end);
        let len = end - start;
        match region.kind {
            MemoryRegionKind::Usable => {
                if start % PAGE_SIZE != 0 || end % PAGE_SIZE != 0 {
                    return Err(MemoryMapError::UnalignedUsable { start, end });
                }
                summary.usable_regions += 1;
                // Sorted, non-overlapping regions within u64 cannot sum past
                // u64::MAX, but saturate rather than trust that reasoning.
                summary.usable_bytes = summary.usable_bytes.saturating_add(len);
            }
            MemoryRegionKind::Bootloader => {
                summary.bootloader_bytes = summary.bootloader_bytes.saturating_add(len);
            }
            _ => {}
        }
    }
    if summary.usable_bytes == 0 {
        return Err(MemoryMapError::NoUsableMemory);
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn region(start: u64, end: u64, kind: MemoryRegionKind) -> MemoryRegion {
        MemoryRegion { start, end, kind }
    }
    use MemoryRegionKind::{Bootloader, Usable};

    #[test]
    fn sorts_copies_and_totals() {
        let source = [
            region(0x10_0000, 0x20_0000, Usable),
            region(0x0, 0x1000, MemoryRegionKind::UnknownUefi(0)),
            region(0x1000, 0x9_f000, Usable),
            region(0x20_0000, 0x20_4000, Bootloader),
            region(0x30_0000, 0x30_0000, Usable),
        ];
        let mut map = MemoryMap::<8>::new();
        let summary = map.load(&source).unwrap();
        assert_eq!(
            summary,
            MemorySummary {
                regions: 5,
                empty_regions: 1,
                usable_regions: 2,
                usable_bytes: 0x9_e000 + 0x10_0000,
                bootloader_bytes: 0x4000,
                highest_address: 0x20_4000,
            }
        );
        let starts: [u64; 5] = core::array::from_fn(|i| map.regions()[i].start);
        assert_eq!(starts, [0, 0x1000, 0x10_0000, 0x20_0000, 0x30_0000]);
    }

    #[test]
    fn rejects_bad_maps_and_leaves_copy_empty() {
        let mut map = MemoryMap::<4>::new();
        assert_eq!(map.load(&[]), Err(MemoryMapError::Empty));
        let too_many = [region(0, 0x1000, Usable); 5];
        assert_eq!(
            map.load(&too_many),
            Err(MemoryMapError::TooManyRegions {
                count: 5,
                capacity: 4
            })
        );
        assert_eq!(
            map.load(&[region(0x2000, 0x1000, Usable)]),
            Err(MemoryMapError::InvertedRange {
                start: 0x2000,
                end: 0x1000
            })
        );
        assert_eq!(
            map.load(&[region(0x1000, 0x1800, Usable)]),
            Err(MemoryMapError::UnalignedUsable {
                start: 0x1000,
                end: 0x1800
            })
        );
        assert_eq!(
            map.load(&[
                region(0, 0x3000, Usable),
                region(0x2000, 0x4000, Bootloader)
            ]),
            Err(MemoryMapError::Overlap { at: 0x2000 })
        );
        assert_eq!(
            map.load(&[region(0, 0x3000, Bootloader)]),
            Err(MemoryMapError::NoUsableMemory)
        );
        assert!(map.regions().is_empty());
    }

    #[test]
    fn unaligned_reserved_regions_are_allowed() {
        let mut map = MemoryMap::<4>::new();
        let source = [
            region(0x0, 0x1000, Usable),
            region(0x1000, 0x1234, MemoryRegionKind::UnknownUefi(11)),
        ];
        assert!(map.load(&source).is_ok());
        assert_eq!(map.regions().len(), 2);
    }
}
