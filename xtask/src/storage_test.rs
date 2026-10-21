//! Only creates new regular images in a fresh target/storage-test directory.
//! No caller-supplied disk paths, no raw disks, no overwrite of existing data.
use crate::{KernelVariant, Options, build_image, qemu, workspace_root};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    process::Stdio,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use storage::{BlockDevice, DISK_SECTORS, DiskFs, SECTOR, Vfs};

#[derive(Debug)]
struct Image(File);
impl BlockDevice for Image {
    fn sectors(&self) -> u64 {
        self.0.metadata().map_or(0, |m| m.len() / SECTOR as u64)
    }
    fn read(&mut self, lba: u64, out: &mut [u8; SECTOR]) -> storage::Result<()> {
        if lba >= DISK_SECTORS {
            return Err(storage::Error::Io);
        }
        self.0
            .seek(SeekFrom::Start(lba * SECTOR as u64))
            .map_err(|_| storage::Error::Io)?;
        self.0.read_exact(out).map_err(|_| storage::Error::Io)
    }
    fn write(&mut self, lba: u64, data: &[u8; SECTOR]) -> storage::Result<()> {
        if lba >= DISK_SECTORS {
            return Err(storage::Error::Io);
        }
        self.0
            .seek(SeekFrom::Start(lba * SECTOR as u64))
            .map_err(|_| storage::Error::Io)?;
        self.0.write_all(data).map_err(|_| storage::Error::Io)
    }
    fn flush(&mut self) -> storage::Result<()> {
        self.0.sync_all().map_err(|_| storage::Error::Io)
    }
}

pub fn run(options: &Options) -> Result<(), String> {
    let boot = build_image(options, KernelVariant::Normal)?;
    let firmware = options.firmware()?;
    let qemu = qemu::program()?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let dir = workspace_root()
        .join("target/storage-test")
        .join(format!("{}-{stamp}", options.profile_dir()));
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let data = dir.join("DISPOSABLE-rsfs.img");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&data)
        .map_err(|e| e.to_string())?;
    file.set_len(DISK_SECTORS * SECTOR as u64)
        .map_err(|e| e.to_string())?;
    storage::format(&mut Image(file)).map_err(|e| format!("format: {e:?}"))?;
    let vars = qemu::copy_vars(&firmware, &dir)?;
    for boot_number in 1..=3 {
        if boot_number == 3 {
            // Corrupt ONLY our freshly created designated image, after both
            // positive runs. Mount must reject it without any guest writes.
            let mut image = OpenOptions::new()
                .write(true)
                .open(&data)
                .map_err(|e| e.to_string())?;
            image.write_all(b"CORRUPT!").map_err(|e| e.to_string())?;
            image.sync_all().map_err(|e| e.to_string())?;
        }
        let before = fs::read(&data).map_err(|e| e.to_string())?;
        let serial = dir.join(format!("boot-{boot_number}.log"));
        let stderr =
            File::create(dir.join(format!("qemu-{boot_number}.log"))).map_err(|e| e.to_string())?;
        let mut cmd = qemu::base_command(&qemu, &firmware, vars.as_deref(), &boot);
        // Override base's no-shutdown: success requires QEMU to exit from
        // guest S5, not from a host kill. Build args without this one flag.
        let args: Vec<_> = cmd
            .get_args()
            .filter(|a| *a != "-no-shutdown")
            .map(|a| a.to_os_string())
            .collect();
        cmd = std::process::Command::new(&qemu);
        cmd.args(args);
        cmd.args(["-display", "none", "-monitor", "none"]);
        cmd.arg("-serial").arg(format!("file:{}", serial.display()));
        cmd.args(["-device", "piix3-ide,id=databus"]);
        cmd.arg("-drive").arg(format!(
            "if=none,id=data,format=raw,cache=writeback,file={}",
            data.display()
        ));
        cmd.args([
            "-device",
            "ide-hd,drive=data,bus=databus.0,unit=0,serial=RUSTOS-DISPOSABLE",
        ]);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(stderr);
        let mut child = cmd.spawn().map_err(|e| format!("QEMU: {e}"))?;
        let start = Instant::now();
        let result = loop {
            let log = fs::read_to_string(&serial).unwrap_or_default();
            if boot_number == 3 && log.contains("[rust-os] storage mount FAILED: Corrupt") {
                break Ok(());
            }
            if log.contains("KERNEL PANIC")
                || log.contains("SELF-TEST FAIL")
                || log.contains("init: FAIL")
                || log.contains("storage driver FAILED")
            {
                break Err(format!("guest failure; see {}", serial.display()));
            }
            if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                let marker = if boot_number == 1 {
                    "STORAGE FIRST WRITE PASS"
                } else {
                    "STORAGE REBOOT READ PASS"
                };
                break if status.success()
                    && log.contains(marker)
                    && log.contains("STORAGE USER TESTS PASS")
                    && log.contains("[rust-os] STORAGE CLEAN SHUTDOWN")
                {
                    Ok(())
                } else {
                    Err(format!(
                        "unexpected QEMU exit {status}; see {}",
                        serial.display()
                    ))
                };
            }
            if start.elapsed() > options.timeout.unwrap_or(Duration::from_secs(120)) {
                break Err(format!("storage boot timeout; see {}", serial.display()));
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        let _ = child.kill();
        let _ = child.wait();
        result?;
        if boot_number == 3 {
            if fs::read(&data).map_err(|e| e.to_string())? != before {
                return Err("corrupt image modified by guest".into());
            }
        } else {
            verify(&data)?;
        }
        eprintln!(
            "storage boot {boot_number}: PASS ({:.1}s)",
            start.elapsed().as_secs_f64()
        );
    }
    eprintln!("storage evidence: {}", dir.display());
    Ok(())
}
fn verify(path: &Path) -> Result<(), String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let disk = DiskFs::mount(Image(file)).map_err(|e| format!("host remount: {e:?}"))?;
    let id = disk
        .lookup("/persist")
        .map_err(|e| format!("persist missing: {e:?}"))?
        .0;
    let mut b = [0; 32];
    if disk.read(id, 0, &mut b) != Ok(17) || &b[..17] != b"survives a reboot" {
        return Err("host verification differs".into());
    }
    if disk.lookup("/test").is_ok() {
        return Err("deleted test directory persisted".into());
    }
    Ok(())
}

/// Creates only a new regular disposable image; never overwrites an existing file.
pub(crate) fn fresh_disk(path: &Path) -> Result<(), String> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    file.set_len(DISK_SECTORS * SECTOR as u64)
        .map_err(|e| e.to_string())?;
    storage::format(&mut Image(file)).map_err(|e| format!("format: {e:?}"))
}

/// Independently remount only the disposable image created by shell_run.
pub fn verify_shell_preferences(path: &Path) -> Result<(), String> {
    let image = File::open(path).map_err(|e| e.to_string())?;
    let fs = DiskFs::mount(Image(image)).map_err(|e| format!("power remount: {e:?}"))?;
    let id = fs
        .lookup("/desktop.pref")
        .map_err(|e| format!("preference: {e:?}"))?
        .0;
    let mut bytes = [0; 6];
    let n = fs
        .read(id, 0, &mut bytes)
        .map_err(|e| format!("preference read: {e:?}"))?;
    if &bytes[..n] != b"RSP1\x01" {
        return Err("power lost saved preference bytes".into());
    }
    Ok(())
}

pub fn verify_shell_files(path: &Path, boot: usize) -> Result<(), String> {
    let image = File::open(path).map_err(|e| e.to_string())?;
    let fs = DiskFs::mount(Image(image)).map_err(|e| format!("shell remount: {e:?}"))?;
    let check = |path: &str, expected: &[u8]| -> Result<(), String> {
        let id = fs.lookup(path).map_err(|e| format!("{path}: {e:?}"))?.0;
        let mut bytes = [0; 4096];
        let n = fs
            .read(id, 0, &mut bytes)
            .map_err(|e| format!("{path}: {e:?}"))?;
        if &bytes[..n] == expected {
            Ok(())
        } else {
            Err(format!("wrong bytes at {path}"))
        }
    };
    check("/docs/beta", b"New text file.\n")?;
    if boot == 1 {
        check("/.trash/t00/origin", b"RST1\n/beta")?;
        check("/.trash/t00/data", b"New text file.\n")?;
    } else if fs.lookup("/.trash/t00").is_ok() {
        return Err("Trash record survived permanent delete".into());
    }
    if fs.lookup("/beta").is_ok() {
        return Err("trashed/deleted file still at original path".into());
    }
    Ok(())
}
