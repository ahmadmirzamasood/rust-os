//! Existing validated file syscalls; no hardware or kernel dependencies.
use alloc::{string::ToString, vec::Vec};
use desktop::files::{Backend, Entry};
use ulib::{Error, Result, fs};
pub struct GuestFs;
impl Backend for GuestFs {
    fn list(&mut self, path: &str) -> Result<Vec<Entry>> {
        let mut bytes = [0; 8192];
        let n = fs::list(path, &mut bytes)?;
        let text = core::str::from_utf8(&bytes[..n]).map_err(|_| Error::Corrupt)?;
        let mut out = Vec::new();
        for name in text.lines() {
            let directory = name.ends_with('/');
            let name = name.trim_end_matches('/');
            let full = desktop::files::child(path, name)?;
            let size = if directory {
                None
            } else {
                self.read(&full).ok().map(|v| v.len())
            };
            out.push(Entry {
                name: name.to_string(),
                directory,
                size,
            });
        }
        Ok(out)
    }
    fn read(&mut self, path: &str) -> Result<Vec<u8>> {
        let file = fs::File::open(path, false, false)?;
        let mut bytes = alloc::vec![0; 4096];
        let n = file.read(0, &mut bytes)?;
        bytes.truncate(n);
        Ok(bytes)
    }
    fn create(&mut self, path: &str, bytes: &[u8]) -> Result<()> {
        if bytes.len() > 4096 {
            return Err(Error::TooLarge);
        }
        let file = fs::File::open(path, true, true)?;
        let result = file.write(0, bytes).and_then(|n| {
            if n == bytes.len() {
                Ok(())
            } else {
                Err(Error::Io)
            }
        });
        drop(file);
        if let Err(e) = result {
            // Exclusive create succeeded, so cleanup cannot remove a preexisting
            // destination. Surface cleanup failure rather than hide partial data.
            fs::remove(path)?;
            return Err(e);
        }
        Ok(())
    }
    fn mkdir(&mut self, path: &str) -> Result<()> {
        fs::mkdir(path)
    }
    fn rename(&mut self, from: &str, to: &str) -> Result<()> {
        fs::rename(from, to)
    }
    fn remove(&mut self, path: &str) -> Result<()> {
        fs::remove(path)
    }
    fn sync_step(&mut self) -> Result<bool> {
        fs::sync_step()
    }
}
