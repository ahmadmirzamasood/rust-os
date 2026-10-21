//! Bounded file-manager policy. Operations never overwrite a destination.
//! Runtime executes one queued action per frame; sync is polled cooperatively.
extern crate alloc;
use alloc::{
    collections::VecDeque,
    format,
    string::{String, ToString},
    vec::Vec,
};
use os_abi::Error;
type Result<T> = core::result::Result<T, Error>;
pub const TRASH: &str = "/.trash";
pub const LIMIT: usize = 64;

/// Whole-file transfers are bounded to 4096 bytes. create is exclusive and must
/// remove an incomplete new file on write failure, preserving existing files.
pub trait Backend {
    fn list(&mut self, path: &str) -> Result<Vec<Entry>>;
    fn read(&mut self, path: &str) -> Result<Vec<u8>>;
    fn create(&mut self, path: &str, bytes: &[u8]) -> Result<()>;
    fn mkdir(&mut self, path: &str) -> Result<()>;
    fn rename(&mut self, from: &str, to: &str) -> Result<()>;
    fn remove(&mut self, path: &str) -> Result<()>;
    fn sync_step(&mut self) -> Result<bool>;
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub directory: bool,
    pub size: Option<usize>,
}
#[derive(Debug)]
enum Step {
    Mkdir(String),
    Copy(String, String),
    Rename(String, String),
    Create(String, Vec<u8>),
    Remove(String),
    Sync,
}
#[derive(Clone, Copy, Debug)]
enum Prompt {
    Folder,
    Text,
    Rename,
    Location,
    Delete,
}
#[derive(Debug)]
pub struct Browser {
    pub path: String,
    pub entries: Vec<Entry>,
    pub selected: usize,
    pub status: String,
    trash: bool,
    prompt: Option<Prompt>,
    input: String,
    clipboard: Option<(String, bool, bool)>,
    queue: VecDeque<Step>,
    viewer: Option<Vec<String>>,
    view_line: usize,
    labels: Vec<String>,
    target: Option<(String, bool)>,
}
fn protected(p: &str) -> bool {
    p == TRASH || p.starts_with("/.trash/")
}
pub fn location(p: &str) -> Result<String> {
    if !p.starts_with('/') || p.len() > 120 || p.chars().any(|c| c.is_control() || c == '\\') {
        return Err(Error::InvalidArgument);
    }
    let mut parts = Vec::new();
    for part in p.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err(Error::InvalidArgument);
                }
            }
            _ => parts.push(part),
        }
    }
    let out = format!("/{}", parts.join("/"));
    if protected(&out) {
        return Err(Error::PermissionDenied);
    }
    Ok(out)
}
pub fn child(p: &str, name: &str) -> Result<String> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\'))
    {
        return Err(Error::InvalidArgument);
    }
    let out = format!("{}/{}", if p == "/" { "" } else { p }, name);
    if out.len() > 120 {
        return Err(Error::TooLarge);
    }
    Ok(out)
}
fn decode_origin(bytes: &[u8]) -> Result<String> {
    let text = core::str::from_utf8(bytes).map_err(|_| Error::Corrupt)?;
    let p = text.strip_prefix("RST1\n").ok_or(Error::Corrupt)?;
    let path = location(p)?;
    if path == "/" || path != p {
        return Err(Error::Corrupt);
    }
    Ok(path)
}
fn parent(p: &str) -> String {
    match p.rfind('/') {
        Some(i) if i > 0 => p[..i].to_string(),
        _ => "/".to_string(),
    }
}
fn tree(b: &mut impl Backend, p: &str, dir: bool, out: &mut Vec<(String, bool)>) -> Result<()> {
    if out.len() >= LIMIT {
        return Err(Error::TooLarge);
    }
    out.push((p.to_string(), dir));
    if dir {
        for entry in b.list(p)? {
            tree(b, &child(p, &entry.name)?, entry.directory, out)?;
        }
    }
    Ok(())
}
impl Browser {
    pub fn new(trash: bool) -> Self {
        Self {
            path: if trash { TRASH } else { "/" }.to_string(),
            entries: Vec::new(),
            selected: 0,
            status: "Ready; H for controls".to_string(),
            trash,
            prompt: None,
            input: String::new(),
            clipboard: None,
            queue: VecDeque::new(),
            viewer: None,
            view_line: 0,
            labels: Vec::new(),
            target: None,
        }
    }
    pub fn captures_escape(&self) -> bool {
        self.prompt.is_some() || self.viewer.is_some()
    }
    pub fn busy(&self) -> bool {
        !self.queue.is_empty()
    }
    pub fn refresh(&mut self, b: &mut impl Backend) -> Result<()> {
        let mut entries = match b.list(&self.path) {
            Err(Error::NotFound) if self.trash => Vec::new(),
            other => other?,
        };
        if !self.trash && self.path == "/" {
            entries.retain(|e| e.name != ".trash");
        }
        entries.sort_by(|a, b| b.directory.cmp(&a.directory).then(a.name.cmp(&b.name)));
        if entries.len() > LIMIT {
            return Err(Error::TooLarge);
        }
        self.labels.clear();
        if self.trash {
            for entry in &entries {
                let container = child(TRASH, &entry.name)?;
                let origin = b
                    .read(&child(&container, "origin")?)
                    .ok()
                    .and_then(|bytes| decode_origin(&bytes).ok());
                let payload = b
                    .list(&container)
                    .ok()
                    .and_then(|children| children.into_iter().find(|e| e.name == "data"));
                self.labels.push(match (origin, payload) {
                    (Some(origin), Some(data)) => format!(
                        "{} {} {}",
                        if data.directory { "DIR" } else { "FILE" },
                        data.size.map(|n| format!("{n}B")).unwrap_or_default(),
                        origin
                    ),
                    (Some(origin), None) => format!("PENDING {origin}"),
                    _ => format!("INVALID {} (D to clean)", entry.name),
                });
            }
        }
        self.entries = entries;
        self.selected = self.selected.min(self.entries.len().saturating_sub(1));
        Ok(())
    }
    fn selected_path(&self) -> Result<String> {
        child(
            &self.path,
            &self.entries.get(self.selected).ok_or(Error::NotFound)?.name,
        )
    }
    fn name_prompt(&mut self, prompt: Prompt) {
        self.target = if matches!(prompt, Prompt::Rename | Prompt::Delete) {
            self.selected_path()
                .ok()
                .zip(self.entries.get(self.selected).map(|e| e.directory))
        } else {
            None
        };
        self.prompt = Some(prompt);
        self.input.clear();
    }
    fn enqueue(&mut self, step: Step) {
        self.queue.push_back(step);
        self.queue.push_back(Step::Sync);
    }
    pub fn report(&mut self, e: Error) {
        self.status = format!("Error: {:?}; H for controls", e);
    }
    /// Raw set-1 keys, printable US lowercase names. Prompts deliberately have no
    /// destructive default: permanent deletion requires typing DELETE then Enter.
    pub fn key(&mut self, b: &mut impl Backend, key: u32) {
        if let Err(e) = self.handle(b, key) {
            self.report(e);
        }
    }
    fn handle(&mut self, b: &mut impl Backend, key: u32) -> Result<()> {
        if self.busy() {
            self.status = "Busy; desktop remains active".to_string();
            return Ok(());
        }
        if let Some(prompt) = self.prompt {
            match key {
                0x01 => {
                    self.prompt = None;
                    self.status = "Cancelled".to_string();
                }
                0x0e => {
                    self.input.pop();
                }
                0x1c => {
                    let input = self.input.clone();
                    self.prompt = None;
                    self.submit(b, prompt, &input)?;
                }
                _ => {
                    if let Some(c) = ascii(key)
                        && self.input.len() < 120
                    {
                        self.input.push(c);
                    }
                }
            }
            return Ok(());
        }
        if self.viewer.is_some() {
            match key {
                0x01 | 0x0e => self.viewer = None,
                0xe050 => self.view_line += 1,
                0xe048 => self.view_line = self.view_line.saturating_sub(1),
                _ => {}
            }
            if let Some(lines) = &self.viewer {
                self.view_line = self.view_line.min(lines.len().saturating_sub(1));
            }
            return Ok(());
        }
        match key {
            0xe048 => self.selected = self.selected.saturating_sub(1),
            0xe050 => self.selected = (self.selected + 1).min(self.entries.len().saturating_sub(1)),
            0x3f => {
                self.refresh(b)?;
                self.status = "Refreshed".to_string();
            }
            0x18 => {
                let path = if self.trash {
                    self.original(b)?
                } else {
                    self.selected_path()?
                };
                self.show_text(&path);
                self.status = "Full original path; Backspace closes".to_string();
            }
            0x23 => {
                self.viewer = Some(
                    [
                        "Files controls",
                        "N new folder; T new sample text file",
                        "R rename; L absolute location; O details",
                        "C copy; X cut; V paste in destination",
                        "Delete moves selection to Trash",
                        "U restore in Trash; D permanent delete",
                        "Enter opens; Up/Down select or scroll",
                        "Backspace parent or close viewer",
                        "F5 refresh; Esc cancels prompt/viewer",
                        "Copy trees may leave partial destination",
                        "Trash records keep original path",
                        "Only one mounted volume is supported",
                    ]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
                );
                self.view_line = 0;
            }
            0x0e if !self.trash => {
                self.path = parent(&self.path);
                self.selected = 0;
                self.refresh(b)?;
            }
            0x26 if !self.trash => self.name_prompt(Prompt::Location),
            0x31 if !self.trash => self.name_prompt(Prompt::Folder),
            0x14 if !self.trash => self.name_prompt(Prompt::Text),
            0x13 if !self.trash => self.name_prompt(Prompt::Rename),
            0x2e | 0x2d if !self.trash => {
                self.clipboard = Some((
                    self.selected_path()?,
                    self.entries[self.selected].directory,
                    key == 0x2d,
                ));
                self.status = "Navigate to destination; V to paste".to_string();
            }
            0x2f if !self.trash => self.paste(b)?,
            0xe053 if !self.trash => self.trash_selected(b)?,
            0x16 if self.trash => self.restore(b)?,
            0x20 => {
                self.selected_path()?;
                self.name_prompt(Prompt::Delete);
            }
            0x1c => {
                let mut path = self.selected_path()?;
                let mut dir = self.entries[self.selected].directory;
                if self.trash {
                    path = child(&path, "data")?;
                    // Trash root entries are containers, not payload types.
                    dir = match b.list(&path) {
                        Ok(_) => true,
                        Err(Error::NotDirectory) => false,
                        Err(e) => return Err(e),
                    };
                }
                if dir {
                    if self.trash {
                        self.status = "U restore; D permanently delete tree".to_string();
                    } else {
                        self.path = path;
                        self.selected = 0;
                        self.refresh(b)?;
                    }
                } else {
                    let bytes = b.read(&path)?;
                    let text = core::str::from_utf8(&bytes).map_err(|_| Error::InvalidArgument)?;
                    if text
                        .chars()
                        .any(|c| c.is_control() && c != '\n' && c != '\r' && c != '\t')
                    {
                        return Err(Error::InvalidArgument);
                    }
                    let mut lines = Vec::new();
                    for line in text.lines() {
                        let chars: Vec<char> = line
                            .chars()
                            .map(|c| {
                                if c.is_ascii_graphic() || c == ' ' {
                                    c
                                } else {
                                    '?'
                                }
                            })
                            .collect();
                        if chars.is_empty() {
                            lines.push(String::new());
                        }
                        for chunk in chars.chunks(39) {
                            lines.push(chunk.iter().collect());
                        }
                    }
                    self.viewer = Some(lines);
                    self.view_line = 0;
                    self.status = "Text viewer (read only); Backspace closes".to_string();
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn submit(&mut self, b: &mut impl Backend, prompt: Prompt, input: &str) -> Result<()> {
        match prompt {
            Prompt::Location => {
                let path = location(input)?;
                b.list(&path)?;
                self.path = path;
                self.selected = 0;
                self.refresh(b)?;
            }
            Prompt::Folder | Prompt::Text | Prompt::Rename => {
                let dest = child(&self.path, input)?;
                if protected(&dest) {
                    return Err(Error::PermissionDenied);
                }
                match prompt {
                    Prompt::Folder => self.enqueue(Step::Mkdir(dest)),
                    Prompt::Text => self.enqueue(Step::Create(dest, b"New text file.\n".to_vec())),
                    _ => self.enqueue(Step::Rename(
                        self.target
                            .as_ref()
                            .map(|t| t.0.clone())
                            .map_or_else(|| self.selected_path(), Ok)?,
                        dest,
                    )),
                }
            }
            Prompt::Delete => {
                // Lowercase input is supplied by the name-entry keyboard.
                if input != "delete" {
                    self.status = "Cancelled: type delete to confirm".to_string();
                    return Ok(());
                }
                let (path, directory) = match &self.target {
                    Some(target) => target.clone(),
                    None => (self.selected_path()?, self.entries[self.selected].directory),
                };
                let mut nodes = Vec::new();
                tree(b, &path, directory, &mut nodes)?;
                for (p, _) in nodes.into_iter().rev() {
                    self.queue.push_back(Step::Remove(p));
                }
                self.queue.push_back(Step::Sync);
            }
        }
        Ok(())
    }
    fn paste(&mut self, b: &mut impl Backend) -> Result<()> {
        let (from, dir, moving) = self.clipboard.clone().ok_or(Error::NotFound)?;
        let name = from.rsplit('/').next().ok_or(Error::InvalidArgument)?;
        let to = child(&self.path, name)?;
        if protected(&to) {
            return Err(Error::PermissionDenied);
        }
        if to == from || to.starts_with(&format!("{from}/")) {
            return Err(Error::InvalidArgument);
        }
        if b.list(&self.path)?.iter().any(|e| e.name == name) {
            return Err(Error::AlreadyExists);
        }
        if moving {
            self.enqueue(Step::Rename(from, to));
            self.clipboard = None;
        } else {
            let mut nodes = Vec::new();
            tree(b, &from, dir, &mut nodes)?;
            let mut steps = VecDeque::new();
            for (p, d) in nodes {
                let dest = format!("{to}{}", &p[from.len()..]);
                if dest.len() > 120 {
                    return Err(Error::TooLarge);
                }
                steps.push_back(if d {
                    Step::Mkdir(dest)
                } else {
                    Step::Copy(p, dest)
                });
            }
            steps.push_back(Step::Sync);
            self.queue = steps;
        }
        Ok(())
    }
    fn trash_selected(&mut self, b: &mut impl Backend) -> Result<()> {
        let original = self.selected_path()?;
        match b.list(TRASH) {
            Ok(_) => {}
            Err(Error::NotFound) => b.mkdir(TRASH)?,
            Err(e) => return Err(e),
        }
        let used = b.list(TRASH)?;
        let name = (0..LIMIT)
            .map(|i| format!("t{i:02}"))
            .find(|name| !used.iter().any(|e| e.name == *name))
            .ok_or(Error::NoSpace)?;
        let container = child(TRASH, &name)?;
        let payload = child(&container, "data")?;
        // Preflight expanded descendant paths before creating anything.
        let mut nodes = Vec::new();
        tree(
            b,
            &original,
            self.entries[self.selected].directory,
            &mut nodes,
        )?;
        if nodes
            .iter()
            .any(|(p, _)| payload.len() + p.len() - original.len() > 120)
        {
            return Err(Error::TooLarge);
        }
        self.queue.push_back(Step::Mkdir(container.clone()));
        self.queue.push_back(Step::Create(
            child(&container, "origin")?,
            format!("RST1\n{original}").into_bytes(),
        ));
        self.queue.push_back(Step::Sync); // Persist recovery information before moving.
        self.queue.push_back(Step::Rename(original, payload));
        self.queue.push_back(Step::Sync);
        Ok(())
    }
    pub fn original(&self, b: &mut impl Backend) -> Result<String> {
        let bytes = b.read(&child(&self.selected_path()?, "origin")?)?;
        decode_origin(&bytes)
    }
    fn restore(&mut self, b: &mut impl Backend) -> Result<()> {
        let container = self.selected_path()?;
        let origin = self.original(b)?;
        self.queue
            .push_back(Step::Rename(child(&container, "data")?, origin));
        self.queue.push_back(Step::Sync); // Keep origin metadata until restore is persisted.
        self.queue
            .push_back(Step::Remove(child(&container, "origin")?));
        self.queue.push_back(Step::Remove(container));
        self.queue.push_back(Step::Sync);
        Ok(())
    }
    /// One action or one sync sector per tick. Failed copies retain their source;
    /// partial destinations / pending Trash records are kept for explicit review.
    pub fn tick(&mut self, b: &mut impl Backend) -> bool {
        let Some(step) = self.queue.pop_front() else {
            return false;
        };
        let result = match &step {
            Step::Mkdir(p) => b.mkdir(p),
            Step::Create(p, data) => b.create(p, data),
            Step::Copy(from, to) => b.read(from).and_then(|data| b.create(to, &data)),
            Step::Rename(from, to) => b.rename(from, to),
            Step::Remove(p) => b.remove(p),
            Step::Sync => match b.sync_step() {
                Ok(false) => {
                    self.queue.push_front(step);
                    self.status = "Syncing; keep VM running".to_string();
                    return true;
                }
                other => other.map(drop),
            },
        };
        match result {
            Ok(()) if self.queue.is_empty() => {
                self.status = "Complete (RAM is temporary)".to_string();
                if let Err(e) = self.refresh(b) {
                    self.report(e);
                }
            }
            Ok(()) => self.status = "Working; keep VM running".to_string(),
            Err(e) => {
                self.queue.clear();
                self.status = format!("{:?}; partial work kept; refresh", e);
                let _ = self.refresh(b);
            }
        }
        true
    }
    fn show_text(&mut self, text: &str) {
        let chars: Vec<char> = text.chars().collect();
        self.viewer = Some(
            chars
                .chunks(39)
                .map(|chunk| chunk.iter().collect())
                .collect(),
        );
        self.view_line = 0;
    }
    pub fn click(&mut self, b: &mut impl Backend, y: i32) {
        // Rows occupy y=56..122 in client coordinates. Click selects, Enter opens.
        if (56..122).contains(&y) && self.prompt.is_none() && !self.busy() && self.viewer.is_none()
        {
            let start = self.selected / 3 * 3;
            self.selected =
                (start + (y as usize - 56) / 22).min(self.entries.len().saturating_sub(1));
        } else if (12..34).contains(&y) {
            self.key(b, 0x0e);
        }
    }
    pub fn lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        out.push(format!(
            "{} {}",
            if self.trash { "Trash" } else { "Files" },
            self.path
        ));
        if let Some(prompt) = self.prompt {
            out.push(
                match prompt {
                    Prompt::Folder => "New folder name:",
                    Prompt::Text => "New text file name:",
                    Prompt::Rename => "Rename to:",
                    Prompt::Location => "Absolute location:",
                    Prompt::Delete => "PERMANENT DELETE: type delete + Enter",
                }
                .to_string(),
            );
            if let Some((path, _)) = &self.target {
                out.push(format!("Target: {path}"));
            }
            let chars: Vec<char> = self.input.chars().collect();
            out.push(chars[chars.len().saturating_sub(39)..].iter().collect());
        } else if let Some(lines) = &self.viewer {
            out.push("Read only; Up/Down scroll".to_string());
            out.extend(lines.iter().skip(self.view_line).take(3).cloned());
        } else {
            out.push(
                if self.trash {
                    "Enter view U restore D permanent delete"
                } else {
                    "Enter open Back parent F5 refresh H help"
                }
                .to_string(),
            );
            let start = self.selected / 3 * 3;
            for (i, e) in self.entries.iter().enumerate().skip(start).take(3) {
                let label = if self.trash {
                    self.labels
                        .get(i)
                        .cloned()
                        .unwrap_or_else(|| e.name.clone())
                } else {
                    format!(
                        "{} {} {}",
                        if e.directory { "DIR" } else { "FILE" },
                        e.size.map(|n| format!("{n}B")).unwrap_or_default(),
                        e.name
                    )
                };
                out.push(format!(
                    "{} {label}",
                    if i == self.selected { ">" } else { " " }
                ));
            }
            if self.entries.is_empty() {
                out.push("(empty)".to_string());
            }
        }
        while out.len() < 5 {
            out.push(String::new());
        }
        // Wrap status so errors and controls can be read on the fixed surface.
        let chars: Vec<char> = self.status.chars().collect();
        for part in chars.chunks(39).take(2) {
            out.push(part.iter().collect());
        }
        out
    }
}
fn ascii(k: u32) -> Option<char> {
    let keys = [
        (0x02, "1234567890-="),
        (0x10, "qwertyuiop"),
        (0x1e, "asdfghjkl"),
        (0x2c, "zxcvbnm"),
    ];
    for (start, letters) in keys {
        if k >= start && k < start + letters.len() as u32 {
            return letters.chars().nth((k - start) as usize);
        }
    }
    match k {
        0x39 => Some(' '),
        0x34 => Some('.'),
        0x35 => Some('/'),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
