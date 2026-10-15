//! The boot program bundle: named programs packed into one blob.
//!
//! The build tool writes it with [`build`]; the bootloader loads it into
//! memory as the ramdisk; the kernel validates it once with
//! [`Bundle::parse`] and looks programs up by name.
//!
//! Layout (all integers little-endian):
//!
//! | Offset | Size | Field |
//! | --- | --- | --- |
//! | 0 | 8 | magic `RSOSBNDL` |
//! | 8 | 4 | format version (1) |
//! | 12 | 4 | entry count (at most [`MAX_ENTRIES`]) |
//! | 16 | 48 × count | entries: name (32 bytes, NUL-padded), offset (u64), size (u64) |
//! | ... | | program data, each at a 16-byte-aligned offset |
//!
//! Names are 1..=32 bytes of `[a-z0-9._-]`, unique. Data ranges lie after
//! the entry table, inside the blob, in entry order, without overlap.

use alloc::vec::Vec;

use crate::{u32_at, u64_at};

pub const MAGIC: [u8; 8] = *b"RSOSBNDL";
pub const VERSION: u32 = 1;
pub const MAX_ENTRIES: usize = 32;
pub const NAME_LEN: usize = 32;
const HEADER: usize = 16;
const ENTRY: usize = NAME_LEN + 16;
const ALIGN: usize = 16;

/// Why a bundle was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BundleError {
    Truncated,
    BadMagic,
    UnsupportedVersion,
    TooManyEntries,
    BadName,
    DuplicateName,
    /// A data range is misaligned, outside the blob, overlaps the table or
    /// another entry, or is out of order.
    BadRange,
}

/// A validated bundle borrowed from its bytes.
#[derive(Clone, Copy, Debug)]
pub struct Bundle<'a> {
    bytes: &'a [u8],
    count: usize,
}

/// One program in a bundle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry<'a> {
    pub name: &'a str,
    pub data: &'a [u8],
}

fn valid_name_byte(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
}

/// Whether `name` is a valid program name.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= NAME_LEN && name.bytes().all(valid_name_byte)
}

impl<'a> Bundle<'a> {
    /// Validate `bytes` completely; afterwards lookups cannot fail on
    /// malformed data.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, BundleError> {
        if bytes.len() < HEADER {
            return Err(BundleError::Truncated);
        }
        if bytes[..8] != MAGIC {
            return Err(BundleError::BadMagic);
        }
        if u32_at(bytes, 8) != Some(VERSION) {
            return Err(BundleError::UnsupportedVersion);
        }
        let count = u32_at(bytes, 12).ok_or(BundleError::Truncated)? as usize;
        if count > MAX_ENTRIES {
            return Err(BundleError::TooManyEntries);
        }
        let table_end = HEADER + count * ENTRY;
        if table_end > bytes.len() {
            return Err(BundleError::Truncated);
        }
        let bundle = Self { bytes, count };
        let mut next_free = table_end as u64;
        for i in 0..count {
            let (name, offset, size) = bundle.raw_entry(i)?;
            if bundle.iter_raw(i).any(|other| other == Ok(name)) {
                return Err(BundleError::DuplicateName);
            }
            let end = offset.checked_add(size).ok_or(BundleError::BadRange)?;
            if offset % ALIGN as u64 != 0 || offset < next_free || end > bytes.len() as u64 {
                return Err(BundleError::BadRange);
            }
            next_free = end;
        }
        Ok(bundle)
    }

    /// Entry `i`'s name, offset and size, with the name validated.
    fn raw_entry(&self, i: usize) -> Result<(&'a str, u64, u64), BundleError> {
        let at = HEADER + i * ENTRY;
        let field = &self.bytes[at..at + NAME_LEN];
        let len = field.iter().position(|&b| b == 0).unwrap_or(NAME_LEN);
        if field[len..].iter().any(|&b| b != 0) {
            return Err(BundleError::BadName);
        }
        let name = core::str::from_utf8(&field[..len]).map_err(|_| BundleError::BadName)?;
        if !valid_name(name) {
            return Err(BundleError::BadName);
        }
        let offset = u64_at(self.bytes, at + NAME_LEN).ok_or(BundleError::Truncated)?;
        let size = u64_at(self.bytes, at + NAME_LEN + 8).ok_or(BundleError::Truncated)?;
        Ok((name, offset, size))
    }

    /// Names of entries before `i`.
    fn iter_raw(&self, i: usize) -> impl Iterator<Item = Result<&'a str, BundleError>> + '_ {
        (0..i).map(|j| self.raw_entry(j).map(|(name, _, _)| name))
    }

    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Entry `i`, if it exists.
    pub fn get(&self, i: usize) -> Option<Entry<'a>> {
        if i >= self.count {
            return None;
        }
        let (name, offset, size) = self.raw_entry(i).ok()?;
        let data = self.bytes.get(offset as usize..(offset + size) as usize)?;
        Some(Entry { name, data })
    }

    pub fn iter(&self) -> impl Iterator<Item = Entry<'a>> + '_ {
        (0..self.count).filter_map(|i| self.get(i))
    }

    /// The program called `name`.
    pub fn find(&self, name: &[u8]) -> Option<Entry<'a>> {
        self.iter().find(|e| e.name.as_bytes() == name)
    }
}

/// Pack `(name, data)` pairs into a bundle.
pub fn build(entries: &[(&str, &[u8])]) -> Result<Vec<u8>, BundleError> {
    if entries.len() > MAX_ENTRIES {
        return Err(BundleError::TooManyEntries);
    }
    for (i, (name, _)) in entries.iter().enumerate() {
        if !valid_name(name) {
            return Err(BundleError::BadName);
        }
        if entries[..i].iter().any(|(other, _)| other == name) {
            return Err(BundleError::DuplicateName);
        }
    }
    let align = |n: usize| n.div_ceil(ALIGN) * ALIGN;
    let mut out = Vec::new();
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    let mut offset = align(HEADER + entries.len() * ENTRY);
    for (name, data) in entries {
        let mut field = [0u8; NAME_LEN];
        field[..name.len()].copy_from_slice(name.as_bytes());
        out.extend_from_slice(&field);
        out.extend_from_slice(&(offset as u64).to_le_bytes());
        out.extend_from_slice(&(data.len() as u64).to_le_bytes());
        offset = align(offset + data.len());
    }
    for (_, data) in entries {
        out.resize(align(out.len()), 0);
        out.extend_from_slice(data);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn sample() -> Vec<u8> {
        build(&[("init", b"first program"), ("echo", &[1, 2, 3]), ("e", &[])]).unwrap()
    }

    #[test]
    fn round_trips() {
        let blob = sample();
        let bundle = Bundle::parse(&blob).unwrap();
        assert_eq!(bundle.len(), 3);
        assert_eq!(bundle.find(b"init").unwrap().data, b"first program");
        assert_eq!(bundle.find(b"echo").unwrap().data, &[1, 2, 3]);
        assert_eq!(bundle.find(b"e").unwrap().data, &[] as &[u8]);
        assert_eq!(bundle.find(b"missing"), None);
        assert_eq!(bundle.find(b""), None);
        let names: Vec<&str> = bundle.iter().map(|e| e.name).collect();
        assert_eq!(names, ["init", "echo", "e"]);
        assert!(Bundle::parse(&build(&[]).unwrap()).unwrap().is_empty());
    }

    #[test]
    fn build_rejects_bad_names() {
        assert_eq!(build(&[("Init", b"")]), Err(BundleError::BadName));
        assert_eq!(build(&[("", b"")]), Err(BundleError::BadName));
        assert_eq!(build(&[("a/b", b"")]), Err(BundleError::BadName));
        let long = "a".repeat(NAME_LEN + 1);
        assert_eq!(build(&[(&long, b"")]), Err(BundleError::BadName));
        assert_eq!(
            build(&[("x", b""), ("x", b"")]),
            Err(BundleError::DuplicateName)
        );
    }

    #[test]
    fn parse_rejects_corruption() {
        let good = sample();
        assert_eq!(
            Bundle::parse(&good[..10]).err(),
            Some(BundleError::Truncated)
        );
        let mut f = good.clone();
        f[0] = b'X';
        assert_eq!(Bundle::parse(&f).err(), Some(BundleError::BadMagic));
        let mut f = good.clone();
        f[8] = 2;
        assert_eq!(
            Bundle::parse(&f).err(),
            Some(BundleError::UnsupportedVersion)
        );
        let mut f = good.clone();
        f[12..16].copy_from_slice(&1000u32.to_le_bytes());
        assert_eq!(Bundle::parse(&f).err(), Some(BundleError::TooManyEntries));
        let mut f = good.clone();
        f[12..16].copy_from_slice(&30u32.to_le_bytes());
        assert_eq!(Bundle::parse(&f).err(), Some(BundleError::Truncated));
        // Upper-case name, garbage after the NUL, duplicate name.
        let mut f = good.clone();
        f[HEADER] = b'I';
        assert_eq!(Bundle::parse(&f).err(), Some(BundleError::BadName));
        let mut f = good.clone();
        f[HEADER + 10] = b'x';
        assert_eq!(Bundle::parse(&f).err(), Some(BundleError::BadName));
        let mut f = good.clone();
        f[HEADER + ENTRY..HEADER + ENTRY + 4].copy_from_slice(b"init");
        assert_eq!(Bundle::parse(&f).err(), Some(BundleError::DuplicateName));
        // Offsets: misaligned, inside the table, past the end, overflowing,
        // out of order.
        let offset_at = HEADER + NAME_LEN;
        let size_at = offset_at + 8;
        for (at, value) in [
            (offset_at, 0x41u64),
            (offset_at, 0),
            (offset_at, good.len() as u64),
            (size_at, u64::MAX),
            (size_at, 0x10_0000),
            (offset_at + ENTRY, 0),
        ] {
            let mut f = good.clone();
            f[at..at + 8].copy_from_slice(&value.to_le_bytes());
            assert_eq!(
                Bundle::parse(&f).err(),
                Some(BundleError::BadRange),
                "{at} {value:#x}"
            );
        }
    }

    #[test]
    fn never_panics_on_corrupted_input() {
        let good = sample();
        for at in 0..good.len() {
            for value in [0x00, 0x01, 0x10, 0x7f, 0xff] {
                let mut f = good.clone();
                f[at] = value;
                if let Ok(bundle) = Bundle::parse(&f) {
                    let _ = bundle.iter().count();
                }
            }
        }
        for len in 0..good.len() {
            let _ = Bundle::parse(&good[..len]);
        }
        let _ = Bundle::parse(&vec![0xff; 4096]);
    }
}
