//! Single mounted volume. All bundled applications share read/write access;
//! each file handle separately carries write permission. No disk passthrough.
use crate::{drivers::ata::Ata, kprintln, sync::TryLock};
use storage::{DiskFs, Error, RamFs, Result, Vfs};

enum Volume {
    Ram(RamFs),
    Disk(DiskFs<Ata>),
    Failed(Error),
}
static VOLUME: TryLock<Option<Volume>> = TryLock::new(None);

pub fn init() {
    let volume = match Ata::discover() {
        Ok(d) => match DiskFs::mount(d) {
            Ok(fs) => {
                kprintln!("[rust-os] storage: RSFS1 disposable disk mounted");
                Volume::Disk(fs)
            }
            Err(e) => {
                kprintln!("[rust-os] storage mount FAILED: {:?}", e);
                Volume::Failed(e)
            }
        },
        Err(Error::NotFound) => {
            kprintln!("[rust-os] storage: temporary RAM filesystem");
            Volume::Ram(RamFs::new())
        }
        Err(e) => {
            kprintln!("[rust-os] storage driver FAILED: {:?}", e);
            Volume::Failed(e)
        }
    };
    *VOLUME.try_lock().expect("boot storage lock") = Some(volume);
}
pub fn with<R>(write: bool, f: impl FnOnce(&mut dyn Vfs) -> Result<R>) -> Result<R> {
    let mut v = VOLUME.try_lock().ok_or(Error::Io)?;
    match v.as_mut().ok_or(Error::Io)? {
        Volume::Ram(fs) => f(fs),
        Volume::Disk(d) => {
            if write {
                d.writable()?;
            }
            f(d)
        }
        Volume::Failed(e) => Err(*e),
    }
}
pub fn sync() -> Result<()> {
    let mut v = VOLUME.try_lock().ok_or(Error::Io)?;
    match v.as_mut().ok_or(Error::Io)? {
        Volume::Ram(_) => Ok(()),
        Volume::Disk(d) => d.sync(),
        Volume::Failed(e) => Err(*e),
    }
}
pub fn persistent() -> Result<bool> {
    let v = VOLUME.try_lock().ok_or(Error::Io)?;
    match v.as_ref().ok_or(Error::Io)? {
        Volume::Ram(_) => Ok(false),
        Volume::Disk(_) => Ok(true),
        Volume::Failed(e) => Err(*e),
    }
}

/// One bounded device transfer; release the lock between calls so display/input
/// can run. DiskFs freezes mutations until the shared writeback finishes.
pub fn sync_step() -> Result<bool> {
    let mut v = VOLUME.try_lock().ok_or(Error::Io)?;
    match v.as_mut().ok_or(Error::Io)? {
        Volume::Ram(_) => Ok(true),
        Volume::Disk(d) => d.sync_step(),
        Volume::Failed(e) => Err(*e),
    }
}
