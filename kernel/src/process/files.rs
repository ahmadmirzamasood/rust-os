//! User-memory validation is complete before any filesystem mutation.
//! File IDs are looked up only through the caller's existing handle table.
use super::*;
use crate::storage as volume;
use os_abi::syscall as nr;
use storage::{FILE_MAX, Kind, PATH_MAX};

fn error(e: storage::Error) -> Error {
    use storage::Error as E;
    match e {
        E::InvalidPath => Error::InvalidArgument,
        E::NotFound => Error::NotFound,
        E::Exists => Error::AlreadyExists,
        E::NotDirectory => Error::NotDirectory,
        E::IsDirectory => Error::IsDirectory,
        E::NotEmpty => Error::NotEmpty,
        E::NoSpace => Error::NoSpace,
        E::TooLarge => Error::TooLarge,
        E::Io => Error::Io,
        E::Corrupt => Error::Corrupt,
        E::ReadOnly => Error::ReadOnly,
        E::NoMemory => Error::NoResources,
        E::Busy => Error::WouldBlock,
    }
}
fn path(ptr: u64, len: u64) -> Result<alloc::string::String, Error> {
    if len == 0 || len > PATH_MAX as u64 {
        return Err(Error::InvalidArgument);
    }
    let mut b = [0; PATH_MAX];
    user::copy_from(current_root()?, ptr, &mut b[..len as usize]).map_err(user_error)?;
    let p = core::str::from_utf8(&b[..len as usize]).map_err(|_| Error::InvalidArgument)?;
    storage::normalize(p).map_err(error)
}
pub fn call(number: u64, a: u64, b: u64, c: u64, d: u64) -> Result<u64, Error> {
    match number {
        nr::FILE_OPEN => {
            if c & !3 != 0 || c == 2 {
                return Err(Error::InvalidArgument);
            }
            let path = path(a, b)?;
            // Prevent a switch between checking capacity, creating and
            // inserting. No I/O happens while the process IrqCell is held.
            let _guard = crate::sync::PreemptGuard::new();
            with_current(|t, s| {
                if t.processes[s].free_handles() > 0 {
                    Ok(())
                } else {
                    Err(Error::NoResources)
                }
            })?;
            let id = volume::with(c & 1 != 0, |fs| {
                if c & 2 != 0 {
                    fs.create(&path, Kind::File)
                } else {
                    let (id, kind) = fs.lookup(&path)?;
                    if kind != Kind::File {
                        Err(storage::Error::IsDirectory)
                    } else {
                        Ok(id)
                    }
                }
            })
            .map_err(error)?;
            with_current(|t, s| {
                t.processes[s]
                    .insert(Object::File {
                        id,
                        writable: c & 1 != 0,
                    })
                    .map(u64::from)
            })
        }
        nr::FILE_READ | nr::FILE_WRITE => {
            if c > FILE_MAX as u64 || d > FILE_MAX as u64 {
                return Err(Error::TooLarge);
            }
            let (id, writable) = with_current(|t, s| match t.processes[s].lookup(a)? {
                Object::File { id, writable } => Ok((id, writable)),
                _ => Err(Error::WrongType),
            })?;
            let root = current_root()?;
            let mut bytes = [0; FILE_MAX];
            let bytes = &mut bytes[..c as usize];
            if number == nr::FILE_WRITE {
                if !writable {
                    return Err(Error::PermissionDenied);
                }
                user::copy_from(root, b, bytes).map_err(user_error)?;
                volume::with(true, |fs| fs.write(id, d as usize, bytes))
                    .map(|n| n as u64)
                    .map_err(error)
            } else {
                user::check(root, b, c, true).map_err(user_error)?;
                let n = volume::with(false, |fs| fs.read(id, d as usize, bytes)).map_err(error)?;
                user::copy_to(root, b, &bytes[..n]).map_err(user_error)?;
                Ok(n as u64)
            }
        }
        nr::FS_LIST => {
            let path = path(a, b)?;
            if d > 8192 {
                return Err(Error::TooLarge);
            }
            let root = current_root()?;
            user::check(root, c, d, true).map_err(user_error)?;
            let mut bytes = [0; 8192];
            let n = volume::with(false, |fs| fs.list(&path, &mut bytes[..d as usize]))
                .map_err(error)?;
            user::copy_to(root, c, &bytes[..n]).map_err(user_error)?;
            Ok(n as u64)
        }
        nr::FS_MKDIR | nr::FS_REMOVE => {
            let path = path(a, b)?;
            volume::with(true, |fs| {
                if number == nr::FS_MKDIR {
                    fs.create(&path, Kind::Directory).map(|_| ())
                } else {
                    fs.remove(&path)
                }
            })
            .map_err(error)?;
            Ok(0)
        }
        nr::FS_RENAME => {
            let from = path(a, b)?;
            let to = path(c, d)?;
            volume::with(true, |fs| fs.rename(&from, &to)).map_err(error)?;
            Ok(0)
        }
        nr::FS_SYNC_STEP => volume::sync_step().map(u64::from).map_err(error),
        nr::FS_SYNC => {
            volume::sync().map_err(error)?;
            Ok(0)
        }
        nr::FS_VOLUME => Ok(u64::from(volume::persistent().map_err(error)?)),
        nr::FS_TEST_SHUTDOWN => {
            let allowed = with_current(|t, s| Ok(t.processes[s].test_driver))?;
            if !allowed || !volume::persistent().map_err(error)? {
                return Err(Error::PermissionDenied);
            }
            // Hold off scheduling from final flush through power-off: no
            // application may dirty the volume after the successful flush.
            let _guard = crate::sync::PreemptGuard::new();
            volume::sync().map_err(error)?;
            kprintln!("[rust-os] STORAGE CLEAN SHUTDOWN");
            // SAFETY: this restricted test path runs only with the designated
            // PIIX data disk on our Q35 machine; 0x604 is its PM1a control,
            // SLP_TYP=0 (S5), SLP_EN=1. No other code owns this register.
            unsafe { arch::storage_io::outw(0x604, 0x2000) };
            arch::halt_forever()
        }
        _ => Err(Error::UnknownSyscall),
    }
}
