use super::*;
use alloc::{format, vec};

#[derive(Debug, Clone)]
struct Disk {
    bytes: Vec<[u8; SECTOR]>,
    fail: bool,
    writes_left: Option<usize>,
    fail_flush: bool,
}
impl Disk {
    fn new() -> Self {
        Self {
            bytes: vec![[0; SECTOR]; DISK_SECTORS as usize],
            fail: false,
            writes_left: None,
            fail_flush: false,
        }
    }
}
impl BlockDevice for Disk {
    fn sectors(&self) -> u64 {
        self.bytes.len() as u64
    }
    fn read(&mut self, lba: u64, out: &mut [u8; SECTOR]) -> Result<()> {
        *out = *self.bytes.get(lba as usize).ok_or(Error::Io)?;
        Ok(())
    }
    fn write(&mut self, lba: u64, data: &[u8; SECTOR]) -> Result<()> {
        if let Some(left) = &mut self.writes_left {
            if *left == 0 {
                return Err(Error::Io);
            }
            *left -= 1;
        }
        if self.fail {
            return Err(Error::Io);
        }
        *self.bytes.get_mut(lba as usize).ok_or(Error::Io)? = *data;
        Ok(())
    }
    fn flush(&mut self) -> Result<()> {
        if self.fail || self.fail_flush {
            Err(Error::Io)
        } else {
            Ok(())
        }
    }
}
fn exercise(fs: &mut dyn Vfs) {
    fs.create("/home", Kind::Directory).unwrap();
    fs.create("/home/sub", Kind::Directory).unwrap();
    let id = fs.create("/home/sub/file", Kind::File).unwrap();
    assert_eq!(fs.write(id, 3, b"hello"), Ok(5));
    let mut b = [0xff; 32];
    assert_eq!(fs.read(id, 0, &mut b), Ok(8));
    assert_eq!(&b[..8], b"\0\0\0hello");
    assert_eq!(fs.read(id, usize::MAX, &mut b), Ok(0));
    fs.rename("/home/sub", "/moved").unwrap();
    assert_eq!(fs.lookup("/moved/file"), Ok((id, Kind::File)));
    assert_eq!(fs.list("/moved", &mut b), Ok(5));
    assert_eq!(&b[..5], b"file\n");
    assert_eq!(fs.remove("/moved"), Err(Error::NotEmpty));
    assert_eq!(fs.rename("/moved", "/moved/child"), Err(Error::InvalidPath));
    fs.remove("/moved/file").unwrap();
    assert_eq!(fs.read(id, 0, &mut b), Err(Error::NotFound));
    assert_eq!(fs.lookup("/absent"), Err(Error::NotFound));
    fs.remove("/moved").unwrap();
}
#[test]
fn ram_operations() {
    exercise(&mut RamFs::new());
}
#[test]
fn paths() {
    assert_eq!(normalize("//a/./b/../c/").unwrap(), "/a/c");
    for p in [
        "",
        "relative",
        "/../x",
        "/a/../../b",
        "/a\0b",
        "/a\nb",
        "/a\\b",
    ] {
        assert_eq!(normalize(p), Err(Error::InvalidPath));
    }
}
#[test]
fn capacity_and_errors_leave_data_intact() {
    let mut fs = RamFs::new();
    for i in 0..NODES {
        fs.create(&format!("/f{i}"), Kind::File).unwrap();
    }
    assert_eq!(fs.create("/extra", Kind::File), Err(Error::NoSpace));
    let id = fs.lookup("/f0").unwrap().0;
    fs.write(id, 0, &[7; FILE_MAX]).unwrap();
    assert_eq!(fs.write(id, FILE_MAX, b"x"), Err(Error::NoSpace));
    assert_eq!(fs.write(id, usize::MAX, b"x"), Err(Error::TooLarge));
    let mut b = [0; FILE_MAX];
    fs.read(id, 0, &mut b).unwrap();
    assert_eq!(b, [7; FILE_MAX]);
    assert_eq!(fs.rename("/f0", "/f1"), Err(Error::Exists));
    fs.remove("/f1").unwrap();
    fs.create("/extra", Kind::File).unwrap();
}
#[test]
fn clean_remount_persists_and_operations_work() {
    let mut d = Disk::new();
    format(&mut d).unwrap();
    let mut mounted = DiskFs::mount(d).unwrap();
    exercise(&mut mounted);
    let id = mounted.fs.create("/home/persist", Kind::File).unwrap();
    mounted.fs.write(id, 0, b"persistent bytes").unwrap();
    mounted.sync().unwrap();
    let mounted = DiskFs::mount(mounted.into_device()).unwrap();
    let id = mounted.fs.lookup("/home/persist").unwrap().0;
    let mut b = [0; 32];
    assert_eq!(mounted.fs.read(id, 0, &mut b), Ok(16));
    assert_eq!(&b[..16], b"persistent bytes");
    assert_eq!(mounted.fs.lookup("/moved"), Err(Error::NotFound));
}
#[test]
fn rejects_corruption_and_forged_metadata() {
    let mut d = Disk::new();
    format(&mut d).unwrap();
    let mut f = DiskFs::mount(d).unwrap();
    let id = f.fs.create("/file", Kind::File).unwrap();
    f.fs.write(id, 0, b"data").unwrap();
    f.sync().unwrap();
    let d = f.into_device();
    for sector in [0, 1, 65] {
        for byte in 0..if sector == 65 { 4 } else { SECTOR } {
            let mut bad = d.clone();
            bad.bytes[sector][byte] ^= 0x80;
            assert!(matches!(DiskFs::mount(bad), Err(Error::Corrupt)));
        }
    }
    let mut bad = d.clone();
    bad.bytes[1][4..8].copy_from_slice(&u32::MAX.to_le_bytes());
    seal(&mut bad.bytes[1]);
    assert!(matches!(DiskFs::mount(bad), Err(Error::Corrupt)));
    let mut bad = d.clone();
    bad.bytes[2] = bad.bytes[1];
    assert!(matches!(DiskFs::mount(bad), Err(Error::Corrupt)));
    let mut bad = d;
    bad.bytes[1][1] = 4;
    bad.bytes[1][12..132].fill(0);
    bad.bytes[1][12..16].copy_from_slice(b"/x/y");
    seal(&mut bad.bytes[1]);
    assert!(matches!(DiskFs::mount(bad), Err(Error::Corrupt)));
}
#[test]
fn io_error_poisoning() {
    let mut d = Disk::new();
    format(&mut d).unwrap();
    let mut f = DiskFs::mount(d).unwrap();
    f.device.fail = true;
    assert_eq!(f.sync(), Err(Error::Io));
    assert_eq!(f.writable(), Err(Error::ReadOnly));
    assert_eq!(f.sync(), Err(Error::ReadOnly));
    assert_eq!(f.create("/denied", Kind::File), Err(Error::ReadOnly));
}

#[test]
fn interrupted_writeback_and_flush_failure_freeze_mutations() {
    let mut d = Disk::new();
    format(&mut d).unwrap();
    for fail_after in [0, 1, 8, 9, 63, 72] {
        let mut f = DiskFs::mount(d.clone()).unwrap();
        let id = f.create("/data", Kind::File).unwrap();
        f.write(id, 0, &[3; FILE_MAX]).unwrap();
        if fail_after == 72 {
            f.device.fail_flush = true;
        } else {
            f.device.writes_left = Some(fail_after);
        }
        assert_eq!(f.sync(), Err(Error::Io));
        assert_eq!(f.write(id, 0, b"x"), Err(Error::ReadOnly));
        assert_eq!(f.remove("/data"), Err(Error::ReadOnly));
        assert_eq!(f.rename("/data", "/new"), Err(Error::ReadOnly));
        // No recovery assertion: a partially written disk may mount or fail.
        let _ = DiskFs::mount(f.into_device());
    }
}

#[test]
fn failed_rename_and_listing_are_non_mutating() {
    let mut fs = RamFs::default();
    fs.create("/a", Kind::Directory).unwrap();
    let long = format!("/a/{}", "x".repeat(PATH_MAX - 3));
    let id = fs.create(&long, Kind::File).unwrap();
    assert_eq!(fs.rename("/a", "/longer"), Err(Error::TooLarge));
    assert_eq!(fs.lookup(&long), Ok((id, Kind::File)));
    assert_eq!(fs.lookup("/longer"), Err(Error::NotFound));
    let mut small = [77; 2];
    assert_eq!(fs.list("/a", &mut small), Err(Error::TooLarge));
    assert_eq!(small, [77; 2]);
    assert_eq!(
        fs.create(&format!("{long}/child"), Kind::File),
        Err(Error::InvalidPath)
    );
    assert_eq!(fs.remove("/"), Err(Error::InvalidPath));
}

#[test]
fn full_disk_remount_and_reuse() {
    let mut d = Disk::new();
    format(&mut d).unwrap();
    let mut f = DiskFs::mount(d).unwrap();
    for i in 0..NODES {
        let id = f.create(&format!("/f{i}"), Kind::File).unwrap();
        f.write(id, 0, &[i as u8; FILE_MAX]).unwrap();
    }
    assert_eq!(f.create("/overflow", Kind::File), Err(Error::NoSpace));
    f.sync().unwrap();
    let mut f = DiskFs::mount(f.into_device()).unwrap();
    for i in 0..NODES {
        let id = f.lookup(&format!("/f{i}")).unwrap().0;
        let mut b = [0; FILE_MAX];
        assert_eq!(f.read(id, 0, &mut b), Ok(FILE_MAX));
        assert_eq!(b, [i as u8; FILE_MAX]);
    }
    f.remove("/f0").unwrap();
    f.create("/replacement", Kind::File).unwrap();
    f.sync().unwrap();
    assert!(
        DiskFs::mount(f.into_device())
            .unwrap()
            .lookup("/replacement")
            .is_ok()
    );
}

#[test]
fn stepped_writeback_freezes_mutations_can_resume_and_remount() {
    let mut disk = Disk::new();
    format(&mut disk).unwrap();
    let mut fs = DiskFs::mount(disk).unwrap();
    let id = fs.create("/a", Kind::File).unwrap();
    fs.write(id, 0, b"saved").unwrap();
    assert_eq!(fs.sync_step(), Ok(false));
    assert_eq!(fs.create("/b", Kind::File), Err(Error::Busy));
    assert_eq!(fs.write(id, 0, b"changed"), Err(Error::Busy));
    let mut bytes = [0; 5];
    assert_eq!(fs.read(id, 0, &mut bytes), Ok(5));
    assert_eq!(&bytes, b"saved");
    // A synchronous caller can complete a cooperative caller's interrupted sync.
    fs.sync().unwrap();
    fs.create("/b", Kind::Directory).unwrap();
    let mut calls = 0;
    while !fs.sync_step().unwrap() {
        calls += 1;
        assert!(calls <= NODES * 9);
    }
    assert!(calls >= NODES);
    let fs = DiskFs::mount(fs.into_device()).unwrap();
    let id = fs.lookup("/a").unwrap().0;
    assert_eq!(fs.read(id, 0, &mut bytes), Ok(5));
    assert_eq!(&bytes, b"saved");
    assert_eq!(fs.lookup("/b").unwrap().1, Kind::Directory);
}
#[test]
fn stepped_writeback_failure_poisons_mount() {
    let mut disk = Disk::new();
    format(&mut disk).unwrap();
    let mut fs = DiskFs::mount(disk).unwrap();
    let id = fs.create("/a", Kind::File).unwrap();
    fs.write(id, 0, b"keep").unwrap();
    fs.device.writes_left = Some(2);
    assert_eq!(fs.sync_step(), Ok(false));
    assert_eq!(fs.sync_step(), Ok(false));
    assert_eq!(fs.sync_step(), Err(Error::Io));
    assert_eq!(fs.sync_step(), Err(Error::ReadOnly));
    assert_eq!(fs.remove("/a"), Err(Error::ReadOnly));
}
