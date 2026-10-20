//! Copy-only display device. Only the bundled display service is authorized.
use crate::{Result, call};
use os_abi::syscall as nr;
/// Query (0), shutdown (1), restart (2); root shell only. Never suspends.
pub fn power(action: u64) -> Result<u64> {
    call(nr::POWER, action, 0, 0, 0)
}
pub fn dimensions() -> Result<(usize, usize)> {
    call(nr::DISPLAY_INFO, 0, 0, 0, 0).map(|v| (v as u32 as usize, (v >> 32) as usize))
}
pub fn row(y: usize, rgb: &[u8]) -> Result<()> {
    call(
        nr::DISPLAY_ROW,
        y as u64,
        rgb.as_ptr() as u64,
        rgb.len() as u64,
        0,
    )
    .map(drop)
}
pub fn input() -> Result<[u8; 16]> {
    let mut bytes = [0; 16];
    call(nr::DISPLAY_INPUT, bytes.as_mut_ptr() as u64, 16, 0, 0)?;
    Ok(bytes)
}
