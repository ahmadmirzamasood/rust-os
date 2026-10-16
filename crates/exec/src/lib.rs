//! Formats for loading project-built user programs.
//!
//! - [`elf`]: a validating parser for static x86_64 ELF executables. It only
//!   describes what to map; the kernel does the mapping.
//! - [`bundle`]: the boot program bundle, a flat archive of named programs
//!   that the build tool writes and the bootloader hands to the kernel as
//!   its ramdisk.
//!
//! Both treat their input as untrusted: every offset, length and address is
//! checked with overflow-checked arithmetic before use, and nothing panics
//! on malformed input. `no_std`, `forbid(unsafe_code)`; [`bundle::build`]
//! needs `alloc` (used by the host build tool).

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod bundle;
pub mod elf;

/// Read a little-endian `u16` at `at`, if in bounds.
fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    let raw = bytes.get(at..at.checked_add(2)?)?;
    Some(u16::from_le_bytes([raw[0], raw[1]]))
}

/// Read a little-endian `u32` at `at`, if in bounds.
fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    let raw = bytes.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
}

/// Read a little-endian `u64` at `at`, if in bounds.
fn u64_at(bytes: &[u8], at: usize) -> Option<u64> {
    let raw = bytes.get(at..at.checked_add(8)?)?;
    let mut word = [0; 8];
    word.copy_from_slice(raw);
    Some(u64::from_le_bytes(word))
}
