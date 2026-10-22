//! Cohesive shell policy and built-in application views, running in user space.
//! No hardware access: the runtime supplies volume state and handles persistence.
extern crate alloc;
use crate::display::{Action, Display, Error, Event};
use alloc::string::String;
use gfx::{Canvas, Rect, Rgb, TextStyle};

pub const APPS: [&str; 6] = [
    "Computer",
    "Files",
    "Trash",
    "Settings",
    "About",
    "Task Manager",
];
pub const MENU_COUNT: usize = APPS.len() + 3;
pub const TASK_MANAGER: usize = 5;
pub const PREF_PATH: &str = "/desktop.pref";
const OWNER: u32 = 100;
const WIDTH: u32 = 480;
const HEIGHT: u32 = 236;

/// Integer, clock-driven ease-out; no allocation, floating point or frame count
/// dependency. Retargeting starts at the current value, avoiding visible jumps.
#[derive(Debug, Default)]
struct Tween {
    value: i32,
    from: i32,
    target: i32,
    start_ns: u64,
}
impl Tween {
    fn target(&mut self, target: i32, now: u64) {
        if self.target != target {
            self.from = self.value;
            self.target = target;
            self.start_ns = now;
        }
    }
    fn tick(&mut self, now: u64) -> bool {
        let before = self.value;
        let t = (now.saturating_sub(self.start_ns).min(140_000_000) * 1024 / 140_000_000) as i64;
        let remaining = 1024 - t;
        let eased = 1024 - remaining * remaining * remaining / (1024 * 1024);
        self.value =
            (i64::from(self.from) + i64::from(self.target - self.from) * eased / 1024) as i32;
        self.value != before
    }
}

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
    pub windows: [Option<u32>; 6],
    pub menu: bool,
    pub selection: usize,
    pub launcher: crate::launcher::Launcher,
    pub persistent: bool,
    pub status: &'static str,
    pub power_bits: u64,
    pub power_confirm: Option<u64>,
    pub unsaved_preferences: bool,
    pub power_notice: bool,
    pub reduced_motion: bool,
    menu_motion: Tween,
    selection_motion: Tween,
    hover_icon: Option<usize>,
    hover_launcher: bool,
    hover_dock: Option<usize>,
    hover_workspace: Option<usize>,
    pub snapshot: os_abi::monitor::Snapshot,
    pub snapshot_available: bool,
    pub task_selected: usize,
    pub task_confirm: Option<u32>,
    pub process_view: bool,
    pub process_offset: usize,
    settings: crate::iced::Settings,
}
impl Shell {
    pub fn new(preferences: Preferences, persistent: bool) -> Self {
        Self {
            preferences,
            windows: [None; 6],
            menu: false,
            selection: 0,
            launcher: crate::launcher::Launcher::default(),
            power_bits: 0,
            power_confirm: None,
            unsaved_preferences: false,
            power_notice: false,
            reduced_motion: false,
            persistent,
            menu_motion: Tween::default(),
            selection_motion: Tween::default(),
            hover_icon: None,
            hover_launcher: false,
            hover_dock: None,
            hover_workspace: None,
            snapshot: os_abi::monitor::Snapshot::default(),
            snapshot_available: false,
            task_selected: 0,
            task_confirm: None,
            process_view: false,
            process_offset: 0,
            settings: crate::iced::Settings::default(),
            status: if persistent {
                "Persistent volume: preferences can be saved"
            } else {
                "Temporary volume: changes last this boot"
            },
        }
    }
    pub fn menu_offset(&self) -> i32 {
        -12 * (256 - self.menu_motion.value) / 256
    }
    pub fn toggle_menu(&mut self) {
        self.menu = !self.menu;
        if self.menu {
            self.launcher.clear();
            self.selection = 0;
            self.selection_motion = Tween::default();
        }
    }
    /// Escape first clears a query, leaving the complete app catalog visible.
    pub fn dismiss_menu(&mut self) {
        if self.menu && self.launcher.clear() {
            self.selection = 0;
            self.selection_motion = Tween::default();
        } else {
            self.toggle_menu();
        }
    }
    pub fn menu_row_height(height: u32) -> i32 {
        if height < 600 { 24 } else { 34 }
    }
    pub fn menu_hit(&self, x: i32, y: i32, height: u32) -> Option<usize> {
        let top = 96 + self.menu_offset();
        let row_height = Self::menu_row_height(height);
        if !(16..388).contains(&x) || y < top {
            return None;
        }
        let row = ((y - top) / row_height) as usize;
        self.launcher.result(row).map(|_| row)
    }
    /// Consumes launcher keyboard input without delivering it to an application.
    pub fn menu_key(&mut self, code: u32) -> Option<usize> {
        let count = self.launcher.result_count();
        match code {
            0xe048 | 0xe050 if count > 0 => {
                self.selection =
                    (self.selection + if code == 0xe048 { count - 1 } else { 1 }) % count;
            }
            0x1c => return self.launcher.result(self.selection),
            _ => {
                let changed = if code == 0x0e {
                    self.launcher.backspace()
                } else {
                    crate::launcher::scancode_ascii(code)
                        .is_some_and(|c| self.launcher.input_ascii(c))
                };
                if changed {
                    self.selection = 0;
                    self.selection_motion = Tween::default();
                }
            }
        }
        None
    }
    /// Called only for real pointer motion so keyboard selection is preserved.
    pub fn pointer_motion(
        &mut self,
        x: i32,
        y: i32,
        over_window: bool,
        width: u32,
        height: u32,
    ) -> bool {
        let old = (
            self.hover_icon,
            self.hover_launcher,
            self.selection,
            self.hover_dock,
            self.hover_workspace,
        );
        self.hover_workspace = crate::workspaces::hit(width, height, x, y);
        self.hover_dock = crate::dock::hit(width, height, x, y);
        self.hover_launcher = (0..220).contains(&x) && (0..36).contains(&y);
        self.hover_icon =
            if !self.menu && !over_window && (16..112).contains(&x) && (72..372).contains(&y) {
                Some(((y - 72) / 100) as usize)
            } else {
                None
            };
        if self.menu
            && let Some(row) = self.menu_hit(x, y, height)
        {
            self.selection = row;
        }
        old != (
            self.hover_icon,
            self.hover_launcher,
            self.selection,
            self.hover_dock,
            self.hover_workspace,
        )
    }
    pub fn animate(&mut self, now: u64) -> bool {
        self.menu_motion
            .target(if self.menu { 256 } else { 0 }, now);
        self.selection_motion
            .target(self.selection.min(MENU_COUNT - 1) as i32 * 34 * 256, now);
        let time = if self.reduced_motion {
            now.saturating_add(140_000_000)
        } else {
            now
        };
        let menu = self.menu_motion.tick(time);
        let selection = self.selection_motion.tick(time);
        menu || ((self.menu || self.menu_motion.value > 0) && selection)
    }
    pub fn animations_settled(&self) -> bool {
        self.menu_motion.value == self.menu_motion.target
            && self.selection_motion.value == self.selection_motion.target
    }
    /// Dialog contents and failed preference saves veto exit; never discard them.
    pub fn request_power(&mut self, action: u64, dialog_open: bool) {
        self.menu = false;
        self.power_confirm = None;
        self.power_notice = true;
        if !(1..=2).contains(&action) || self.power_bits & (1 << (action - 1)) == 0 {
            self.status = "Unavailable: platform support or real resume drivers missing";
        } else if dialog_open || self.unsaved_preferences {
            self.status = "Power cancelled: close dialogs and save preferences first";
        } else {
            self.power_confirm = Some(action);
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
        if app == TASK_MANAGER && self.windows[self.task_selected].is_none() {
            self.task_selected = self
                .windows
                .iter()
                .position(Option::is_some)
                .unwrap_or(TASK_MANAGER);
        }
        display.action(
            id,
            Action::Move(160 + app as i32 * 38, 100 + app as i32 * 38),
        )?;
        self.paint(display, app, "")
    }
    pub fn app_for(&self, id: u32) -> Option<usize> {
        self.windows.iter().position(|w| *w == Some(id))
    }
    pub fn dock_activate(&mut self, display: &mut Display, app: usize) -> Result<(), Error> {
        if let Some(id) = self.windows.get(app).copied().flatten() {
            if display.focus() == Some(id) {
                display.action(id, Action::Minimize)
            } else {
                display.raise(id)
            }
        } else {
            self.launch(display, app)
        }
    }
    pub fn work_area(width: u32, height: u32) -> Rect {
        Rect::new(8, 44, width.saturating_sub(16), height.saturating_sub(140))
    }
    pub fn task_key(&mut self, display: &mut Display, code: u32) -> Result<(), Error> {
        if let Some(id) = self.task_confirm {
            match code {
                0x01 => self.task_confirm = None,
                0x1c => {
                    self.task_confirm = None;
                    if display.window_info(id).is_some() {
                        self.close(display, id)?;
                    }
                }
                _ => {}
            }
            return Ok(());
        }
        if code == 0x19 {
            // P: process telemetry vs desktop task controls.
            self.process_view = !self.process_view;
            return Ok(());
        }
        if self.process_view {
            match code {
                0xe048 => self.process_offset = self.process_offset.saturating_sub(1),
                0xe050 => {
                    self.process_offset =
                        (self.process_offset + 1).min(self.snapshot.count.saturating_sub(6))
                }
                _ => {}
            }
            return Ok(());
        }
        if matches!(code, 0xe048 | 0xe050) {
            for step in 1..=APPS.len() {
                let app = (self.task_selected
                    + if code == 0xe048 {
                        APPS.len() - step
                    } else {
                        step
                    })
                    % APPS.len();
                if self.windows[app].is_some() {
                    self.task_selected = app;
                    break;
                }
            }
        } else if let Some(id) = self.windows[self.task_selected] {
            match code {
                0x1c => {
                    display.raise(id)?;
                }
                0x32 => {
                    display.action(id, Action::Minimize)?;
                }
                0xe053 => self.task_confirm = Some(id),
                _ => {}
            }
        }
        Ok(())
    }
    pub fn task_text(&self, display: &Display) -> String {
        use core::fmt::Write;
        let mut text = String::new();
        if self.snapshot_available {
            let _ = writeln!(
                text,
                "{} processes | {} threads | {} MiB free",
                self.snapshot.count,
                self.snapshot.threads,
                self.snapshot.free_bytes / (1024 * 1024)
            );
        } else {
            text.push_str("System metrics unavailable\n");
        }
        if let Some(id) = self.task_confirm {
            let name = self.app_for(id).map_or("closed task", |a| APPS[a]);
            let _ = writeln!(text, "Close {name}? Enter: close / Esc: cancel");
            text.push_str(
                "[ Close ]               [ Cancel ]\nFile jobs continue after their view closes.",
            );
        } else if self.process_view {
            text.push_str("Processes / P: Apps / Up,Down: scroll\n");
            for p in self.snapshot.processes[..self.snapshot.count]
                .iter()
                .skip(self.process_offset)
                .take(6)
            {
                let name = core::str::from_utf8(&p.name[..p.name_len as usize]).unwrap_or("?");
                let _ = writeln!(
                    text,
                    "PID {} {} | CPU {} ms | {} handles",
                    p.pid,
                    name,
                    p.cpu_ns / 1_000_000,
                    p.handles
                );
            }
            text.push_str("Read-only process telemetry; no force kill.");
        } else {
            text.push_str("Apps / P: Processes / Up,Down: select\n");
            for (app, id) in self.windows.iter().enumerate() {
                let state =
                    id.and_then(|id| display.window_info(id))
                        .map_or("Not running", |info| {
                            if info.minimized {
                                "Minimized"
                            } else if info.focused {
                                "Focused"
                            } else {
                                "Running"
                            }
                        });
                let _ = writeln!(
                    text,
                    "{} {:<13} {}{}",
                    if self.task_selected == app { ">" } else { " " },
                    APPS[app],
                    state,
                    id.and_then(|id| display.workspace_of(id))
                        .map_or_else(String::new, |w| alloc::format!(" / W{}", w + 1))
                );
            }
            text.push_str(
                "[ Focus ]       [ Minimize ]    [ Close ]\nEnter / M / Del | Shared shell process",
            );
        }
        text
    }
    pub fn close(&mut self, display: &mut Display, id: u32) -> Result<(), Error> {
        display.action(id, Action::Close)?;
        if let Some(app) = self.app_for(id) {
            self.windows[app] = None;
            if app == TASK_MANAGER {
                self.task_confirm = None;
            }
        }
        if self.task_confirm == Some(id) {
            self.task_confirm = None;
        }
        Ok(())
    }
    /// Tab cycles live apps on the current workspace, including minimized ones.
    pub fn switch(&self, display: &mut Display) -> Result<(), Error> {
        let start = display
            .focus()
            .and_then(|id| self.app_for(id))
            .map_or(0, |n| n + 1);
        for offset in 0..APPS.len() {
            if let Some(id) = self.windows[(start + offset) % APPS.len()]
                && display.workspace_of(id) == Some(display.current_workspace())
            {
                return display.raise(id);
            }
        }
        Ok(())
    }
    /// Returns true when the runtime must save the changed preference.
    pub fn activate(&mut self, display: &mut Display, id: u32, event: Event) -> bool {
        if self.app_for(id) != Some(3) {
            return false;
        }
        if let Some(rect) = display.geometry(id) {
            self.settings
                .set_viewport(rect.w, rect.h.saturating_sub(24));
        }
        let update = self.settings.event(self.settings_model(), event);
        let save = match update.message {
            Some(crate::iced::Message::ToggleAccent) => {
                self.preferences.warm = !self.preferences.warm;
                true
            }
            Some(crate::iced::Message::ToggleMotion) => {
                self.reduced_motion = !self.reduced_motion;
                false
            }
            None => false,
        };
        if update.redraw || update.message.is_some() {
            let _ = self.paint(display, 3, "");
        }
        save
    }

    fn settings_model(&self) -> crate::iced::Model<'static> {
        crate::iced::Model {
            warm: self.preferences.warm,
            reduced_motion: self.reduced_motion,
            status: self.status,
        }
    }

    /// Clear control capture when another shell surface owns input. A later
    /// release must never activate a button held before focus/workspace loss.
    pub fn sync_controls(&mut self, display: &mut Display) {
        let active = self.windows[3].is_some_and(|id| {
            display.focus() == Some(id)
                && display
                    .window_info(id)
                    .is_some_and(|w| !w.minimized && w.workspace == display.current_workspace())
        });
        if !active || self.menu || self.power_confirm.is_some() {
            self.cancel_controls(display);
        }
    }

    /// Input loss and window-manager capture invalidate pending widget presses.
    pub fn cancel_controls(&mut self, display: &mut Display) {
        let update = self.settings.cancel(self.settings_model());
        if update.redraw {
            let _ = self.paint(display, 3, "");
        }
    }

    /// Route hover from window-local coordinates. Leaving the Settings window
    /// also cancels a pending click, including motion over another shell view.
    pub fn controls_motion(&mut self, display: &mut Display, x: i32, y: i32) {
        let delivery = display.motion(x, y).filter(|d| {
            self.app_for(d.window) == Some(3) && display.focus() == Some(d.window) && !self.menu
        });
        if let Some(d) = delivery {
            self.activate(display, d.window, d.event);
        } else {
            self.cancel_controls(display);
        }
    }

    pub fn paint(&mut self, display: &mut Display, app: usize, listing: &str) -> Result<(), Error> {
        let Some(id) = self.windows[app] else {
            return Ok(());
        };
        if app == 3 {
            let model = self.settings_model();
            return display.paint_surface(OWNER, id, |pixels, width, height| {
                self.settings.draw(model, pixels, width, height);
            });
        }
        let mut fill = [0; 27];
        fill[0] = 1;
        fill[1] = 2;
        fill[4..8].copy_from_slice(&id.to_le_bytes());
        fill[16..20].copy_from_slice(&WIDTH.to_le_bytes());
        fill[20..24].copy_from_slice(&HEIGHT.to_le_bytes());
        fill[24..].copy_from_slice(&[238, 242, 246]);
        display.request(OWNER, &fill)?;
        if app == TASK_MANAGER {
            let mut rectangles = [(Rect::new(8, 32, 464, 22), [220, 231, 242]); 5];
            let mut count = 1;
            if !self.process_view && self.task_confirm.is_none() {
                rectangles[1] = (
                    Rect::new(8, 56 + self.task_selected as i32 * 22, 464, 22),
                    [207, 228, 244],
                );
                for i in 0..3 {
                    rectangles[i + 2] = (
                        Rect::new(12 + i as i32 * 150, 188, 140, 22),
                        [211, 225, 240],
                    );
                }
                count = 5;
            }
            for (rect, color) in &rectangles[..count] {
                fill[8..12].copy_from_slice(&(rect.x as u32).to_le_bytes());
                fill[12..16].copy_from_slice(&(rect.y as u32).to_le_bytes());
                fill[16..20].copy_from_slice(&rect.w.to_le_bytes());
                fill[20..24].copy_from_slice(&rect.h.to_le_bytes());
                fill[24..27].copy_from_slice(color);
                display.request(OWNER, &fill)?;
            }
        }
        let task_text = if app == TASK_MANAGER {
            self.task_text(display)
        } else {
            String::new()
        };
        let lines: [&str; 6] = match app {
            TASK_MANAGER => [&task_text, "", "", "", "", ""],
            0 => [
                "Computer / mounted volume",
                if self.persistent {
                    "RSFS1: persistent guest data disk"
                } else {
                    "RAM: temporary guest volume"
                },
                "64 entries, 4096 bytes per file",
                "Shared by bundled applications",
                "Enter / click to browse mounted /",
                "",
            ],
            1 | 2 => [listing, "", "", "", "", ""],
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
                if row >= 10 {
                    break;
                }
                let mut packet = [0; 147];
                packet[0] = 1;
                packet[1] = 5;
                packet[4..8].copy_from_slice(&id.to_le_bytes());
                packet[8..12].copy_from_slice(&16u32.to_le_bytes());
                packet[12..16].copy_from_slice(&(12u32 + row * 22).to_le_bytes());
                packet[16..19].copy_from_slice(&[34, 44, 58]);
                let len = text.len().min(49);
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
        // Static, allocation-free wallpaper accents stay in the retained scene.
        let right = canvas.width() as i32 - 360;
        let bottom = canvas.height() as i32 - 250;
        canvas.fill_rounded_rect(
            Rect::new(right - 60, bottom - 20, 330, 140),
            50,
            bg.blend(Rgb::new(77, 139, 180), 28),
        );
        canvas.draw_text(
            right,
            bottom + 8,
            "rust-os",
            TextStyle::Title,
            Rgb::new(215, 230, 245),
            bg.blend(Rgb::new(77, 139, 180), 28),
        );
        canvas.draw_text(
            right,
            bottom + 58,
            "Your space. Your applications.",
            TextStyle::Body,
            Rgb::new(157, 181, 209),
            bg.blend(Rgb::new(77, 139, 180), 28),
        );
        for (i, title) in APPS[..3].iter().enumerate() {
            let y = 72 + i as i32 * 100;
            let bg = if self.hover_icon == Some(i) {
                bg.blend(Rgb::new(170, 205, 240), 30)
            } else {
                bg
            };
            if self.hover_icon == Some(i) {
                canvas.fill_rounded_rect(Rect::new(16, y - 4, 96, 86), 8, bg);
            }
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
    pub fn overlay(&self, canvas: &mut Canvas<'_>, uptime: &str, display: &Display) {
        let panel = Rgb::new(24, 29, 38);
        let ink = Rgb::new(236, 242, 248);
        canvas.fill_rect(Rect::new(0, 0, canvas.width() as u32, 36), panel);
        let launcher = if self.hover_launcher || self.menu {
            Rgb::new(46, 64, 85)
        } else {
            panel
        };
        canvas.fill_rounded_rect(Rect::new(8, 4, 210, 28), 6, launcher);
        canvas.draw_text(
            16,
            10,
            "Applications [Esc]",
            TextStyle::Strong,
            ink,
            launcher,
        );
        if canvas.width() >= 1000 {
            let title = display
                .focus()
                .and_then(|id| self.app_for(id))
                .map_or("Desktop", |a| APPS[a]);
            canvas.draw_text(240, 10, title, TextStyle::Body, ink, panel);
        }
        crate::workspaces::paint(canvas, display, self.hover_workspace);
        canvas.draw_text(
            canvas.width() as i32 - 190,
            10,
            uptime,
            TextStyle::Body,
            ink,
            panel,
        );
        if self.menu || self.menu_motion.value > 0 {
            let offset = self.menu_offset();
            let row_height = Self::menu_row_height(canvas.height() as u32);
            let count = self.launcher.result_count();
            let height = 82 + count.max(1) as u32 * row_height as u32;
            canvas.fill_rounded_rect(
                Rect::new(12, 45 + offset, 388, height),
                8,
                Rgb::new(16, 20, 28),
            );
            canvas.fill_rounded_rect(Rect::new(8, 40 + offset, 388, height), 8, panel);
            let search_bg = Rgb::new(37, 47, 64);
            canvas.fill_rounded_rect(Rect::new(16, 48 + offset, 372, 36), 5, search_bg);
            canvas.draw_text(
                26,
                58 + offset,
                if self.launcher.query().is_empty() {
                    "Type to search applications..."
                } else {
                    self.launcher.query()
                },
                TextStyle::Body,
                if self.launcher.query().is_empty() {
                    Rgb::new(154, 170, 190)
                } else {
                    ink
                },
                search_bg,
            );
            let highlight = 96 + offset + self.selection_motion.value * row_height / (34 * 256);
            if count > 0 {
                canvas.fill_rounded_rect(
                    Rect::new(16, highlight, 372, row_height as u32 - 4),
                    5,
                    Rgb::new(56, 107, 150),
                );
            } else {
                canvas.draw_text(
                    26,
                    100 + offset,
                    "No matching applications",
                    TextStyle::Body,
                    ink,
                    panel,
                );
            }
            for i in 0..count {
                let entry = self.launcher.result(i).unwrap_or(0);
                let title = if entry == APPS.len() && self.power_bits & 1 == 0 {
                    "Shut Down - unavailable"
                } else if entry == APPS.len() + 1 && self.power_bits & 2 == 0 {
                    "Restart - unavailable"
                } else if entry == APPS.len() + 2 {
                    "Sleep: resume drivers missing"
                } else {
                    crate::launcher::ENTRIES[entry].label
                };
                let enabled = entry < APPS.len()
                    || (entry == APPS.len() && self.power_bits & 1 != 0)
                    || (entry == APPS.len() + 1 && self.power_bits & 2 != 0);
                let selected = self.selection == i && enabled;
                let bg = if selected {
                    Rgb::new(56, 107, 150)
                } else {
                    panel
                };
                let color = if enabled {
                    ink
                } else {
                    Rgb::new(133, 143, 155)
                };
                // The highlight glides independently; keep glyph anti-aliasing
                // matched to its current row during the short transition.
                let bg = if (96 + offset + i as i32 * row_height - highlight).abs() < 4 && enabled {
                    bg
                } else {
                    panel
                };
                canvas.draw_text(
                    24,
                    99 + offset + i as i32 * row_height,
                    title,
                    TextStyle::Body,
                    color,
                    bg,
                );
            }
            canvas.draw_text(
                24,
                40 + offset + height as i32 - 22,
                "Enter: open  Esc: clear / close",
                TextStyle::Body,
                Rgb::new(154, 170, 190),
                panel,
            );
        }
        crate::dock::paint(
            canvas,
            self.windows.map(|w| w.is_some()),
            display.focus().and_then(|id| self.app_for(id)),
            self.hover_dock,
        );
        if self.power_confirm.is_some() {
            canvas.fill_rect(Rect::new(320, 44, 700, 82), panel);
            canvas.draw_text(
                332,
                54,
                "Exit applications and flush storage? Enter: confirm  Esc: cancel",
                TextStyle::Body,
                ink,
                panel,
            );
            canvas.draw_text(
                332,
                80,
                "RAM volume contents will be lost. Sleep is unavailable.",
                TextStyle::Body,
                ink,
                panel,
            );
        }
        if self.power_notice {
            let y = canvas.height() as i32 - 120;
            canvas.fill_rect(Rect::new(320, y, 800, 28), panel);
            canvas.draw_text(330, y + 6, self.status, TextStyle::Body, ink, panel);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filtered_launcher_keyboard_pointer_and_escape_share_catalog_ids() {
        let mut s = Shell::new(Preferences::default(), false);
        s.toggle_menu();
        for code in [0x21, 0x17, 0x26, 0x12, 0x1f] {
            assert_eq!(s.menu_key(code), None);
        }
        assert_eq!(s.launcher.query(), "files");
        assert_eq!(s.launcher.result_count(), 2);
        s.menu_key(0xe050);
        assert_eq!(s.menu_key(0x1c), Some(2));
        s.menu_key(0xe048);
        assert_eq!(s.menu_key(0x1c), Some(1));
        assert_eq!(s.menu_hit(30, 100 + s.menu_offset(), 800), Some(0));
        assert_eq!(s.menu_hit(30, 134 + s.menu_offset(), 800), Some(1));
        assert_eq!(s.menu_hit(30, 168 + s.menu_offset(), 800), None);
        assert_eq!(s.menu_hit(30, 60 + s.menu_offset(), 800), None);
        s.dismiss_menu();
        assert!(s.menu);
        assert!(s.launcher.query().is_empty());
        s.dismiss_menu();
        assert!(!s.menu);
        s.toggle_menu();
        for _ in 0..8 {
            s.menu_key(0x2c);
        }
        assert_eq!(s.launcher.result_count(), 0);
        s.menu_key(0xe048);
        assert_eq!(s.menu_key(0x1c), None);
        s.menu_key(0x0e);
        assert_eq!(s.launcher.query().len(), 7);
    }
    #[test]
    fn workspace_tab_dock_and_task_manager_share_real_window_locations() {
        let mut d = Display::new(1280, 800).unwrap();
        let mut s = Shell::new(Preferences::default(), true);
        s.launch(&mut d, 0).unwrap();
        let computer = s.windows[0].unwrap();
        d.switch_workspace(1).unwrap();
        s.launch(&mut d, 1).unwrap();
        let files = s.windows[1].unwrap();
        s.switch(&mut d).unwrap();
        assert_eq!(d.focus(), Some(files));
        assert!(s.task_text(&d).contains("W2"));
        s.dock_activate(&mut d, 0).unwrap();
        assert_eq!(d.current_workspace(), 0);
        assert_eq!(d.focus(), Some(computer));
        s.launch(&mut d, TASK_MANAGER).unwrap();
        s.task_selected = 1;
        s.task_key(&mut d, 0xe053).unwrap();
        assert!(s.task_confirm.is_some());
        s.close(&mut d, s.windows[TASK_MANAGER].unwrap()).unwrap();
        assert_eq!(s.task_confirm, None);
        s.power_bits = 3;
        s.request_power(2, s.task_confirm.is_some());
        assert_eq!(s.power_confirm, Some(2));
    }
    #[test]
    fn dock_and_task_manager_control_real_windows_and_stale_confirmation_is_safe() {
        let mut d = Display::new(1280, 800).unwrap();
        let mut s = Shell::new(Preferences::default(), true);
        for app in 0..APPS.len() {
            s.launch(&mut d, app).unwrap();
        }
        let files = s.windows[1].unwrap();
        s.dock_activate(&mut d, 1).unwrap();
        assert_eq!(d.focus(), Some(files));
        s.dock_activate(&mut d, 1).unwrap();
        assert!(d.window_info(files).unwrap().minimized);
        s.dock_activate(&mut d, 1).unwrap();
        assert!(!d.window_info(files).unwrap().minimized);
        s.task_selected = 1;
        s.task_key(&mut d, 0x32).unwrap();
        assert!(d.window_info(files).unwrap().minimized);
        s.task_key(&mut d, 0x1c).unwrap();
        assert_eq!(d.focus(), Some(files));
        s.task_key(&mut d, 0xe053).unwrap();
        assert_eq!(s.task_confirm, Some(files));
        s.task_key(&mut d, 0x01).unwrap();
        assert!(d.window_info(files).is_some());
        s.task_key(&mut d, 0xe053).unwrap();
        s.close(&mut d, files).unwrap();
        s.launch(&mut d, 1).unwrap();
        let replacement = s.windows[1].unwrap();
        s.task_key(&mut d, 0x1c).unwrap();
        assert!(d.window_info(replacement).is_some());
        s.task_key(&mut d, 0xe053).unwrap();
        s.task_key(&mut d, 0x1c).unwrap();
        assert_eq!(s.windows[1], None);
        s.task_key(&mut d, 0x19).unwrap();
        assert!(s.process_view);
        assert!(s.task_text(&d).contains("System metrics unavailable"));
    }
    #[test]
    fn motion_retargets_finishes_and_reduced_motion_snaps_without_idle_redraws() {
        let mut s = Shell::new(Preferences::default(), false);
        s.menu = true;
        s.animate(1_000_000);
        assert_eq!(s.menu_offset(), -12);
        assert!(s.animate(71_000_000));
        let middle = s.menu_motion.value;
        assert!((128..256).contains(&middle));
        s.menu = false;
        s.animate(71_000_000);
        assert_eq!(s.menu_motion.value, middle);
        assert!(s.animate(211_000_000));
        assert_eq!(s.menu_motion.value, 0);
        assert!(!s.animate(500_000_000));
        s.reduced_motion = true;
        s.menu = true;
        s.selection = 4;
        assert!(s.animate(600_000_000));
        assert_eq!(s.menu_offset(), 0);
        assert_eq!(s.selection_motion.value, 4 * 34 * 256);
        assert!(!s.animate(700_000_000));
        assert!(s.pointer_motion(30, 96 + 2 * 34 + 10, false, 1280, 800));
        assert_eq!(s.selection, 2);
        assert!(!s.pointer_motion(31, 96 + 2 * 34 + 11, false, 1280, 800));
        s.menu = false;
        assert!(s.pointer_motion(40, 90, false, 1280, 800));
        assert_eq!(s.hover_icon, Some(0));
        assert!(s.pointer_motion(40, 90, true, 1280, 800));
        assert_eq!(s.hover_icon, None);
    }
    #[test]
    fn power_requires_capability_and_preserves_unsaved_work() {
        let mut shell = Shell::new(Preferences::default(), true);
        shell.request_power(1, false);
        assert_eq!(shell.power_confirm, None);
        shell.power_bits = 3;
        shell.request_power(3, false);
        assert_eq!(shell.power_confirm, None);
        shell.request_power(1, true);
        assert_eq!(shell.power_confirm, None);
        shell.unsaved_preferences = true;
        shell.request_power(2, false);
        assert_eq!(shell.power_confirm, None);
        shell.unsaved_preferences = false;
        shell.request_power(2, false);
        assert_eq!(shell.power_confirm, Some(2));
    }
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
