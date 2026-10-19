//! Cohesive shell policy and built-in application views, running in user space.
//! No hardware access: the runtime supplies volume state and handles persistence.
use crate::display::{Action, Display, Error, Event};
use gfx::{Canvas, Rect, Rgb, TextStyle};

pub const APPS: [&str; 5] = ["Computer", "Files", "Trash", "Settings", "About"];
pub const PREF_PATH: &str = "/desktop.pref";
const OWNER: u32 = 100;
const WIDTH: u32 = 400;
const HEIGHT: u32 = 170;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Preferences {
    pub warm: bool,
}
impl Preferences {
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        match bytes {
            b"RSP1\0" => Some(Self { warm: false }),
            b"RSP1\x01" => Some(Self { warm: true }),
            _ => None,
        }
    }
    pub fn encode(self) -> [u8; 5] {
        [b'R', b'S', b'P', b'1', u8::from(self.warm)]
    }
}

#[derive(Debug)]
pub struct Shell {
    pub preferences: Preferences,
    pub windows: [Option<u32>; 5],
    pub menu: bool,
    pub selection: usize,
    pub persistent: bool,
    pub status: &'static str,
}
impl Shell {
    pub fn new(preferences: Preferences, persistent: bool) -> Self {
        Self {
            preferences,
            windows: [None; 5],
            menu: false,
            selection: 0,
            persistent,
            status: if persistent {
                "Persistent volume: preferences can be saved"
            } else {
                "Temporary volume: changes last this boot"
            },
        }
    }
    pub fn launch(&mut self, display: &mut Display, app: usize) -> Result<(), Error> {
        let title = APPS.get(app).ok_or(Error::Invalid)?;
        self.menu = false;
        if let Some(id) = self.windows[app] {
            return display.raise(id);
        }
        let mut packet = [0; 48];
        packet[0] = 1;
        packet[1] = 1;
        packet[8..12].copy_from_slice(&WIDTH.to_le_bytes());
        packet[12..16].copy_from_slice(&HEIGHT.to_le_bytes());
        packet[16..16 + title.len()].copy_from_slice(title.as_bytes());
        let id = display.request(OWNER, &packet[..16 + title.len()])?;
        self.windows[app] = Some(id);
        display.action(
            id,
            Action::Move(160 + app as i32 * 38, 100 + app as i32 * 38),
        )?;
        self.paint(display, app, "")
    }
    pub fn app_for(&self, id: u32) -> Option<usize> {
        self.windows.iter().position(|w| *w == Some(id))
    }
    pub fn close(&mut self, display: &mut Display, id: u32) -> Result<(), Error> {
        display.action(id, Action::Close)?;
        if let Some(app) = self.app_for(id) {
            self.windows[app] = None;
        }
        Ok(())
    }
    /// Tab cycles every live app, including minimized ones, in stable launch order.
    pub fn switch(&self, display: &mut Display) -> Result<(), Error> {
        let start = display
            .focus()
            .and_then(|id| self.app_for(id))
            .map_or(0, |n| n + 1);
        for offset in 0..5 {
            if let Some(id) = self.windows[(start + offset) % 5] {
                return display.raise(id);
            }
        }
        Ok(())
    }
    /// Returns true when the runtime must save the changed preference.
    pub fn activate(&mut self, display: &mut Display, id: u32, event: Event) -> bool {
        if self.app_for(id) == Some(3)
            && matches!(
                event,
                Event::Key {
                    code: 0x1c | 0x39,
                    pressed: true
                } | Event::Pointer {
                    x: 16..=380,
                    y: 30..=54,
                    pressed: true
                }
            )
        {
            self.preferences.warm = !self.preferences.warm;
            return true;
        }
        let _ = display;
        false
    }
    pub fn paint(&self, display: &mut Display, app: usize, listing: &str) -> Result<(), Error> {
        let Some(id) = self.windows[app] else {
            return Ok(());
        };
        let mut fill = [0; 27];
        fill[0] = 1;
        fill[1] = 2;
        fill[4..8].copy_from_slice(&id.to_le_bytes());
        fill[16..20].copy_from_slice(&WIDTH.to_le_bytes());
        fill[20..24].copy_from_slice(&HEIGHT.to_le_bytes());
        fill[24..].copy_from_slice(&[238, 242, 246]);
        display.request(OWNER, &fill)?;
        let lines: [&str; 6] = match app {
            0 => [
                "Computer / mounted volume",
                if self.persistent {
                    "RSFS1: persistent guest data disk"
                } else {
                    "RAM: temporary guest volume"
                },
                "64 entries, 4096 bytes per file",
                "Shared by bundled applications",
                "Open Files from Applications or desktop",
                "",
            ],
            1 => [
                "Files / (read-only browser)",
                "Directories end in /",
                listing,
                "",
                "Refresh: close and reopen Files",
                "File editing is unavailable",
            ],
            2 => [
                "Trash is unavailable",
                "Move-to-trash and restore are not",
                "implemented for this volume.",
                "No files have been removed.",
                "",
                "",
            ],
            3 => [
                "Desktop appearance",
                if self.preferences.warm {
                    "[ Warm accent ]  Enter / click to change"
                } else {
                    "[ Cool accent ]  Enter / click to change"
                },
                self.status,
                "Keyboard layout: US only",
                "Display mode: firmware supplied",
                "Other settings are unavailable",
            ],
            _ => [
                "rust-os / experimental desktop",
                concat!("Version ", env!("CARGO_PKG_VERSION")),
                if cfg!(debug_assertions) {
                    "Build: debug / ABI 1 / x86_64"
                } else {
                    "Build: release / ABI 1 / x86_64"
                },
                "Rust kernel and ring-3 shell",
                "MIT license / Noto font OFL",
                "No wall clock; panel shows uptime",
            ],
        };
        let mut row = 0;
        for line in lines {
            // Listings may contain newlines; all text is sanitized to the protocol's ASCII alphabet.
            for text in line.split('\n') {
                if row >= 7 {
                    break;
                }
                let mut packet = [0; 147];
                packet[0] = 1;
                packet[1] = 5;
                packet[4..8].copy_from_slice(&id.to_le_bytes());
                packet[8..12].copy_from_slice(&16u32.to_le_bytes());
                packet[12..16].copy_from_slice(&(12u32 + row * 22).to_le_bytes());
                packet[16..19].copy_from_slice(&[34, 44, 58]);
                let len = text.len().min(40);
                if len > 0 {
                    for (dest, src) in packet[19..19 + len].iter_mut().zip(text.bytes()) {
                        *dest = if (32..127).contains(&src) { src } else { b'?' };
                    }
                    display.request(OWNER, &packet[..19 + len])?;
                }
                row += 1;
            }
        }
        let mut present = [1, 3, 0, 0, 0, 0, 0, 0];
        present[4..].copy_from_slice(&id.to_le_bytes());
        display.request(OWNER, &present)?;
        Ok(())
    }
    pub fn background(&self, canvas: &mut Canvas<'_>) {
        let bg = if self.preferences.warm {
            Rgb::new(65, 42, 52)
        } else {
            Rgb::new(29, 38, 59)
        };
        canvas.clear(bg);
        for (i, title) in APPS[..3].iter().enumerate() {
            let y = 72 + i as i32 * 100;
            let accent = Rgb::new(91, 160, 187);
            match i {
                0 => {
                    canvas.fill_rect(Rect::new(30, y, 56, 36), accent);
                    canvas.fill_rect(Rect::new(34, y + 4, 48, 28), bg);
                    canvas.fill_rect(Rect::new(54, y + 36, 8, 7), accent);
                    canvas.fill_rect(Rect::new(42, y + 43, 32, 3), accent);
                }
                1 => {
                    canvas.fill_rect(Rect::new(30, y + 4, 24, 8), accent);
                    canvas.fill_rect(Rect::new(30, y + 12, 56, 32), accent);
                    canvas.fill_rect(Rect::new(34, y + 18, 48, 22), bg);
                }
                _ => {
                    let disabled = Rgb::new(145, 151, 166);
                    canvas.fill_rect(Rect::new(36, y + 8, 44, 36), disabled);
                    canvas.fill_rect(Rect::new(40, y + 12, 36, 28), bg);
                    canvas.fill_rect(Rect::new(32, y + 4, 52, 3), disabled);
                    for dx in [48, 58, 68] {
                        canvas.fill_rect(Rect::new(dx, y + 16, 2, 20), disabled);
                    }
                }
            }
            canvas.draw_text(
                16,
                y + 48,
                title,
                TextStyle::Body,
                Rgb::new(239, 244, 250),
                bg,
            );
        }
    }
    pub fn overlay(&self, canvas: &mut Canvas<'_>, uptime: &str) {
        let panel = Rgb::new(24, 29, 38);
        let ink = Rgb::new(236, 242, 248);
        canvas.fill_rect(Rect::new(0, 0, canvas.width() as u32, 36), panel);
        canvas.draw_text(16, 10, "Applications [Esc]", TextStyle::Strong, ink, panel);
        canvas.draw_text(
            250,
            10,
            "Tab: switch  F1: hide  F2: restore  F3: close",
            TextStyle::Body,
            ink,
            panel,
        );
        canvas.draw_text(
            canvas.width() as i32 - 190,
            10,
            uptime,
            TextStyle::Body,
            ink,
            panel,
        );
        if self.menu {
            canvas.fill_rect(Rect::new(8, 40, 310, 300), panel);
            for (i, title) in APPS
                .iter()
                .chain(
                    [
                        "Shut Down - unavailable",
                        "Restart - unavailable",
                        "Sleep - unavailable",
                    ]
                    .iter(),
                )
                .enumerate()
            {
                let selected = self.selection == i && i < 5;
                let bg = if selected {
                    Rgb::new(56, 107, 150)
                } else {
                    panel
                };
                let color = if i < 5 { ink } else { Rgb::new(133, 143, 155) };
                canvas.fill_rect(Rect::new(16, 48 + i as i32 * 34, 294, 30), bg);
                canvas.draw_text(24, 55 + i as i32 * 34, title, TextStyle::Body, color, bg);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preferences_reject_corrupt_or_unknown_versions() {
        for warm in [false, true] {
            let p = Preferences { warm };
            assert_eq!(Preferences::decode(&p.encode()), Some(p));
        }
        assert_eq!(Preferences::decode(b"RSP1\x02"), None);
        assert_eq!(Preferences::decode(b"RSP1\0extra"), None);
    }
    #[test]
    fn shell_launch_switch_hide_restore_close_relaunch() {
        let mut d = Display::new(1280, 800).unwrap();
        let mut s = Shell::new(Preferences::default(), false);
        s.launch(&mut d, 3).unwrap();
        let first = s.windows[3].unwrap();
        s.launch(&mut d, 4).unwrap();
        let second = s.windows[4].unwrap();
        d.action(second, Action::Minimize).unwrap();
        s.switch(&mut d).unwrap();
        assert_eq!(d.focus(), Some(second));
        s.close(&mut d, first).unwrap();
        s.launch(&mut d, 3).unwrap();
        assert_ne!(s.windows[3], Some(first));
        let id = s.windows[3].unwrap();
        assert!(s.activate(
            &mut d,
            id,
            Event::Key {
                code: 0x1c,
                pressed: true
            }
        ));
        assert!(s.preferences.warm);
        assert!(!s.activate(
            &mut d,
            second,
            Event::Key {
                code: 0x1c,
                pressed: true
            }
        ));
    }
}
