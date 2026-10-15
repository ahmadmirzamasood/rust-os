//! The boot program bundle.
//!
//! The build tool packs the user programs into one bundle
//! ([`exec::bundle`]) and the bootloader loads it as the ramdisk, mapped
//! into the kernel half. It is validated once at boot (including every
//! program's ELF headers) and is read-only afterwards. Without a valid
//! bundle the kernel still boots; it just cannot start processes.

use core::sync::atomic::{AtomicU64, Ordering};

use exec::bundle::{Bundle, BundleError};

static ADDR: AtomicU64 = AtomicU64::new(0);
static LEN: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgramsError {
    /// The bootloader passed no ramdisk.
    NoRamdisk,
    Bundle(BundleError),
}

/// Validate the ramdisk at `[addr, addr + len)` and make it available.
///
/// # Safety
///
/// The range must be the bootloader's ramdisk mapping: mapped, readable,
/// never unmapped and never written by anyone afterwards.
pub unsafe fn init(addr: Option<u64>, len: u64) -> Result<Bundle<'static>, ProgramsError> {
    let addr = addr.filter(|_| len > 0).ok_or(ProgramsError::NoRamdisk)?;
    let ptr = core::ptr::with_exposed_provenance::<u8>(addr as usize);
    // SAFETY: per the caller's contract the bytes stay mapped, readable and
    // unchanged for the rest of the kernel's life.
    let bytes: &'static [u8] = unsafe { core::slice::from_raw_parts(ptr, len as usize) };
    let bundle = Bundle::parse(bytes).map_err(ProgramsError::Bundle)?;
    ADDR.store(addr, Ordering::SeqCst);
    LEN.store(len, Ordering::SeqCst);
    Ok(bundle)
}

fn bundle() -> Option<Bundle<'static>> {
    let (addr, len) = (ADDR.load(Ordering::SeqCst), LEN.load(Ordering::SeqCst));
    if addr == 0 {
        return None;
    }
    let ptr = core::ptr::with_exposed_provenance::<u8>(addr as usize);
    // SAFETY: only set by `init` after validating its contract.
    let bytes: &'static [u8] = unsafe { core::slice::from_raw_parts(ptr, len as usize) };
    Bundle::parse(bytes).ok()
}

/// The program called `name`.
pub fn find(name: &[u8]) -> Option<&'static [u8]> {
    bundle()?.find(name).map(|e| e.data)
}
