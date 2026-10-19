use super::*;
use storage::{Kind, RamFs, Vfs};
struct Fs {
    fs: RamFs,
    fail: Option<Error>,
}
impl Fs {
    fn new() -> Self {
        Self {
            fs: RamFs::new(),
            fail: None,
        }
    }
}
fn error(e: storage::Error) -> Error {
    match e {
        storage::Error::NotFound => Error::NotFound,
        storage::Error::Exists => Error::AlreadyExists,
        storage::Error::NoSpace => Error::NoSpace,
        storage::Error::NotDirectory => Error::NotDirectory,
        storage::Error::IsDirectory => Error::IsDirectory,
        storage::Error::NotEmpty => Error::NotEmpty,
        _ => Error::InvalidArgument,
    }
}
impl Backend for Fs {
    fn list(&mut self, path: &str) -> Result<Vec<Entry>> {
        let mut bytes = [0; 8192];
        let n = self.fs.list(path, &mut bytes).map_err(error)?;
        core::str::from_utf8(&bytes[..n])
            .unwrap()
            .lines()
            .map(|name| {
                let directory = name.ends_with('/');
                let name = name.trim_end_matches('/');
                let size = if directory {
                    None
                } else {
                    Some(self.read(&child(path, name)?)?.len())
                };
                Ok(Entry {
                    name: name.to_string(),
                    directory,
                    size,
                })
            })
            .collect()
    }
    fn read(&mut self, path: &str) -> Result<Vec<u8>> {
        let id = self.fs.lookup(path).map_err(error)?.0;
        let mut bytes = alloc::vec![0;4096];
        let n = self.fs.read(id, 0, &mut bytes).map_err(error)?;
        bytes.truncate(n);
        Ok(bytes)
    }
    fn create(&mut self, path: &str, bytes: &[u8]) -> Result<()> {
        if let Some(e) = self.fail {
            return Err(e);
        }
        let id = self.fs.create(path, Kind::File).map_err(error)?;
        self.fs.write(id, 0, bytes).map(drop).map_err(error)
    }
    fn mkdir(&mut self, path: &str) -> Result<()> {
        if let Some(e) = self.fail {
            return Err(e);
        }
        self.fs
            .create(path, Kind::Directory)
            .map(drop)
            .map_err(error)
    }
    fn rename(&mut self, from: &str, to: &str) -> Result<()> {
        self.fs.rename(from, to).map_err(error)
    }
    fn remove(&mut self, path: &str) -> Result<()> {
        self.fs.remove(path).map_err(error)
    }
    fn sync_step(&mut self) -> Result<bool> {
        if let Some(e) = self.fail {
            Err(e)
        } else {
            Ok(true)
        }
    }
}
fn drain(ui: &mut Browser, fs: &mut Fs) {
    for _ in 0..1000 {
        if !ui.tick(fs) {
            return;
        }
    }
    panic!("operation never completes");
}
fn select(ui: &mut Browser, fs: &mut Fs, name: &str) {
    ui.refresh(fs).unwrap();
    ui.selected = ui.entries.iter().position(|e| e.name == name).unwrap();
}
#[test]
fn folder_copy_rename_trash_restore_and_confirmed_delete() {
    let mut fs = Fs::new();
    let mut ui = Browser::new(false);
    ui.submit(&mut fs, Prompt::Folder, "docs").unwrap();
    drain(&mut ui, &mut fs);
    ui.path = "/docs".to_string();
    ui.submit(&mut fs, Prompt::Text, "a.txt").unwrap();
    drain(&mut ui, &mut fs);
    assert_eq!(fs.read("/docs/a.txt").unwrap(), b"New text file.\n");
    select(&mut ui, &mut fs, "a.txt");
    ui.key(&mut fs, 0x2e);
    ui.path = "/".to_string();
    ui.paste(&mut fs).unwrap();
    drain(&mut ui, &mut fs);
    select(&mut ui, &mut fs, "a.txt");
    ui.submit(&mut fs, Prompt::Rename, "b.txt").unwrap();
    drain(&mut ui, &mut fs);
    select(&mut ui, &mut fs, "b.txt");
    ui.trash_selected(&mut fs).unwrap();
    drain(&mut ui, &mut fs);
    assert!(fs.read("/b.txt").is_err());
    let mut trash = Browser::new(true);
    trash.refresh(&mut fs).unwrap();
    assert_eq!(trash.original(&mut fs).unwrap(), "/b.txt");
    trash.restore(&mut fs).unwrap();
    drain(&mut trash, &mut fs);
    assert_eq!(fs.read("/b.txt").unwrap(), fs.read("/docs/a.txt").unwrap());
    select(&mut ui, &mut fs, "b.txt");
    ui.submit(&mut fs, Prompt::Delete, "").unwrap();
    drain(&mut ui, &mut fs);
    assert!(fs.read("/b.txt").is_ok());
    ui.trash_selected(&mut fs).unwrap();
    drain(&mut ui, &mut fs);
    trash.refresh(&mut fs).unwrap();
    trash.submit(&mut fs, Prompt::Delete, "delete").unwrap();
    drain(&mut trash, &mut fs);
    assert!(trash.entries.is_empty());
    assert!(fs.read("/docs/a.txt").is_ok());
}
#[test]
fn collisions_preserve_payload_and_metadata() {
    let mut fs = Fs::new();
    fs.create("/a", b"old").unwrap();
    let mut ui = Browser::new(false);
    select(&mut ui, &mut fs, "a");
    ui.trash_selected(&mut fs).unwrap();
    drain(&mut ui, &mut fs);
    fs.create("/a", b"new").unwrap();
    let mut trash = Browser::new(true);
    trash.refresh(&mut fs).unwrap();
    trash.restore(&mut fs).unwrap();
    drain(&mut trash, &mut fs);
    assert!(trash.status.contains("AlreadyExists"));
    assert_eq!(fs.read("/a").unwrap(), b"new");
    assert_eq!(fs.read("/.trash/t00/data").unwrap(), b"old");
    assert_eq!(trash.original(&mut fs).unwrap(), "/a");
    fs.remove("/a").unwrap();
    trash.restore(&mut fs).unwrap();
    drain(&mut trash, &mut fs);
    assert_eq!(fs.read("/a").unwrap(), b"old");
    select(&mut ui, &mut fs, "a");
    ui.key(&mut fs, 0x2e);
    assert_eq!(ui.paste(&mut fs), Err(Error::InvalidArgument));
}
#[test]
fn full_volume_and_permission_failure_never_remove_source() {
    let mut fs = Fs::new();
    fs.create("/a", b"keep").unwrap();
    let mut ui = Browser::new(false);
    select(&mut ui, &mut fs, "a");
    for i in 1..64 {
        fs.create(&format!("/f{i}"), b"").unwrap();
    }
    ui.refresh(&mut fs).unwrap();
    assert_eq!(ui.entries.len(), 64);
    for _ in 0..64 {
        ui.key(&mut fs, 0xe050);
    }
    assert_eq!(ui.selected, 63);
    assert!(ui.lines().iter().any(|l| l.starts_with(">")));
    select(&mut ui, &mut fs, "a");
    assert_eq!(ui.trash_selected(&mut fs), Err(Error::NoSpace));
    assert_eq!(fs.read("/a").unwrap(), b"keep");
    fs.fail = Some(Error::PermissionDenied);
    ui.submit(&mut fs, Prompt::Folder, "denied").unwrap();
    drain(&mut ui, &mut fs);
    assert!(ui.status.contains("PermissionDenied"));
    assert_eq!(fs.read("/a").unwrap(), b"keep");
}
#[test]
fn interrupted_trash_and_sync_failure_leave_recovery_records() {
    let mut fs = Fs::new();
    fs.create("/a", b"keep").unwrap();
    let mut ui = Browser::new(false);
    select(&mut ui, &mut fs, "a");
    ui.trash_selected(&mut fs).unwrap();
    ui.tick(&mut fs);
    ui.tick(&mut fs);
    ui.tick(&mut fs);
    drop(ui); // Simulated process exit after persisted origin, before rename.
    let mut trash = Browser::new(true);
    trash.refresh(&mut fs).unwrap();
    assert_eq!(trash.original(&mut fs).unwrap(), "/a");
    assert_eq!(fs.read("/a").unwrap(), b"keep");
    trash.restore(&mut fs).unwrap();
    drain(&mut trash, &mut fs);
    assert!(trash.status.contains("NotFound"));
    trash.submit(&mut fs, Prompt::Delete, "delete").unwrap();
    drain(&mut trash, &mut fs);
    assert_eq!(fs.read("/a").unwrap(), b"keep");
    let mut ui = Browser::new(false);
    select(&mut ui, &mut fs, "a");
    ui.trash_selected(&mut fs).unwrap();
    ui.tick(&mut fs);
    ui.tick(&mut fs);
    fs.fail = Some(Error::Io);
    ui.tick(&mut fs);
    assert!(ui.status.contains("Io"));
    assert_eq!(fs.read("/a").unwrap(), b"keep");
    assert!(fs.read("/.trash/t00/origin").is_ok());
}
#[test]
fn recursive_copy_cut_move_viewer_and_large_directory_scroll() {
    let mut fs = Fs::new();
    fs.mkdir("/src").unwrap();
    fs.mkdir("/dest").unwrap();
    fs.mkdir("/src/sub").unwrap();
    fs.create("/src/sub/a", b"hello\nworld").unwrap();
    let mut ui = Browser::new(false);
    select(&mut ui, &mut fs, "src");
    ui.key(&mut fs, 0x2e);
    ui.path = "/dest".to_string();
    ui.paste(&mut fs).unwrap();
    drain(&mut ui, &mut fs);
    assert_eq!(fs.read("/dest/src/sub/a").unwrap(), b"hello\nworld");
    ui.path = "/src/sub".to_string();
    select(&mut ui, &mut fs, "a");
    ui.key(&mut fs, 0x2d);
    ui.path = "/".to_string();
    ui.paste(&mut fs).unwrap();
    drain(&mut ui, &mut fs);
    assert!(fs.read("/src/sub/a").is_err());
    select(&mut ui, &mut fs, "a");
    ui.key(&mut fs, 0x1c);
    assert!(ui.viewer.is_some());
    ui.key(&mut fs, 0x0e);
    for i in 0..50 {
        fs.create(&format!("/f{i:02}"), b"").unwrap();
    }
    ui.refresh(&mut fs).unwrap();
    for _ in 0..60 {
        ui.key(&mut fs, 0xe050);
    }
    assert_eq!(ui.selected, ui.entries.len() - 1);
    assert!(ui.lines().iter().any(|l| l.starts_with(">")));
}
#[test]
fn input_validation_and_escape_cancel() {
    for bad in ["", ".", "..", "a/b", "a\\b", "a\nb"] {
        assert_eq!(child("/", bad), Err(Error::InvalidArgument));
    }
    assert_eq!(location("/a/../b").unwrap(), "/b");
    assert!(location("/.trash/x").is_err());
    assert!(location("/../../").is_err());
    let mut fs = Fs::new();
    let mut ui = Browser::new(false);
    ui.key(&mut fs, 0x31);
    ui.key(&mut fs, 0x1e);
    ui.key(&mut fs, 0x01);
    ui.key(&mut fs, 0x1c);
    assert!(ui.queue.is_empty());
}

#[test]
fn missing_parent_and_corrupt_origin_are_safe_restore_errors() {
    let mut fs = Fs::new();
    fs.mkdir("/parent").unwrap();
    fs.create("/parent/a", b"safe").unwrap();
    let mut ui = Browser::new(false);
    ui.path = "/parent".to_string();
    select(&mut ui, &mut fs, "a");
    ui.trash_selected(&mut fs).unwrap();
    drain(&mut ui, &mut fs);
    fs.remove("/parent").unwrap();
    let mut trash = Browser::new(true);
    trash.refresh(&mut fs).unwrap();
    trash.restore(&mut fs).unwrap();
    drain(&mut trash, &mut fs);
    assert!(trash.status.contains("NotFound"));
    assert_eq!(fs.read("/.trash/t00/data").unwrap(), b"safe");
    fs.mkdir("/parent").unwrap();
    trash.restore(&mut fs).unwrap();
    // Interruption after rename+sync: original exists, pending metadata remains.
    trash.tick(&mut fs);
    trash.tick(&mut fs);
    drop(trash);
    let mut trash = Browser::new(true);
    trash.refresh(&mut fs).unwrap();
    assert!(trash.labels[0].contains("PENDING"));
    trash.submit(&mut fs, Prompt::Delete, "delete").unwrap();
    drain(&mut trash, &mut fs);
    assert_eq!(fs.read("/parent/a").unwrap(), b"safe");
    fs.mkdir("/.trash/t00").unwrap();
    fs.create("/.trash/t00/origin", b"RST1\n/.trash/t01/data")
        .unwrap();
    trash.refresh(&mut fs).unwrap();
    assert!(trash.labels[0].starts_with("INVALID"));
    assert_eq!(trash.restore(&mut fs), Err(Error::PermissionDenied));
    assert!(!trash.busy());
}
#[test]
fn failed_recursive_copy_keeps_source_and_reports_partial_destination() {
    let mut fs = Fs::new();
    fs.mkdir("/src").unwrap();
    fs.create("/src/a", b"safe").unwrap();
    fs.mkdir("/dest").unwrap();
    let mut ui = Browser::new(false);
    select(&mut ui, &mut fs, "src");
    ui.key(&mut fs, 0x2e);
    ui.path = "/dest".to_string();
    ui.paste(&mut fs).unwrap();
    ui.tick(&mut fs); // destination directory created
    fs.fail = Some(Error::NoSpace);
    ui.tick(&mut fs);
    assert!(ui.status.contains("NoSpace"));
    assert!(ui.status.contains("partial work kept"));
    assert_eq!(fs.read("/src/a").unwrap(), b"safe");
    assert!(fs.fs.lookup("/dest/src").is_ok());
}

#[test]
fn destructive_prompt_keeps_its_named_target_across_refresh() {
    let mut fs = Fs::new();
    fs.create("/b", b"selected").unwrap();
    let mut ui = Browser::new(false);
    select(&mut ui, &mut fs, "b");
    ui.key(&mut fs, 0x20);
    assert!(ui.lines().iter().any(|l| l == "Target: /b"));
    fs.create("/a", b"other").unwrap();
    ui.refresh(&mut fs).unwrap();
    ui.submit(&mut fs, Prompt::Delete, "delete").unwrap();
    drain(&mut ui, &mut fs);
    assert_eq!(fs.read("/a").unwrap(), b"other");
    assert!(fs.read("/b").is_err());
}

#[test]
fn paste_cannot_hide_user_data_in_reserved_trash_root() {
    let mut fs = Fs::new();
    fs.mkdir("/nested").unwrap();
    fs.mkdir("/nested/.trash").unwrap();
    let mut ui = Browser::new(false);
    ui.path = "/nested".to_string();
    select(&mut ui, &mut fs, ".trash");
    ui.key(&mut fs, 0x2d);
    ui.path = "/".to_string();
    assert_eq!(ui.paste(&mut fs), Err(Error::PermissionDenied));
    assert!(fs.fs.lookup("/nested/.trash").is_ok());
    assert!(!ui.busy());
}
