//! Validating parser for static x86_64 ELF executables.
//!
//! Accepted input: a 64-bit little-endian `ET_EXEC` file for `EM_X86_64`
//! (a statically linked, non-position-independent program). The parser
//! returns the entry point and the loadable segments; it never maps memory.
//!
//! Rejected (each with its own [`ElfError`]): truncated or oversized
//! headers, other classes/machines/types, dynamic linking (`PT_INTERP`,
//! `PT_DYNAMIC`), thread-local storage (`PT_TLS`), segments that are both
//! writable and executable, segments whose file bytes lie outside the file
//! or exceed their memory size, segments outside the caller's address
//! range, segments that share a page, programs larger than the caller's
//! limit, and entry points outside every executable segment.
//!
//! Other program-header types (notes, `PT_PHDR`, `PT_GNU_STACK`,
//! `PT_GNU_RELRO`, ...) are ignored: they do not change what is mapped. The
//! user stack is always non-executable regardless of `PT_GNU_STACK`, and
//! RELRO regions simply stay writable.

use crate::{u16_at, u32_at, u64_at};

/// Most loadable segments an executable may have.
pub const MAX_SEGMENTS: usize = 8;
/// Most program headers an executable may have.
pub const MAX_PROGRAM_HEADERS: usize = 32;

const PAGE: u64 = 4096;
const HEADER_SIZE: usize = 64;
const PHDR_SIZE: usize = 56;

const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PT_INTERP: u32 = 3;
const PT_TLS: u32 = 7;

const PF_X: u32 = 1;
const PF_W: u32 = 2;

/// Address range and size limits imposed by the loader.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Lowest address any segment may cover.
    pub start: u64,
    /// End (exclusive) of the range segments must stay in.
    pub end: u64,
    /// Largest sum of page-rounded segment sizes.
    pub max_bytes: u64,
}

/// One loadable segment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Segment {
    pub vaddr: u64,
    pub mem_size: u64,
    pub file_offset: u64,
    pub file_size: u64,
    pub writable: bool,
    pub executable: bool,
}

impl Segment {
    /// First page-aligned address of the segment.
    pub fn page_start(&self) -> u64 {
        self.vaddr & !(PAGE - 1)
    }

    /// End (exclusive) of the segment, rounded up to a page. Cannot
    /// overflow for a segment returned by [`parse`].
    pub fn page_end(&self) -> u64 {
        (self.vaddr + self.mem_size).div_ceil(PAGE) * PAGE
    }
}

/// A validated executable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Image {
    pub entry: u64,
    segments: [Segment; MAX_SEGMENTS],
    count: usize,
}

impl Image {
    pub fn segments(&self) -> &[Segment] {
        &self.segments[..self.count]
    }

    /// Total bytes of memory the segments occupy (whole pages).
    pub fn mapped_bytes(&self) -> u64 {
        self.segments()
            .iter()
            .map(|s| s.page_end() - s.page_start())
            .sum()
    }
}

/// Why an executable was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ElfError {
    /// Shorter than its own headers say.
    Truncated,
    /// Not `\x7fELF`.
    BadMagic,
    /// Not 64-bit little-endian version 1.
    UnsupportedFormat,
    /// Not an executable (for example a shared object or PIE).
    NotExecutable,
    /// Not x86_64.
    WrongMachine,
    /// Program-header size or count is wrong.
    BadProgramHeaders,
    /// Needs a dynamic linker or relocations.
    DynamicLinking,
    /// Uses thread-local storage, which the loader does not set up.
    ThreadLocalStorage,
    /// More than [`MAX_SEGMENTS`] loadable segments.
    TooManySegments,
    /// No loadable segment.
    NoSegments,
    /// File size larger than memory size, or file range outside the file.
    BadSegmentRange,
    /// A segment is writable and executable.
    WriteExecute,
    /// A segment lies outside the allowed address range.
    OutOfRange,
    /// Two segments share a page.
    Overlap,
    /// The program needs more memory than allowed.
    TooLarge,
    /// The entry point is not inside an executable segment.
    BadEntry,
}

impl ElfError {
    pub const fn description(self) -> &'static str {
        match self {
            Self::Truncated => "file truncated",
            Self::BadMagic => "not an ELF file",
            Self::UnsupportedFormat => "not 64-bit little-endian ELF version 1",
            Self::NotExecutable => "not a static executable (ET_EXEC)",
            Self::WrongMachine => "not an x86_64 program",
            Self::BadProgramHeaders => "malformed program-header table",
            Self::DynamicLinking => "needs dynamic linking",
            Self::ThreadLocalStorage => "uses thread-local storage",
            Self::TooManySegments => "too many loadable segments",
            Self::NoSegments => "no loadable segment",
            Self::BadSegmentRange => "segment file range invalid",
            Self::WriteExecute => "segment both writable and executable",
            Self::OutOfRange => "segment outside the user image range",
            Self::Overlap => "segments share a page",
            Self::TooLarge => "program too large",
            Self::BadEntry => "entry point not in an executable segment",
        }
    }
}

/// Validate `file` and describe how to load it.
pub fn parse(file: &[u8], limits: &Limits) -> Result<Image, ElfError> {
    if file.len() < HEADER_SIZE {
        return Err(ElfError::Truncated);
    }
    if file[..4] != [0x7f, b'E', b'L', b'F'] {
        return Err(ElfError::BadMagic);
    }
    // EI_CLASS 2 = 64-bit, EI_DATA 1 = little-endian, EI_VERSION 1.
    if file[4] != 2 || file[5] != 1 || file[6] != 1 {
        return Err(ElfError::UnsupportedFormat);
    }
    let field16 = |at| u16_at(file, at).ok_or(ElfError::Truncated);
    let field32 = |at| u32_at(file, at).ok_or(ElfError::Truncated);
    let field64 = |at| u64_at(file, at).ok_or(ElfError::Truncated);
    if field16(16)? != 2 {
        return Err(ElfError::NotExecutable);
    }
    if field16(18)? != 62 {
        return Err(ElfError::WrongMachine);
    }
    if field32(20)? != 1 {
        return Err(ElfError::UnsupportedFormat);
    }
    let entry = field64(24)?;
    let phoff = field64(32)?;
    let phentsize = usize::from(field16(54)?);
    let phnum = usize::from(field16(56)?);
    if phentsize != PHDR_SIZE || phnum == 0 || phnum > MAX_PROGRAM_HEADERS {
        return Err(ElfError::BadProgramHeaders);
    }
    let table_start = usize::try_from(phoff).map_err(|_| ElfError::Truncated)?;
    let table_end = phnum
        .checked_mul(PHDR_SIZE)
        .and_then(|size| table_start.checked_add(size))
        .ok_or(ElfError::Truncated)?;
    if table_end > file.len() {
        return Err(ElfError::Truncated);
    }

    let mut image = Image {
        entry,
        segments: [Segment::default(); MAX_SEGMENTS],
        count: 0,
    };
    for i in 0..phnum {
        let at = table_start + i * PHDR_SIZE;
        let kind = field32(at)?;
        match kind {
            PT_INTERP | PT_DYNAMIC => return Err(ElfError::DynamicLinking),
            PT_TLS => return Err(ElfError::ThreadLocalStorage),
            PT_LOAD => {}
            _ => continue,
        }
        let flags = field32(at + 4)?;
        let segment = Segment {
            file_offset: field64(at + 8)?,
            vaddr: field64(at + 16)?,
            file_size: field64(at + 32)?,
            mem_size: field64(at + 40)?,
            writable: flags & PF_W != 0,
            executable: flags & PF_X != 0,
        };
        if segment.mem_size == 0 {
            continue;
        }
        check_segment(file.len() as u64, &segment, limits)?;
        if image.count == MAX_SEGMENTS {
            return Err(ElfError::TooManySegments);
        }
        image.segments[image.count] = segment;
        image.count += 1;
    }
    if image.count == 0 {
        return Err(ElfError::NoSegments);
    }

    let segments = image.segments();
    for (i, a) in segments.iter().enumerate() {
        for b in &segments[i + 1..] {
            if a.page_start() < b.page_end() && b.page_start() < a.page_end() {
                return Err(ElfError::Overlap);
            }
        }
    }
    if image.mapped_bytes() > limits.max_bytes {
        return Err(ElfError::TooLarge);
    }
    let entry_ok = segments
        .iter()
        .any(|s| s.executable && entry >= s.vaddr && entry - s.vaddr < s.mem_size);
    if !entry_ok {
        return Err(ElfError::BadEntry);
    }
    Ok(image)
}

fn check_segment(file_len: u64, s: &Segment, limits: &Limits) -> Result<(), ElfError> {
    if s.writable && s.executable {
        return Err(ElfError::WriteExecute);
    }
    if s.file_size > s.mem_size {
        return Err(ElfError::BadSegmentRange);
    }
    let file_end = s
        .file_offset
        .checked_add(s.file_size)
        .ok_or(ElfError::BadSegmentRange)?;
    if file_end > file_len {
        return Err(ElfError::BadSegmentRange);
    }
    // Page-rounded end must fit as well, so `page_end` cannot overflow.
    let end = s
        .vaddr
        .checked_add(s.mem_size)
        .and_then(|end| end.checked_add(PAGE - 1))
        .ok_or(ElfError::OutOfRange)?;
    if s.vaddr < limits.start || end - (PAGE - 1) > limits.end {
        return Err(ElfError::OutOfRange);
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    pub const LIMITS: Limits = Limits {
        start: 0x40_0000,
        end: 0x4000_0000,
        max_bytes: 16 << 20,
    };

    #[derive(Clone, Copy)]
    pub struct Ph {
        pub kind: u32,
        pub flags: u32,
        pub offset: u64,
        pub vaddr: u64,
        pub filesz: u64,
        pub memsz: u64,
    }

    pub fn load(flags: u32, offset: u64, vaddr: u64, filesz: u64, memsz: u64) -> Ph {
        Ph {
            kind: PT_LOAD,
            flags,
            offset,
            vaddr,
            filesz,
            memsz,
        }
    }

    /// A minimal ELF file: header, program headers, then `body` bytes.
    pub fn build(entry: u64, phs: &[Ph], body: usize) -> Vec<u8> {
        let mut f = vec![0u8; HEADER_SIZE + phs.len() * PHDR_SIZE + body];
        f[..4].copy_from_slice(&[0x7f, b'E', b'L', b'F']);
        f[4] = 2;
        f[5] = 1;
        f[6] = 1;
        f[16..18].copy_from_slice(&2u16.to_le_bytes());
        f[18..20].copy_from_slice(&62u16.to_le_bytes());
        f[20..24].copy_from_slice(&1u32.to_le_bytes());
        f[24..32].copy_from_slice(&entry.to_le_bytes());
        f[32..40].copy_from_slice(&(HEADER_SIZE as u64).to_le_bytes());
        f[54..56].copy_from_slice(&(PHDR_SIZE as u16).to_le_bytes());
        f[56..58].copy_from_slice(&(phs.len() as u16).to_le_bytes());
        for (i, ph) in phs.iter().enumerate() {
            let at = HEADER_SIZE + i * PHDR_SIZE;
            f[at..at + 4].copy_from_slice(&ph.kind.to_le_bytes());
            f[at + 4..at + 8].copy_from_slice(&ph.flags.to_le_bytes());
            f[at + 8..at + 16].copy_from_slice(&ph.offset.to_le_bytes());
            f[at + 16..at + 24].copy_from_slice(&ph.vaddr.to_le_bytes());
            f[at + 32..at + 40].copy_from_slice(&ph.filesz.to_le_bytes());
            f[at + 40..at + 48].copy_from_slice(&ph.memsz.to_le_bytes());
        }
        f
    }

    fn typical() -> Vec<u8> {
        build(
            0x40_1000,
            &[
                load(4, 0, 0x40_0000, 0x100, 0x100),     // R: headers, rodata
                load(5, 0x100, 0x40_1000, 0x200, 0x200), // RX: text
                load(6, 0x300, 0x40_2000, 0x10, 0x3000), // RW: data + bss
            ],
            0x400,
        )
    }

    #[test]
    fn accepts_a_typical_static_executable() {
        let image = parse(&typical(), &LIMITS).unwrap();
        assert_eq!(image.entry, 0x40_1000);
        let s = image.segments();
        assert_eq!(s.len(), 3);
        assert!(!s[0].writable && !s[0].executable);
        assert!(s[1].executable && !s[1].writable);
        assert!(s[2].writable && !s[2].executable);
        assert_eq!((s[2].page_start(), s[2].page_end()), (0x40_2000, 0x40_5000));
        assert_eq!(image.mapped_bytes(), 0x5000);
    }

    #[test]
    fn rejects_bad_headers() {
        let good = typical();
        assert_eq!(parse(&good[..63], &LIMITS), Err(ElfError::Truncated));
        let mut f = good.clone();
        f[1] = b'X';
        assert_eq!(parse(&f, &LIMITS), Err(ElfError::BadMagic));
        let mut f = good.clone();
        f[4] = 1; // 32-bit
        assert_eq!(parse(&f, &LIMITS), Err(ElfError::UnsupportedFormat));
        let mut f = good.clone();
        f[5] = 2; // big-endian
        assert_eq!(parse(&f, &LIMITS), Err(ElfError::UnsupportedFormat));
        let mut f = good.clone();
        f[16] = 3; // ET_DYN (shared object or PIE)
        assert_eq!(parse(&f, &LIMITS), Err(ElfError::NotExecutable));
        let mut f = good.clone();
        f[18] = 3; // i386
        assert_eq!(parse(&f, &LIMITS), Err(ElfError::WrongMachine));
        let mut f = good.clone();
        f[54] = 32; // wrong program-header size
        assert_eq!(parse(&f, &LIMITS), Err(ElfError::BadProgramHeaders));
        let mut f = good.clone();
        f[56..58].copy_from_slice(&0u16.to_le_bytes());
        assert_eq!(parse(&f, &LIMITS), Err(ElfError::BadProgramHeaders));
        let mut f = good.clone();
        f[56..58].copy_from_slice(&1000u16.to_le_bytes());
        assert_eq!(parse(&f, &LIMITS), Err(ElfError::BadProgramHeaders));
        let mut f = good.clone();
        f[32..40].copy_from_slice(&u64::MAX.to_le_bytes()); // table offset overflows
        assert_eq!(parse(&f, &LIMITS), Err(ElfError::Truncated));
        let mut f = good;
        let near_end = f.len() as u64 - 10;
        f[32..40].copy_from_slice(&near_end.to_le_bytes());
        assert_eq!(parse(&f, &LIMITS), Err(ElfError::Truncated));
    }

    #[test]
    fn rejects_dynamic_and_tls() {
        for (kind, err) in [
            (PT_INTERP, ElfError::DynamicLinking),
            (PT_DYNAMIC, ElfError::DynamicLinking),
            (PT_TLS, ElfError::ThreadLocalStorage),
        ] {
            let f = build(
                0x40_0000,
                &[
                    load(5, 0, 0x40_0000, 0x10, 0x10),
                    Ph {
                        kind,
                        ..load(4, 0, 0x40_1000, 0, 0x10)
                    },
                ],
                0x10,
            );
            assert_eq!(parse(&f, &LIMITS), Err(err));
        }
    }

    #[test]
    fn ignores_other_program_headers() {
        let gnu_stack = Ph {
            kind: 0x6474_e551,
            ..load(7, 0, 0, 0, 0)
        };
        let f = build(
            0x40_0000,
            &[gnu_stack, load(5, 0, 0x40_0000, 0x10, 0x10)],
            0x10,
        );
        assert_eq!(parse(&f, &LIMITS).unwrap().segments().len(), 1);
    }

    #[test]
    fn rejects_bad_segments() {
        let cases: [(Ph, ElfError); 7] = [
            (load(7, 0, 0x40_0000, 0x10, 0x10), ElfError::WriteExecute),
            (load(5, 0, 0x40_0000, 0x20, 0x10), ElfError::BadSegmentRange),
            (
                load(5, 0x1000, 0x40_0000, 0x10, 0x10),
                ElfError::BadSegmentRange,
            ),
            (
                load(5, u64::MAX, 0x40_0000, 2, 0x10),
                ElfError::BadSegmentRange,
            ),
            (load(5, 0, 0x1000, 0x10, 0x10), ElfError::OutOfRange),
            (load(5, 0, 0x3fff_f000, 0x10, 0x2000), ElfError::OutOfRange),
            (load(5, 0, u64::MAX - 8, 0, 0x10), ElfError::OutOfRange),
        ];
        for (ph, err) in cases {
            let f = build(ph.vaddr, &[ph], 0x40);
            assert_eq!(parse(&f, &LIMITS), Err(err), "{:#x}", ph.vaddr);
        }
    }

    #[test]
    fn kernel_and_non_canonical_addresses_are_out_of_range() {
        for vaddr in [
            0xffff_8000_0000_0000,
            0x8000_0000_0000_0000,
            0x0000_8000_0000_0000,
        ] {
            let f = build(vaddr, &[load(5, 0, vaddr, 0x10, 0x10)], 0x10);
            assert_eq!(parse(&f, &LIMITS), Err(ElfError::OutOfRange));
        }
    }

    #[test]
    fn rejects_shared_pages_size_and_entry() {
        let overlap = build(
            0x40_0000,
            &[
                load(5, 0, 0x40_0000, 0x10, 0x10),
                load(6, 0, 0x40_0800, 0x10, 0x10),
            ],
            0x10,
        );
        assert_eq!(parse(&overlap, &LIMITS), Err(ElfError::Overlap));

        let big = build(0x40_0000, &[load(5, 0, 0x40_0000, 0x10, 17 << 20)], 0x10);
        assert_eq!(parse(&big, &LIMITS), Err(ElfError::TooLarge));

        // Entry in a non-executable segment, past the end, or nowhere.
        for entry in [0x40_2000, 0x40_1200, 0x50_0000] {
            let f = build(
                entry,
                &[
                    load(5, 0, 0x40_1000, 0x200, 0x200),
                    load(6, 0, 0x40_2000, 0x10, 0x10),
                ],
                0x200,
            );
            assert_eq!(parse(&f, &LIMITS), Err(ElfError::BadEntry), "{entry:#x}");
        }
        let none = build(0x40_0000, &[load(5, 0, 0x40_0000, 0, 0)], 0);
        assert_eq!(parse(&none, &LIMITS), Err(ElfError::NoSegments));
    }

    #[test]
    fn rejects_too_many_segments() {
        let phs: Vec<Ph> = (0..9)
            .map(|i| load(4, 0, 0x40_0000 + i * 0x1000, 0x10, 0x10))
            .collect();
        let f = build(0x40_0000, &phs, 0x10);
        assert_eq!(parse(&f, &LIMITS), Err(ElfError::TooManySegments));
    }

    #[test]
    fn never_panics_on_corrupted_input() {
        // Flip every byte of the headers to every bit pattern of interest;
        // the parser must return (either result) without panicking.
        let good = typical();
        let header_len = HEADER_SIZE + 3 * PHDR_SIZE;
        for at in 0..header_len {
            for value in [0x00, 0x01, 0x7f, 0x80, 0xff] {
                let mut f = good.clone();
                f[at] = value;
                let _ = parse(&f, &LIMITS);
            }
        }
        for len in 0..good.len() {
            let _ = parse(&good[..len], &LIMITS);
        }
    }
}
