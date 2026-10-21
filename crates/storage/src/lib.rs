//! Bounded VFS, RAM filesystem, and RSFS1 disposable-volume format.
//! See docs/FILESYSTEM.md for the wire format and durability contract.
#![no_std]
#![forbid(unsafe_code)]
extern crate alloc;

use alloc::{string::String, vec::Vec};

pub const SECTOR: usize = 512;
pub const NODES: usize = 64;
pub const FILE_MAX: usize = 4096;
pub const PATH_MAX: usize = 120;
pub const DISK_SECTORS: u64 = 1 + NODES as u64 * 9;
const MAGIC: &[u8; 8] = b"RSFS0001";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidPath,
    NotFound,
    Exists,
    NotDirectory,
    IsDirectory,
    NotEmpty,
    NoSpace,
    TooLarge,
    Io,
    Corrupt,
    ReadOnly,
    NoMemory,
    Busy,
}
pub type Result<T> = core::result::Result<T, Error>;

/// Exclusive device ownership; each transfer is exactly one 512-byte sector.
/// A successful flush requests persistence of all preceding writes.
pub trait BlockDevice {
    fn sectors(&self) -> u64;
    fn read(&mut self, lba: u64, out: &mut [u8; SECTOR]) -> Result<()>;
    fn write(&mut self, lba: u64, data: &[u8; SECTOR]) -> Result<()>;
    fn flush(&mut self) -> Result<()>;
}

/// Absolute UTF-8 paths; repeated slashes and dot collapse, dot-dot cannot
/// escape root. Control characters, backslash and overlong inputs are rejected.
pub fn normalize(path: &str) -> Result<String> {
    if !path.starts_with('/')
        || path.len() > PATH_MAX
        || path.chars().any(|c| c.is_control() || c == '\\')
    {
        return Err(Error::InvalidPath);
    }
    let mut out = String::new();
    out.try_reserve(PATH_MAX).map_err(|_| Error::NoMemory)?;
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if out.is_empty() {
                    return Err(Error::InvalidPath);
                }
                out.truncate(out.rfind('/').ok_or(Error::InvalidPath)?);
            }
            _ => {
                out.push('/');
                out.push_str(part);
            }
        }
    }
    if out.is_empty() {
        out.push('/');
    }
    Ok(out)
}

fn parent(path: &str) -> &str {
    let i = path.rfind('/').unwrap_or(0);
    if i == 0 { "/" } else { &path[..i] }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Directory,
}

#[derive(Debug)]
struct Node {
    id: u64,
    path: String,
    kind: Kind,
    data: Vec<u8>,
}

/// Node IDs are mount-local, never reused. Deletion invalidates open IDs;
/// rename preserves them. Callers own handles and enforce access modes.
pub trait Vfs {
    fn lookup(&self, path: &str) -> Result<(u64, Kind)>;
    fn create(&mut self, path: &str, kind: Kind) -> Result<u64>;
    fn read(&self, id: u64, offset: usize, out: &mut [u8]) -> Result<usize>;
    fn write(&mut self, id: u64, offset: usize, data: &[u8]) -> Result<usize>;
    fn list(&self, path: &str, out: &mut [u8]) -> Result<usize>;
    fn rename(&mut self, from: &str, to: &str) -> Result<()>;
    fn remove(&mut self, path: &str) -> Result<()>;
    fn sync(&mut self) -> Result<()>;
}

#[derive(Debug)]
pub struct RamFs {
    nodes: Vec<Node>,
    next: u64,
}

impl Default for RamFs {
    fn default() -> Self {
        Self::new()
    }
}

impl RamFs {
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            next: 1,
        }
    }
    fn node(&self, id: u64) -> Result<&Node> {
        self.nodes
            .iter()
            .find(|n| n.id == id)
            .ok_or(Error::NotFound)
    }
    fn directory(&self, path: &str) -> Result<()> {
        if self.lookup(path)?.1 != Kind::Directory {
            return Err(Error::NotDirectory);
        }
        Ok(())
    }
}

impl Vfs for RamFs {
    fn sync(&mut self) -> Result<()> {
        Ok(())
    }
    fn lookup(&self, path: &str) -> Result<(u64, Kind)> {
        let p = normalize(path)?;
        if p == "/" {
            return Ok((0, Kind::Directory));
        }
        self.nodes
            .iter()
            .find(|n| n.path == p)
            .map(|n| (n.id, n.kind))
            .ok_or(Error::NotFound)
    }
    fn create(&mut self, path: &str, kind: Kind) -> Result<u64> {
        let path = normalize(path)?;
        if self.lookup(&path).is_ok() {
            return Err(Error::Exists);
        }
        self.directory(parent(&path))?;
        if self.nodes.len() == NODES {
            return Err(Error::NoSpace);
        }
        let next = self.next.checked_add(1).ok_or(Error::NoSpace)?;
        self.nodes.try_reserve(1).map_err(|_| Error::NoMemory)?;
        let id = self.next;
        self.nodes.push(Node {
            id,
            path,
            kind,
            data: Vec::new(),
        });
        self.next = next;
        Ok(id)
    }
    fn read(&self, id: u64, offset: usize, out: &mut [u8]) -> Result<usize> {
        let n = self.node(id)?;
        if n.kind != Kind::File {
            return Err(Error::IsDirectory);
        }
        let count = out.len().min(n.data.len().saturating_sub(offset));
        if count > 0 {
            out[..count].copy_from_slice(&n.data[offset..offset + count]);
        }
        Ok(count)
    }
    fn write(&mut self, id: u64, offset: usize, data: &[u8]) -> Result<usize> {
        let n = self
            .nodes
            .iter_mut()
            .find(|n| n.id == id)
            .ok_or(Error::NotFound)?;
        if n.kind != Kind::File {
            return Err(Error::IsDirectory);
        }
        let end = offset.checked_add(data.len()).ok_or(Error::TooLarge)?;
        if end > FILE_MAX {
            return Err(Error::NoSpace);
        }
        if data.is_empty() {
            return Ok(0);
        }
        if end > n.data.len() {
            n.data
                .try_reserve(end - n.data.len())
                .map_err(|_| Error::NoMemory)?;
            n.data.resize(end, 0);
        }
        n.data[offset..end].copy_from_slice(data);
        Ok(data.len())
    }
    /// Newline-separated immediate child names; directories have a trailing
    /// slash. Fails without touching output if the complete listing won't fit.
    fn list(&self, path: &str, out: &mut [u8]) -> Result<usize> {
        let path = normalize(path)?;
        self.directory(&path)?;
        let mut size = 0usize;
        for n in self.nodes.iter().filter(|n| parent(&n.path) == path) {
            let name = n.path.rsplit('/').next().ok_or(Error::Corrupt)?;
            size = size
                .checked_add(name.len() + 1 + usize::from(n.kind == Kind::Directory))
                .ok_or(Error::TooLarge)?;
        }
        if size > out.len() {
            return Err(Error::TooLarge);
        }
        let mut i = 0;
        for n in self.nodes.iter().filter(|n| parent(&n.path) == path) {
            let name = n.path.rsplit('/').next().ok_or(Error::Corrupt)?.as_bytes();
            out[i..i + name.len()].copy_from_slice(name);
            i += name.len();
            if n.kind == Kind::Directory {
                out[i] = b'/';
                i += 1;
            }
            out[i] = b'\n';
            i += 1;
        }
        Ok(size)
    }
    fn rename(&mut self, from: &str, to: &str) -> Result<()> {
        let from = normalize(from)?;
        let to = normalize(to)?;
        if from == "/" {
            return Err(Error::InvalidPath);
        }
        self.lookup(&from)?;
        if from == to {
            return Ok(());
        }
        if self.lookup(&to).is_ok() {
            return Err(Error::Exists);
        }
        self.directory(parent(&to))?;
        if to.strip_prefix(&from).is_some_and(|s| s.starts_with('/')) {
            return Err(Error::InvalidPath);
        }
        let mut changes = Vec::new();
        changes
            .try_reserve(self.nodes.len())
            .map_err(|_| Error::NoMemory)?;
        for (i, n) in self.nodes.iter().enumerate() {
            if n.path == from
                || n.path
                    .strip_prefix(&from)
                    .is_some_and(|s| s.starts_with('/'))
            {
                let suffix = &n.path[from.len()..];
                if to.len().checked_add(suffix.len()).ok_or(Error::TooLarge)? > PATH_MAX {
                    return Err(Error::TooLarge);
                }
                let mut p = String::new();
                p.try_reserve(PATH_MAX).map_err(|_| Error::NoMemory)?;
                p.push_str(&to);
                p.push_str(suffix);
                changes.push((i, p));
            }
        }
        for (i, p) in changes {
            self.nodes[i].path = p;
        }
        Ok(())
    }
    fn remove(&mut self, path: &str) -> Result<()> {
        let path = normalize(path)?;
        if path == "/" {
            return Err(Error::InvalidPath);
        }
        let (id, _) = self.lookup(&path)?;
        if self.nodes.iter().any(|n| parent(&n.path) == path) {
            return Err(Error::NotEmpty);
        }
        self.nodes.retain(|n| n.id != id);
        Ok(())
    }
}

fn hash(bytes: &[u8]) -> u32 {
    bytes.iter().fold(2166136261u32, |h, b| {
        (h ^ u32::from(*b)).wrapping_mul(16777619)
    })
}
fn seal(b: &mut [u8; SECTOR]) {
    let checksum = hash(&b[..508]).to_le_bytes();
    b[508..].copy_from_slice(&checksum);
}
fn valid(b: &[u8; SECTOR]) -> bool {
    b[508..] == hash(&b[..508]).to_le_bytes()
}
fn header() -> [u8; SECTOR] {
    let mut b = [0; SECTOR];
    b[..8].copy_from_slice(MAGIC);
    b[8..16].copy_from_slice(&DISK_SECTORS.to_le_bytes());
    seal(&mut b);
    b
}

/// Formats only an explicitly supplied disposable device. The guest never
/// calls this: mounting bad or blank media must not destroy evidence.
pub fn format(device: &mut impl BlockDevice) -> Result<()> {
    if device.sectors() != DISK_SECTORS {
        return Err(Error::Corrupt);
    }
    for lba in 0..DISK_SECTORS {
        device.write(lba, &[0; SECTOR])?;
    }
    device.write(0, &header())?;
    device.flush()
}

#[derive(Debug)]
pub struct DiskFs<D> {
    fs: RamFs,
    device: D,
    failed: bool,
    syncing: Option<usize>,
}

impl<D: BlockDevice> Vfs for DiskFs<D> {
    fn sync(&mut self) -> Result<()> {
        DiskFs::sync(self)
    }
    fn lookup(&self, path: &str) -> Result<(u64, Kind)> {
        self.fs.lookup(path)
    }
    fn create(&mut self, path: &str, kind: Kind) -> Result<u64> {
        self.writable()?;
        self.fs.create(path, kind)
    }
    fn read(&self, id: u64, offset: usize, out: &mut [u8]) -> Result<usize> {
        self.fs.read(id, offset, out)
    }
    fn write(&mut self, id: u64, offset: usize, data: &[u8]) -> Result<usize> {
        self.writable()?;
        self.fs.write(id, offset, data)
    }
    fn list(&self, path: &str, out: &mut [u8]) -> Result<usize> {
        self.fs.list(path, out)
    }
    fn rename(&mut self, from: &str, to: &str) -> Result<()> {
        self.writable()?;
        self.fs.rename(from, to)
    }
    fn remove(&mut self, path: &str) -> Result<()> {
        self.writable()?;
        self.fs.remove(path)
    }
}

impl<D: BlockDevice> DiskFs<D> {
    pub fn mount(mut device: D) -> Result<Self> {
        if device.sectors() != DISK_SECTORS {
            return Err(Error::Corrupt);
        }
        let mut b = [0; SECTOR];
        device.read(0, &mut b)?;
        if b != header() {
            return Err(Error::Corrupt);
        }
        let mut fs = RamFs::new();
        for i in 0..NODES {
            device.read(1 + i as u64, &mut b)?;
            if b == [0; SECTOR] {
                continue;
            }
            if !valid(&b)
                || !matches!(b[0], 1 | 2)
                || b[2..4] != [0; 2]
                || b[12 + PATH_MAX..508].iter().any(|v| *v != 0)
            {
                return Err(Error::Corrupt);
            }
            let len = usize::from(b[1]);
            if len == 0 || len > PATH_MAX || b[12 + len..12 + PATH_MAX].iter().any(|v| *v != 0) {
                return Err(Error::Corrupt);
            }
            let p = core::str::from_utf8(&b[12..12 + len]).map_err(|_| Error::Corrupt)?;
            let path = normalize(p).map_err(|_| Error::Corrupt)?;
            if path != p || path == "/" || fs.nodes.iter().any(|n| n.path == path) {
                return Err(Error::Corrupt);
            }
            let size = u32::from_le_bytes(b[4..8].try_into().map_err(|_| Error::Corrupt)?) as usize;
            let checksum = u32::from_le_bytes(b[8..12].try_into().map_err(|_| Error::Corrupt)?);
            let kind = if b[0] == 1 {
                Kind::File
            } else {
                Kind::Directory
            };
            if size > FILE_MAX || (kind == Kind::Directory && size != 0) {
                return Err(Error::Corrupt);
            }
            let mut data = Vec::new();
            data.try_reserve_exact(size).map_err(|_| Error::NoMemory)?;
            data.resize(size, 0);
            for (j, chunk) in data.chunks_mut(SECTOR).enumerate() {
                device.read(1 + NODES as u64 + (i * 8 + j) as u64, &mut b)?;
                chunk.copy_from_slice(&b[..chunk.len()]);
            }
            if hash(&data) != checksum {
                return Err(Error::Corrupt);
            }
            fs.nodes.try_reserve(1).map_err(|_| Error::NoMemory)?;
            fs.nodes.push(Node {
                id: fs.next,
                path,
                kind,
                data,
            });
            fs.next += 1;
        }
        for n in &fs.nodes {
            fs.directory(parent(&n.path)).map_err(|_| Error::Corrupt)?;
        }
        Ok(Self {
            fs,
            device,
            failed: false,
            syncing: None,
        })
    }
    pub fn writable(&self) -> Result<()> {
        if self.failed {
            Err(Error::ReadOnly)
        } else if self.syncing.is_some() {
            Err(Error::Busy)
        } else {
            Ok(())
        }
    }
    /// Whole-volume writeback. On error the mount is poisoned read-only;
    /// reboot/remount is required. There is no atomic commit or recovery.
    pub fn sync(&mut self) -> Result<()> {
        while !self.sync_step()? {}
        Ok(())
    }
    /// Writes at most one sector or issues one flush. Mutations are rejected
    /// with Busy until complete; reads remain available. Any caller can resume
    /// an interrupted sync. The caller must keep polling until true or error.
    pub fn sync_step(&mut self) -> Result<bool> {
        if self.failed {
            return Err(Error::ReadOnly);
        }
        let mut step = self.syncing.unwrap_or(0);
        // Unoccupied/empty extents have no logical bytes. Skip their no-op
        // slots within this call, still issuing at most one device transfer.
        if step < NODES * 9
            && step % 9 < 8
            && self
                .fs
                .nodes
                .get(step / 9)
                .is_none_or(|n| n.data.is_empty())
        {
            step = step / 9 * 9 + 8;
        }
        self.syncing = Some(step);
        let result = self.save_step(step);
        match result {
            Ok(true) => self.syncing = None,
            Ok(false) => self.syncing = Some(step + 1),
            Err(_) => {
                self.failed = true;
                self.syncing = None;
            }
        }
        result
    }
    fn save_step(&mut self, step: usize) -> Result<bool> {
        if step == NODES * 9 {
            self.device.flush()?;
            return Ok(true);
        }
        let i = step / 9;
        let j = step % 9;
        let mut bytes = [0; SECTOR];
        if j < 8 {
            if let Some(n) = self.fs.nodes.get(i) {
                let start = j * SECTOR;
                if start < n.data.len() {
                    let len = SECTOR.min(n.data.len() - start);
                    bytes[..len].copy_from_slice(&n.data[start..start + len]);
                }
                self.device
                    .write(1 + NODES as u64 + (i * 8 + j) as u64, &bytes)?;
            }
        } else {
            if let Some(n) = self.fs.nodes.get(i) {
                bytes[0] = if n.kind == Kind::File { 1 } else { 2 };
                bytes[1] = n.path.len() as u8;
                bytes[4..8].copy_from_slice(&(n.data.len() as u32).to_le_bytes());
                bytes[8..12].copy_from_slice(&hash(&n.data).to_le_bytes());
                bytes[12..12 + n.path.len()].copy_from_slice(n.path.as_bytes());
                seal(&mut bytes);
            }
            self.device.write(1 + i as u64, &bytes)?;
        }
        Ok(false)
    }
    pub fn into_device(self) -> D {
        self.device
    }
}

#[cfg(test)]
mod tests;
