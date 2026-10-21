//! A separate address space must not gain file access by guessing a parent's
//! handle value; it also cannot invoke the init-only test shutdown.
#![no_std]
#![no_main]
use ulib::Error;
ulib::entry!(main);
fn main() -> i32 {
    let Some(channel) = ulib::startup_channel() else {
        return 1;
    };
    let mut value = [0; 4];
    if channel.recv(&mut value) != Ok(4) {
        return 2;
    }
    let mut out = [0; 8];
    // SAFETY: exclusive output buffer; numeric foreign handle deliberately
    // does not name a file owned by this process.
    let result = unsafe {
        ulib::sys::syscall(
            os_abi::syscall::FILE_READ,
            u32::from_le_bytes(value) as u64,
            out.as_mut_ptr() as u64,
            8,
            0,
        )
    };
    if !matches!(
        os_abi::decode_result(result),
        Err(Error::BadHandle | Error::WrongType)
    ) {
        return 3;
    }
    if ulib::fs::test_shutdown() != Err(Error::PermissionDenied) {
        return 4;
    }
    for action in 0..=3 {
        if ulib::display::power(action) != Err(Error::PermissionDenied) {
            return 5;
        }
    }
    0
}
