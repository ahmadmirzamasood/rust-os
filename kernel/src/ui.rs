//! Interactive screen: the welcome card, the input-debug panel with a test
//! button, and the mouse pointer.
//!
//! Rendering uses a back buffer in the kernel heap with the framebuffer's
//! exact layout. The scene (welcome card, panel) is drawn there; only
//! changed rectangles are copied to the framebuffer, and the pointer is then
//! drawn on the framebuffer itself. Moving the pointer therefore costs two
//! small copies (restore the old spot from the back buffer, draw the arrow
//! at the new one) and never redraws the scene.
//!
//! Runs only in the main loop, never in interrupt context.

use alloc::vec::Vec;
use core::fmt::Write;

use gfx::screens::{InputPanel, PanelLayout, input_panel_layout};
use gfx::widgets::PushButton;
use gfx::{Canvas, FmtBuf, Layout, Rect, cursor};
use input::event::{InputEvent, MouseButton};
use input::keyboard::{Key, KeyEvent};
use input::mouse::Buttons;

use crate::display;

/// Text rows in the input panel.
pub const PANEL_LINES: usize = 5;
const RECENT_KEYS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiError {
    /// The heap could not provide a back buffer of this many bytes.
    NoMemory(usize),
    /// The framebuffer is not installed or is busy.
    NoDisplay,
}

/// Values shown in the panel that the UI does not own.
#[derive(Debug, Clone, Copy)]
pub struct Status {
    pub uptime_ms: u64,
    pub ticks: u64,
    pub tick_millihertz: u64,
    /// The uptime comes from the calibrated TSC (else from ticks).
    pub tsc_clock: bool,
    pub irq: crate::arch::x86_64::interrupts::Stats,
    pub input: crate::input_events::Stats,
}

#[derive(Debug)]
pub struct Ui {
    back: Vec<u8>,
    layout: Layout,
    panel: PanelLayout,
    button: PushButton,
    clicks: u32,
    pointer: (i32, i32),
    buttons: Buttons,
    drawn_pointer: Option<(i32, i32)>,
    panel_dirty: bool,
    screen_dirty: bool,
    last_key: Option<KeyEvent>,
    recent: [Option<(Key, bool)>; RECENT_KEYS],
    recent_next: usize,
}

impl Ui {
    /// Allocate the back buffer and draw the static scene with `draw_scene`.
    pub fn new(
        layout: Layout,
        pointer: (i32, i32),
        draw_scene: impl FnOnce(&mut Canvas<'_>),
    ) -> Result<Self, UiError> {
        let len = layout
            .stride()
            .checked_mul(layout.height())
            .and_then(|px| px.checked_mul(layout.bytes_per_pixel()))
            .ok_or(UiError::NoMemory(usize::MAX))?;
        let mut back = Vec::new();
        back.try_reserve_exact(len)
            .map_err(|_| UiError::NoMemory(len))?;
        back.resize(len, 0);
        {
            let mut canvas = Canvas::new(&mut back, layout).map_err(|_| UiError::NoDisplay)?;
            draw_scene(&mut canvas);
        }
        let panel = input_panel_layout(layout.width(), layout.height(), PANEL_LINES);
        let mut button = PushButton::new(panel.button);
        button.pointer_moved(pointer.0, pointer.1);
        Ok(Self {
            back,
            layout,
            panel,
            button,
            clicks: 0,
            pointer,
            buttons: Buttons::default(),
            drawn_pointer: None,
            panel_dirty: true,
            screen_dirty: true,
            last_key: None,
            recent: [None; RECENT_KEYS],
            recent_next: 0,
        })
    }

    /// Where the test button is (for the serial log and the smoke test).
    pub fn button_rect(&self) -> Rect {
        self.button.rect()
    }

    /// Apply one event. Returns the new click count if it completed a click
    /// on the test button.
    pub fn handle(&mut self, event: &InputEvent) -> Option<u32> {
        match *event {
            InputEvent::Key(key) => {
                self.last_key = Some(key);
                self.recent[self.recent_next] = Some((key.key, key.pressed));
                self.recent_next = (self.recent_next + 1) % RECENT_KEYS;
                self.panel_dirty = true;
                None
            }
            InputEvent::PointerMoved { x, y } => {
                self.pointer = (x, y);
                if self.button.pointer_moved(x, y) {
                    self.panel_dirty = true;
                }
                None
            }
            InputEvent::PointerButton {
                button,
                pressed,
                x,
                y,
            } => {
                self.pointer = (x, y);
                match button {
                    MouseButton::Left => self.buttons.left = pressed,
                    MouseButton::Right => self.buttons.right = pressed,
                    MouseButton::Middle => self.buttons.middle = pressed,
                }
                self.panel_dirty = true;
                if button == MouseButton::Left {
                    let (clicked, _) = self.button.left_button(pressed, x, y);
                    if clicked {
                        self.clicks = self.clicks.wrapping_add(1);
                        return Some(self.clicks);
                    }
                }
                None
            }
        }
    }

    /// Ask for the panel to be redrawn (for example because uptime changed).
    pub fn invalidate_panel(&mut self) {
        self.panel_dirty = true;
    }

    /// Restore the complete diagnostic scene after a user display service exits.
    pub fn invalidate_screen(&mut self) {
        self.screen_dirty = true;
        self.panel_dirty = true;
    }

    /// Copy whatever changed to the framebuffer. Returns `false` if the
    /// framebuffer was busy (only possible while a panic is being reported).
    pub fn render(&mut self, status: &Status) -> bool {
        let mut dirty: [Option<Rect>; 3] = [None; 3];
        if self.screen_dirty {
            let w = u32::try_from(self.layout.width()).unwrap_or(u32::MAX);
            let h = u32::try_from(self.layout.height()).unwrap_or(u32::MAX);
            dirty[0] = Some(Rect::new(0, 0, w, h));
        }
        if self.panel_dirty {
            self.draw_panel(status);
            dirty[0] = dirty[0].or(Some(self.panel.panel));
        }
        if self.drawn_pointer != Some(self.pointer) {
            if let Some((x, y)) = self.drawn_pointer {
                dirty[1] = Some(cursor::bounds(x, y));
            }
            dirty[2] = Some(cursor::bounds(self.pointer.0, self.pointer.1));
        }
        if dirty.iter().all(Option::is_none) {
            return true;
        }
        let back = &self.back;
        let pointer = self.pointer;
        let drawn = display::with_canvas(|front| {
            // The framebuffer lock disables preemption. Recheck ownership here:
            // a service may have claimed it since the event-loop's earlier check.
            if crate::display_device::active() {
                return false;
            }
            for rect in dirty.iter().flatten() {
                front.blit_from(back, *rect);
            }
            cursor::draw(front, pointer.0, pointer.1);
            true
        })
        .unwrap_or(false);
        if drawn {
            self.screen_dirty = false;
            self.panel_dirty = false;
            self.drawn_pointer = Some(pointer);
        }
        drawn
    }

    fn draw_panel(&mut self, s: &Status) {
        let mut lines: [FmtBuf<112>; PANEL_LINES] = core::array::from_fn(|_| FmtBuf::new());
        let _ = write!(
            lines[0],
            "Uptime {}.{:03} s ({})   timer IRQs {} (avg {}/s, set {} Hz)   kbd {}, mouse {}",
            s.uptime_ms / 1000,
            s.uptime_ms % 1000,
            if s.tsc_clock { "TSC" } else { "ticks" },
            s.ticks,
            s.ticks * 1000 / s.uptime_ms.max(1),
            s.tick_millihertz / 1000,
            s.irq.keyboard,
            s.irq.mouse
        );
        let flag = |down: bool| if down { "down" } else { "up" };
        let _ = write!(
            lines[1],
            "Pointer {},{}   left {}, middle {}, right {}   button clicks: {}",
            self.pointer.0,
            self.pointer.1,
            flag(self.buttons.left),
            flag(self.buttons.middle),
            flag(self.buttons.right),
            self.clicks
        );
        match self.last_key {
            Some(k) => {
                let _ = write!(
                    lines[2],
                    "Last key: {} {} (scancode {:#06x})",
                    if k.pressed { "press" } else { "release" },
                    k.key.name(),
                    k.scancode
                );
                if let Some(c) = k.ch.filter(|c| c.is_ascii_graphic()) {
                    let _ = write!(lines[2], " '{c}'");
                }
                let m = k.modifiers;
                for (on, name) in [
                    (m.shift, " shift"),
                    (m.ctrl, " ctrl"),
                    (m.alt, " alt"),
                    (m.meta, " meta"),
                    (m.caps_lock, " caps"),
                ] {
                    if on {
                        let _ = lines[2].write_str(name);
                    }
                }
            }
            None => {
                let _ = lines[2].write_str("Last key: none yet - type something");
            }
        }
        let _ = lines[3].write_str("Recent:");
        for i in 0..RECENT_KEYS {
            if let Some((key, pressed)) = self.recent[(self.recent_next + i) % RECENT_KEYS] {
                let _ = write!(
                    lines[3],
                    " {}{}",
                    if pressed { '+' } else { '-' },
                    key.name()
                );
            }
        }
        let q = s.input;
        let _ = write!(
            lines[4],
            "Queues: kbd {}/{} lost {}, mouse {}/{} lost {}, events lost {}; spurious {}, stray {}",
            q.keyboard_pending,
            q.keyboard_capacity,
            q.keyboard_dropped,
            q.mouse_pending,
            q.mouse_capacity,
            q.mouse_dropped,
            q.events_dropped,
            s.irq.spurious,
            s.irq.unexpected
        );
        let texts: [&str; PANEL_LINES] = core::array::from_fn(|i| lines[i].as_str());
        let panel = InputPanel {
            layout: self.panel,
            title: "Input debug",
            lines: &texts,
            button_label: "Click me",
            button_look: self.button.look(),
            hint: "Move the pointer, click, type. F9: overflow test.",
        };
        if let Ok(mut canvas) = Canvas::new(&mut self.back, self.layout) {
            gfx::screens::draw_input_panel(&mut canvas, &panel);
        }
    }
}
