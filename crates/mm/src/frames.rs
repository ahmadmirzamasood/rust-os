//! Physical frame allocator.
//!
//! A bitmap over a contiguous span of frames, with two bits per frame:
//!
//! - **managed**: the allocator may ever hand this frame out. Set only for
//!   frames inside usable firmware regions, at or above the configured
//!   minimum address; cleared for good by [`FrameBitmap::reserve`].
//! - **free**: managed and not currently allocated.
//!
//! Invariant: `free ⊆ managed`. A frame that was never managed can neither be
//! allocated nor freed, so reserved memory (kernel image, page tables,
//! framebuffer, boot data, firmware regions) is never handed out, and a stray
//! free of such a frame is reported instead of corrupting the allocator.
//!
//! Allocation is next-fit over 64-frame words: O(1) amortized when memory is
//! plentiful, O(span / 64) worst case. Frame contents are never touched;
//! callers must not assume a newly allocated frame is zeroed.

#![forbid(unsafe_code)]

use crate::addr::{PAGE_SIZE, PhysAddr, PhysFrame, align_down, align_up};

/// Why a frame operation failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameError {
    /// No free frame is left.
    Exhausted,
    /// The frame is outside the tracked span or reserved; it was never the
    /// allocator's to hand out, so it cannot be freed either.
    NotManaged(PhysFrame),
    /// The frame is already free.
    DoubleFree(PhysFrame),
}

/// Why the allocator could not be built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameInitError {
    /// A usable range ends before it starts.
    InvertedRange { start: u64, end: u64 },
    /// The storage slice is empty or has odd length.
    BadStorage,
}

/// What [`FrameBitmap::new`] did with the usable ranges.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameInitReport {
    /// Frames that became allocatable.
    pub managed_frames: u64,
    /// Usable bytes below the minimum address (deliberately left alone).
    pub below_minimum_bytes: u64,
    /// Usable bytes beyond the bitmap's capacity (not tracked).
    pub beyond_capacity_bytes: u64,
    /// Usable bytes lost to page alignment at range edges.
    pub unaligned_bytes: u64,
}

/// Bitmap frame allocator over caller-provided storage.
#[derive(Debug)]
pub struct FrameBitmap<S> {
    storage: S,
    /// Words per bitmap; storage holds `managed` then `free`.
    words: usize,
    /// Physical address of frame index 0.
    base: u64,
    free_frames: u64,
    managed_frames: u64,
    /// Word index where the next allocation search starts.
    hint: usize,
}

impl<S: AsRef<[u64]> + AsMut<[u64]>> FrameBitmap<S> {
    /// Build the allocator.
    ///
    /// `storage` must have even length; half tracks "managed", half "free",
    /// so it covers `storage.len() / 2 * 64` frames starting at `minimum`
    /// (rounded up to a frame). `usable` lists `(start, end)` byte ranges the
    /// firmware reports as free RAM; they are shrunk inward to whole frames
    /// and clipped to the tracked span. Overlapping usable ranges are fine.
    /// The storage is cleared first, so its prior contents do not matter.
    pub fn new(
        mut storage: S,
        minimum: u64,
        usable: impl IntoIterator<Item = (u64, u64)>,
    ) -> Result<(Self, FrameInitReport), FrameInitError> {
        let len = storage.as_ref().len();
        if len == 0 || len % 2 != 0 {
            return Err(FrameInitError::BadStorage);
        }
        storage.as_mut().fill(0);
        let base = align_up(minimum, PAGE_SIZE).ok_or(FrameInitError::BadStorage)?;
        let words = len / 2;
        let capacity_end = base.saturating_add(words as u64 * 64 * PAGE_SIZE);

        let mut report = FrameInitReport::default();
        let mut bitmap = Self {
            storage,
            words,
            base,
            free_frames: 0,
            managed_frames: 0,
            hint: 0,
        };
        for (start, end) in usable {
            if end < start {
                return Err(FrameInitError::InvertedRange { start, end });
            }
            report.below_minimum_bytes += end.min(base).saturating_sub(start);
            report.beyond_capacity_bytes += end.saturating_sub(start.max(capacity_end));
            let (clip_start, clip_end) = (start.max(base), end.min(capacity_end));
            if clip_start >= clip_end {
                continue;
            }
            let first = align_up(clip_start, PAGE_SIZE).unwrap_or(u64::MAX);
            let last = align_down(clip_end, PAGE_SIZE);
            if first >= last {
                report.unaligned_bytes += clip_end - clip_start;
                continue;
            }
            report.unaligned_bytes += (first - clip_start) + (clip_end - last);
            let mut addr = first;
            while addr < last {
                let index = ((addr - base) / PAGE_SIZE) as usize;
                bitmap.set(index, true, true);
                addr += PAGE_SIZE;
            }
        }
        bitmap.recount();
        report.managed_frames = bitmap.managed_frames;
        Ok((bitmap, report))
    }

    /// Permanently withdraw every frame overlapping `[start, end)` from the
    /// allocator. Returns how many of those frames were free (and so would
    /// otherwise have been handed out). Reserving a currently allocated frame
    /// is allowed: freeing it later then fails with
    /// [`FrameError::NotManaged`].
    pub fn reserve(&mut self, start: u64, end: u64) -> u64 {
        if end <= start {
            return 0;
        }
        let first = align_down(start, PAGE_SIZE).max(self.base);
        let last = align_up(end, PAGE_SIZE).unwrap_or(u64::MAX);
        let mut withdrawn = 0;
        let mut addr = first;
        while addr < last {
            let Some(index) = self.index_of(addr) else {
                break;
            };
            if self.free_bit(index) {
                withdrawn += 1;
                self.free_frames -= 1;
            }
            if self.bit(index) {
                self.managed_frames -= 1;
            }
            self.set(index, false, false);
            addr += PAGE_SIZE;
        }
        withdrawn
    }

    /// Allocate one frame. Contents are unspecified.
    pub fn allocate(&mut self) -> Result<PhysFrame, FrameError> {
        if self.free_frames == 0 {
            return Err(FrameError::Exhausted);
        }
        for step in 0..self.words {
            let word = (self.hint + step) % self.words;
            let bits = self.storage.as_ref()[self.words + word];
            if bits != 0 {
                let index = word * 64 + bits.trailing_zeros() as usize;
                self.storage.as_mut()[self.words + word] &= !(1u64 << (index % 64));
                self.free_frames -= 1;
                self.hint = word;
                return Ok(self.frame_at(index));
            }
        }
        // free_frames said otherwise: the count and bitmap disagree. Repair
        // the count rather than looping forever.
        self.recount();
        Err(FrameError::Exhausted)
    }

    /// Return `frame` to the allocator.
    pub fn free(&mut self, frame: PhysFrame) -> Result<(), FrameError> {
        let index = self
            .index_of(frame.start().as_u64())
            .filter(|&i| self.bit(i))
            .ok_or(FrameError::NotManaged(frame))?;
        if self.free_bit(index) {
            return Err(FrameError::DoubleFree(frame));
        }
        self.storage.as_mut()[self.words + index / 64] |= 1u64 << (index % 64);
        self.free_frames += 1;
        Ok(())
    }

    /// Whether `frame` is managed and currently free.
    pub fn is_free(&self, frame: PhysFrame) -> bool {
        self.index_of(frame.start().as_u64())
            .is_some_and(|i| self.free_bit(i))
    }

    /// Whether `frame` may ever be handed out (free or allocated).
    pub fn is_managed(&self, frame: PhysFrame) -> bool {
        self.index_of(frame.start().as_u64())
            .is_some_and(|i| self.bit(i))
    }

    /// Whether any frame overlapping `[start, end)` is free.
    pub fn any_free_in(&self, start: u64, end: u64) -> bool {
        let mut addr = align_down(start, PAGE_SIZE).max(self.base);
        while addr < end {
            match self.index_of(addr) {
                Some(i) if self.free_bit(i) => return true,
                Some(_) => {}
                None => break,
            }
            addr += PAGE_SIZE;
        }
        false
    }

    pub fn free_frames(&self) -> u64 {
        self.free_frames
    }

    pub fn managed_frames(&self) -> u64 {
        self.managed_frames
    }

    /// Lowest address the bitmap can track.
    pub fn base(&self) -> u64 {
        self.base
    }

    /// Exclusive end of the trackable span.
    pub fn capacity_end(&self) -> u64 {
        self.base.saturating_add(self.words as u64 * 64 * PAGE_SIZE)
    }

    fn index_of(&self, addr: u64) -> Option<usize> {
        let offset = addr.checked_sub(self.base)?;
        let index = usize::try_from(offset / PAGE_SIZE).ok()?;
        (index < self.words * 64).then_some(index)
    }

    fn frame_at(&self, index: usize) -> PhysFrame {
        let addr = self.base + index as u64 * PAGE_SIZE;
        // `index` came from the bitmap, which only covers page-aligned
        // addresses below `capacity_end`; for any storage a machine can hold
        // these fit in 52 bits.
        PhysFrame::from_start(PhysAddr::new(addr).expect("tracked frame below 2^52"))
            .expect("tracked frames are page-aligned")
    }

    /// Managed bit of frame `index`.
    fn bit(&self, index: usize) -> bool {
        self.storage.as_ref()[index / 64] & (1u64 << (index % 64)) != 0
    }

    /// Free bit of frame `index`.
    fn free_bit(&self, index: usize) -> bool {
        self.storage.as_ref()[self.words + index / 64] & (1u64 << (index % 64)) != 0
    }

    fn set(&mut self, index: usize, managed: bool, free: bool) {
        let words = self.words;
        let mask = 1u64 << (index % 64);
        let storage = self.storage.as_mut();
        for (word, on) in [(index / 64, managed), (words + index / 64, free)] {
            if on {
                storage[word] |= mask;
            } else {
                storage[word] &= !mask;
            }
        }
    }

    fn recount(&mut self) {
        let storage = self.storage.as_ref();
        let (managed, free) = storage.split_at(self.words);
        self.managed_frames = managed.iter().map(|w| u64::from(w.count_ones())).sum();
        self.free_frames = free.iter().map(|w| u64::from(w.count_ones())).sum();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::vec;
    use std::vec::Vec;

    const MIB: u64 = 1024 * 1024;

    fn bitmap(
        words: usize,
        minimum: u64,
        usable: &[(u64, u64)],
    ) -> (FrameBitmap<Vec<u64>>, FrameInitReport) {
        FrameBitmap::new(
            vec![0xffff_ffff; words * 2],
            minimum,
            usable.iter().copied(),
        )
        .unwrap()
    }

    fn frame(addr: u64) -> PhysFrame {
        PhysFrame::from_start(PhysAddr::new(addr).unwrap()).unwrap()
    }

    #[test]
    fn only_usable_frames_above_minimum_are_managed() {
        let (b, report) = bitmap(
            8,
            MIB,
            &[
                (0x1000, 0x9_f000),
                (MIB, MIB + 0x5800),
                (2 * MIB + 0x10, 2 * MIB + 0x3000),
            ],
        );
        // [1M, 1M+0x5000) = 5 frames; [2M+0x1000, 2M+0x3000) = 2 frames.
        assert_eq!(report.managed_frames, 7);
        assert_eq!(b.free_frames(), 7);
        assert_eq!(report.below_minimum_bytes, 0x9_e000);
        assert_eq!(report.unaligned_bytes, 0x800 + (0x1000 - 0x10));
        assert!(!b.is_managed(frame(0x1000)));
        assert!(b.is_free(frame(MIB)));
        assert!(!b.is_free(frame(MIB + 0x5000)));
        assert!(!b.is_free(frame(2 * MIB)));
        assert!(b.is_free(frame(2 * MIB + 0x1000)));
    }

    #[test]
    fn capacity_is_reported_not_overrun() {
        // 1 word per bitmap = 64 frames = 256 KiB from 1 MiB.
        let (b, report) = bitmap(1, MIB, &[(MIB, 2 * MIB)]);
        assert_eq!(b.managed_frames(), 64);
        assert_eq!(report.beyond_capacity_bytes, MIB - 64 * PAGE_SIZE);
        assert_eq!(b.capacity_end(), MIB + 64 * PAGE_SIZE);
    }

    #[test]
    fn allocations_are_unique_aligned_and_inside_usable_memory() {
        let usable = [
            (MIB, MIB + 40 * PAGE_SIZE),
            (3 * MIB, 3 * MIB + 30 * PAGE_SIZE),
        ];
        let (mut b, _) = bitmap(16, MIB, &usable);
        let mut seen = BTreeSet::new();
        while let Ok(f) = b.allocate() {
            let addr = f.start().as_u64();
            assert_eq!(addr % PAGE_SIZE, 0);
            assert!(
                usable
                    .iter()
                    .any(|&(s, e)| addr >= s && addr + PAGE_SIZE <= e)
            );
            assert!(seen.insert(addr), "frame {addr:#x} handed out twice");
        }
        assert_eq!(seen.len(), 70);
        assert_eq!(b.allocate(), Err(FrameError::Exhausted));
        assert_eq!(b.free_frames(), 0);
    }

    #[test]
    fn free_and_reuse() {
        let (mut b, _) = bitmap(2, MIB, &[(MIB, MIB + 8 * PAGE_SIZE)]);
        let all: Vec<_> = (0..8).map(|_| b.allocate().unwrap()).collect();
        assert_eq!(b.allocate(), Err(FrameError::Exhausted));
        b.free(all[3]).unwrap();
        assert_eq!(b.free_frames(), 1);
        assert_eq!(b.allocate(), Ok(all[3]));
        for f in &all {
            b.free(*f).unwrap();
        }
        assert_eq!(b.free_frames(), 8);
        // Many rounds of allocate/free keep the counts exact.
        for _ in 0..100 {
            let a = b.allocate().unwrap();
            let c = b.allocate().unwrap();
            assert_ne!(a, c);
            b.free(a).unwrap();
            b.free(c).unwrap();
        }
        assert_eq!(b.free_frames(), 8);
    }

    #[test]
    fn invalid_frees_are_rejected() {
        let (mut b, _) = bitmap(2, MIB, &[(MIB, MIB + 4 * PAGE_SIZE)]);
        let f = b.allocate().unwrap();
        b.free(f).unwrap();
        assert_eq!(b.free(f), Err(FrameError::DoubleFree(f)));
        let outside = frame(0x5000);
        assert_eq!(b.free(outside), Err(FrameError::NotManaged(outside)));
        let beyond = frame(MIB + 4 * PAGE_SIZE);
        assert_eq!(b.free(beyond), Err(FrameError::NotManaged(beyond)));
        assert_eq!(b.free_frames(), 4);
    }

    #[test]
    fn reserved_frames_are_never_allocated_or_freed() {
        let (mut b, _) = bitmap(4, MIB, &[(MIB, MIB + 32 * PAGE_SIZE)]);
        // Reserve a range with unaligned edges: frames 2..=4 overlap it.
        let withdrawn = b.reserve(MIB + 2 * PAGE_SIZE + 8, MIB + 4 * PAGE_SIZE + 1);
        assert_eq!(withdrawn, 3);
        // Reserving an allocated frame withdraws it without counting it free.
        let held = b.allocate().unwrap();
        assert_eq!(
            b.reserve(held.start().as_u64(), held.start().as_u64() + 1),
            0
        );
        assert_eq!(b.free(held), Err(FrameError::NotManaged(held)));
        assert_eq!(b.managed_frames(), 28);
        let mut got = BTreeSet::new();
        while let Ok(f) = b.allocate() {
            got.insert(f.start().as_u64());
        }
        assert_eq!(got.len(), 28);
        for i in 2..=4 {
            assert!(!got.contains(&(MIB + i * PAGE_SIZE)));
        }
        assert!(!got.contains(&held.start().as_u64()));
        assert!(!b.any_free_in(MIB, MIB + 32 * PAGE_SIZE));
    }

    #[test]
    fn bad_input_is_rejected() {
        assert_eq!(
            FrameBitmap::new(vec![0u64; 3], MIB, []).unwrap_err(),
            FrameInitError::BadStorage
        );
        assert_eq!(
            FrameBitmap::new(Vec::<u64>::new(), MIB, []).unwrap_err(),
            FrameInitError::BadStorage
        );
        assert_eq!(
            FrameBitmap::new(vec![0u64; 2], MIB, [(2 * MIB, MIB)]).unwrap_err(),
            FrameInitError::InvertedRange {
                start: 2 * MIB,
                end: MIB
            }
        );
        // Empty and fully-below-minimum ranges are simply ignored.
        let (mut b, report) = bitmap(1, MIB, &[(5, 5), (0, 0x8000)]);
        assert_eq!(b.free_frames(), 0);
        assert_eq!(report.below_minimum_bytes, 0x8000);
        assert_eq!(b.reserve(10, 5), 0);
    }
}
