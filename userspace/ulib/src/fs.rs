//! Shared volume access. File owns its process-local handle; Drop closes
//! without flushing. Offsets are explicit; sync flushes the whole volume.
use crate::{Error, Handle, Result, call};
use os_abi::syscall as nr;

#[derive(Debug)]
pub struct File(Handle);
impl File {
    pub fn open(path: &str, writable: bool, create: bool) -> Result<Self> {
        let raw = call(
            nr::FILE_OPEN,
            path.as_ptr() as u64,
            path.len() as u64,
            u64::from(writable) | (u64::from(create) << 1),
            0,
        )?;
        Ok(Self(Handle::from_raw(raw as u32).ok_or(Error::BadHandle)?))
    }
    pub fn raw(&self) -> u32 {
        self.0.raw()
    }
    pub fn read(&self, offset: usize, out: &mut [u8]) -> Result<usize> {
        call(
            nr::FILE_READ,
            self.raw() as u64,
            out.as_mut_ptr() as u64,
            out.len() as u64,
            offset as u64,
        )
        .map(|n| n as usize)
    }
    pub fn write(&self, offset: usize, data: &[u8]) -> Result<usize> {
        call(
            nr::FILE_WRITE,
            self.raw() as u64,
            data.as_ptr() as u64,
            data.len() as u64,
            offset as u64,
        )
        .map(|n| n as usize)
    }
}
pub fn list(path: &str, out: &mut [u8]) -> Result<usize> {
    call(
        nr::FS_LIST,
        path.as_ptr() as u64,
        path.len() as u64,
        out.as_mut_ptr() as u64,
        out.len() as u64,
    )
    .map(|n| n as usize)
}
pub fn mkdir(path: &str) -> Result<()> {
    path_call(nr::FS_MKDIR, path)
}
pub fn remove(path: &str) -> Result<()> {
    path_call(nr::FS_REMOVE, path)
}
fn path_call(nr: u64, path: &str) -> Result<()> {
    call(nr, path.as_ptr() as u64, path.len() as u64, 0, 0).map(drop)
}
pub fn rename(from: &str, to: &str) -> Result<()> {
    call(
        nr::FS_RENAME,
        from.as_ptr() as u64,
        from.len() as u64,
        to.as_ptr() as u64,
        to.len() as u64,
    )
    .map(drop)
}
pub fn sync() -> Result<()> {
    call(nr::FS_SYNC, 0, 0, 0, 0).map(drop)
}
pub fn persistent() -> Result<bool> {
    call(nr::FS_VOLUME, 0, 0, 0, 0).map(|n| n == 1)
}
/// Restricted Q35 disposable-disk test shutdown; only init is authorized.
pub fn test_shutdown() -> Result<()> {
    call(nr::FS_TEST_SHUTDOWN, 0, 0, 0, 0).map(drop)
}
