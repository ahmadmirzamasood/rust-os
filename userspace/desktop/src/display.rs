//! Bounded software display core. No hardware, shared mappings or raw pointers.
//! The transport must supply authenticated client identities, never client bytes.

extern crate alloc;

use alloc::vec::Vec;
use gfx::{Canvas, Rect, Rgb, TextStyle};

pub const MAX_WINDOWS: usize = 8;
/// Fixed, allocation-free workspace policy. The window limit is shared by all
/// workspaces; switching never reallocates or copies application surfaces.
pub const WORKSPACE_COUNT: usize = 4;
pub const MAX_DIMENSION: u32 = 1024;
pub const MAX_PIXELS: usize = 512 * 1024;
const TITLE_HEIGHT: u32 = 24;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Invalid,
    Ownership,
    NotFound,
    Exhausted,
}

/// Local protocol v1: exact-length little-endian packets, no native structs.
/// Header: version u8, opcode u8, reserved u16=0, window u32.
/// Payloads: Create = width u32, height u32, printable ASCII title (1..32);
/// Fill = x/y/w/h u32 + RGB u8; Present/Close = empty.
/// Pixels are copied into service-owned staging memory; Present publishes it.
#[derive(Debug, PartialEq, Eq)]
pub enum Request<'a> {
    Create {
        width: u32,
        height: u32,
        title: &'a str,
    },
    Fill {
        id: u32,
        rect: Rect,
        color: Rgb,
    },
    Present(u32),
    Close(u32),
    Text {
        id: u32,
        x: u32,
        y: u32,
        color: Rgb,
        text: &'a str,
    },
}

pub fn decode(bytes: &[u8]) -> Result<Request<'_>, Error> {
    if bytes.len() < 8 || bytes[0] != 1 || bytes[2..4] != [0, 0] {
        return Err(Error::Invalid);
    }
    let word = |offset| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
    let id = word(4);
    match bytes[1] {
        1 if id == 0 && (17..=48).contains(&bytes.len()) => {
            let width = word(8);
            let height = word(12);
            pixel_count(width, height)?;
            let title = &bytes[16..];
            if !title.iter().all(|b| (32..127).contains(b)) {
                return Err(Error::Invalid);
            }
            Ok(Request::Create {
                width,
                height,
                title: core::str::from_utf8(title).map_err(|_| Error::Invalid)?,
            })
        }
        2 if id != 0 && bytes.len() == 27 => {
            let x = word(8);
            let y = word(12);
            if x > i32::MAX as u32 || y > i32::MAX as u32 {
                return Err(Error::Invalid);
            }
            Ok(Request::Fill {
                id,
                rect: Rect::new(x as i32, y as i32, word(16), word(20)),
                color: Rgb::new(bytes[24], bytes[25], bytes[26]),
            })
        }
        3 if id != 0 && bytes.len() == 8 => Ok(Request::Present(id)),
        4 if id != 0 && bytes.len() == 8 => Ok(Request::Close(id)),
        5 if id != 0 && (20..=147).contains(&bytes.len()) => {
            if !bytes[19..].iter().all(|b| (32..127).contains(b)) {
                return Err(Error::Invalid);
            }
            Ok(Request::Text {
                id,
                x: word(8),
                y: word(12),
                color: Rgb::new(bytes[16], bytes[17], bytes[18]),
                text: core::str::from_utf8(&bytes[19..]).map_err(|_| Error::Invalid)?,
            })
        }
        _ => Err(Error::Invalid),
    }
}

fn pixel_count(width: u32, height: u32) -> Result<usize, Error> {
    if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err(Error::Invalid);
    }
    let count = (width as usize)
        .checked_mul(height as usize)
        .ok_or(Error::Invalid)?;
    if count > MAX_PIXELS {
        return Err(Error::Invalid);
    }
    Ok(count)
}

fn buffer(count: usize) -> Result<Vec<Rgb>, Error> {
    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(count)
        .map_err(|_| Error::Exhausted)?;
    pixels.resize(count, Rgb::new(0, 0, 0));
    Ok(pixels)
}

#[derive(Debug)]
struct Window {
    id: u32,
    owner: u32,
    workspace: usize,
    rect: Rect,
    restore: Option<Rect>,
    minimized: bool,
    title: [u8; 32],
    title_len: usize,
    width: u32,
    height: u32,
    front: Vec<Rgb>,
    back: Vec<Rgb>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Move(i32, i32),
    Resize(u32, u32),
    Minimize,
    Restore,
    Maximize,
    Close,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowInfo {
    pub rect: Rect,
    pub minimized: bool,
    pub focused: bool,
    pub workspace: usize,
}

/// Delivery includes an authenticated destination and window-local coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Delivery {
    pub client: u32,
    pub window: u32,
    pub event: Event,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Closed,
    Motion { x: i32, y: i32 },
    Key { code: u32, pressed: bool },
    Pointer { x: i32, y: i32, pressed: bool },
}

/// Fixed window count; two owned buffers per window. IDs never wrap/reuse.
/// Full scene recomposition on damage restores exposed pixels. An idle scene
/// does no work. The caller supplies a back canvas and presents after rendering.
#[derive(Debug)]
pub struct Display {
    windows: Vec<Window>,
    next_id: u32,
    focus: Option<u32>,
    workspace: usize,
    workspace_focus: [Option<u32>; WORKSPACE_COUNT],
    capture: Option<u32>,
    dirty: bool,
    width: u32,
    height: u32,
    task_strip: bool,
}

impl Display {
    pub fn new(width: u32, height: u32) -> Result<Self, Error> {
        if width < 64 || height < 64 || width > 16384 || height > 16384 {
            return Err(Error::Invalid);
        }
        let mut windows = Vec::new();
        windows
            .try_reserve_exact(MAX_WINDOWS)
            .map_err(|_| Error::Exhausted)?;
        Ok(Self {
            windows,
            next_id: 1,
            focus: None,
            workspace: 0,
            workspace_focus: [None; WORKSPACE_COUNT],
            capture: None,
            dirty: true,
            width,
            height,
            task_strip: true,
        })
    }

    pub fn cancel_capture(&mut self) {
        self.capture = None;
    }

    pub fn focus(&self) -> Option<u32> {
        self.focus
    }

    pub fn current_workspace(&self) -> usize {
        self.workspace
    }

    pub fn workspace_of(&self, id: u32) -> Option<usize> {
        self.windows
            .iter()
            .find(|w| w.id == id)
            .map(|w| w.workspace)
    }

    /// Counts all live windows, including minimized windows. Invalid workspace
    /// indices have no windows; mutating workspace operations reject them.
    pub fn workspace_window_count(&self, workspace: usize) -> usize {
        self.windows
            .iter()
            .filter(|w| w.workspace == workspace)
            .count()
    }

    /// Restore the workspace's last live, visible focus, falling back to its
    /// topmost window. Pointer capture cannot cross a workspace boundary.
    pub fn switch_workspace(&mut self, workspace: usize) -> Result<(), Error> {
        if workspace >= WORKSPACE_COUNT {
            return Err(Error::Invalid);
        }
        if self.workspace == workspace {
            return Ok(());
        }
        self.workspace_focus[self.workspace] = self.focus;
        self.workspace = workspace;
        self.capture = None;
        let remembered = self.workspace_focus[workspace];
        self.focus = self
            .windows
            .iter()
            .find(|w| Some(w.id) == remembered && w.workspace == workspace && !w.minimized)
            .map(|w| w.id);
        if self.focus.is_none() {
            self.refocus();
        }
        self.dirty = true;
        Ok(())
    }

    /// Trusted window-manager operation. Geometry, surfaces and minimized state
    /// remain intact; moving a focused window away selects a local replacement.
    pub fn move_to_workspace(&mut self, id: u32, workspace: usize) -> Result<(), Error> {
        if workspace >= WORKSPACE_COUNT {
            return Err(Error::Invalid);
        }
        let w = self
            .windows
            .iter_mut()
            .find(|w| w.id == id)
            .ok_or(Error::NotFound)?;
        if w.workspace == workspace {
            return Ok(());
        }
        w.workspace = workspace;
        for focus in &mut self.workspace_focus {
            if *focus == Some(id) {
                *focus = None;
            }
        }
        if self.capture == Some(id) {
            self.capture = None;
        }
        if self.focus == Some(id) || self.focus.is_none() {
            self.refocus();
        }
        self.dirty = true;
        Ok(())
    }

    pub fn geometry(&self, id: u32) -> Option<Rect> {
        self.windows.iter().find(|w| w.id == id).map(|w| w.rect)
    }

    pub fn window_info(&self, id: u32) -> Option<WindowInfo> {
        self.windows
            .iter()
            .find(|w| w.id == id)
            .map(|w| WindowInfo {
                rect: w.rect,
                minimized: w.minimized,
                focused: self.focus == Some(id),
                workspace: w.workspace,
            })
    }
    pub fn set_task_strip(&mut self, visible: bool) {
        self.task_strip = visible;
        self.invalidate();
    }
    /// Fit a frame into a caller-selected work area, retaining its pre-layout
    /// geometry. The fixed backing surface stays bounded; extra space uses its
    /// background colour. Restore returns to the original floating rectangle.
    pub fn place(&mut self, id: u32, area: Rect) -> Result<(), Error> {
        if area.x < 0
            || area.y < 0
            || area.w < 80
            || area.h < TITLE_HEIGHT + 32
            || area.x as u64 + u64::from(area.w) > u64::from(self.width)
            || area.y as u64 + u64::from(area.h) > u64::from(self.height)
        {
            return Err(Error::Invalid);
        }
        let w = self
            .windows
            .iter_mut()
            .find(|w| w.id == id)
            .ok_or(Error::NotFound)?;
        if w.restore.is_none() {
            w.restore = Some(w.rect);
        }
        w.rect = area;
        w.minimized = false;
        self.raise(id)
    }
    pub fn toggle_maximize(&mut self, id: u32, area: Rect) -> Result<(), Error> {
        let w = self
            .windows
            .iter()
            .find(|w| w.id == id)
            .ok_or(Error::NotFound)?;
        if w.restore.is_some() {
            self.action(id, Action::Restore)
        } else {
            self.place(id, area)
        }
    }

    pub fn task_at(&self, x: i32, y: i32) -> Option<u32> {
        if !self.task_strip
            || x < 0
            || x >= self.width as i32
            || y < self.height as i32 - 24
            || y >= self.height as i32
        {
            return None;
        }
        self.windows
            .iter()
            .filter(|w| w.workspace == self.workspace)
            .nth(x as usize / 128)
            .map(|w| w.id)
    }

    pub fn raise(&mut self, id: u32) -> Result<(), Error> {
        let index = self
            .windows
            .iter()
            .position(|w| w.id == id)
            .ok_or(Error::NotFound)?;
        self.switch_workspace(self.windows[index].workspace)?;
        let mut window = self.windows.remove(index);
        window.minimized = false;
        self.windows.push(window);
        self.focus = Some(id);
        self.workspace_focus[self.workspace] = Some(id);
        self.dirty = true;
        Ok(())
    }

    pub fn request(&mut self, client: u32, bytes: &[u8]) -> Result<u32, Error> {
        if client == 0 {
            return Err(Error::Ownership);
        }
        match decode(bytes)? {
            Request::Create {
                width,
                height,
                title,
            } => self.create(client, width, height, title),
            Request::Fill { id, rect, color } => {
                let w = self.owned(client, id)?;
                // Require a fully contained rectangle; arithmetic in u64.
                if rect.x < 0
                    || rect.y < 0
                    || rect.w == 0
                    || rect.h == 0
                    || rect.x as u64 + u64::from(rect.w) > u64::from(w.width)
                    || rect.y as u64 + u64::from(rect.h) > u64::from(w.height)
                {
                    return Err(Error::Invalid);
                }
                for y in rect.y as usize..rect.y as usize + rect.h as usize {
                    let start = y * w.width as usize + rect.x as usize;
                    w.back[start..start + rect.w as usize].fill(color);
                }
                Ok(id)
            }
            Request::Present(id) => {
                let w = self.owned(client, id)?;
                w.front.copy_from_slice(&w.back);
                self.dirty = true;
                Ok(id)
            }
            Request::Text {
                id,
                x,
                y,
                color,
                text,
            } => {
                let w = self.owned(client, id)?;
                if x >= w.width || y >= w.height {
                    return Err(Error::Invalid);
                }
                let style = TextStyle::Body;
                for (index, ch) in text.chars().enumerate() {
                    let pen = x as usize + index * style.char_width();
                    if pen >= w.width as usize {
                        break;
                    }
                    if let Some(glyph) = style.glyph(ch) {
                        for (dy, row) in glyph.raster().iter().enumerate() {
                            if y as usize + dy >= w.height as usize {
                                break;
                            }
                            for (dx, alpha) in row.iter().enumerate() {
                                if pen + dx >= w.width as usize {
                                    break;
                                }
                                let p =
                                    &mut w.back[(y as usize + dy) * w.width as usize + pen + dx];
                                *p = p.blend(color, *alpha);
                            }
                        }
                    }
                }
                Ok(id)
            }
            Request::Close(id) => {
                self.owned(client, id)?;
                self.remove(id);
                Ok(id)
            }
        }
    }

    /// Draw a trusted built-in view into its owned staging surface, then publish.
    /// This is an in-process rendering adapter; external clients still use the
    /// validated copy protocol. The callback cannot access other window buffers.
    pub(crate) fn paint_surface(
        &mut self,
        client: u32,
        id: u32,
        paint: impl FnOnce(&mut [Rgb], u32, u32),
    ) -> Result<(), Error> {
        let w = self.owned(client, id)?;
        paint(&mut w.back, w.width, w.height);
        w.front.copy_from_slice(&w.back);
        self.dirty = true;
        Ok(())
    }

    fn create(&mut self, client: u32, width: u32, height: u32, title: &str) -> Result<u32, Error> {
        if self.windows.len() == MAX_WINDOWS {
            return Err(Error::Exhausted);
        }
        let next = self.next_id.checked_add(1).ok_or(Error::Exhausted)?;
        let count = pixel_count(width, height)?;
        let front = buffer(count)?;
        let back = buffer(count)?;
        let mut stored = [0; 32];
        stored[..title.len()].copy_from_slice(title.as_bytes());
        let id = self.next_id;
        let offset = 16 + self.windows.len() as i32 * 28;
        self.windows.push(Window {
            id,
            owner: client,
            workspace: self.workspace,
            rect: Rect::new(offset, offset, width, height + TITLE_HEIGHT),
            restore: None,
            minimized: false,
            title: stored,
            title_len: title.len(),
            width,
            height,
            front,
            back,
        });
        self.next_id = next;
        self.focus = Some(id);
        self.workspace_focus[self.workspace] = Some(id);
        self.dirty = true;
        Ok(id)
    }

    fn owned(&mut self, client: u32, id: u32) -> Result<&mut Window, Error> {
        let w = self
            .windows
            .iter_mut()
            .find(|w| w.id == id)
            .ok_or(Error::NotFound)?;
        if w.owner != client {
            return Err(Error::Ownership);
        }
        Ok(w)
    }

    fn remove(&mut self, id: u32) {
        self.windows.retain(|w| w.id != id);
        if self.capture == Some(id) {
            self.capture = None;
        }
        self.refocus();
        self.dirty = true;
    }

    fn refocus(&mut self) {
        self.focus = self
            .windows
            .iter()
            .rev()
            .find(|w| !w.minimized && w.workspace == self.workspace)
            .map(|w| w.id);
        self.workspace_focus[self.workspace] = self.focus;
    }

    /// Called by the transport on peer closure, including a faulted process.
    pub fn disconnect(&mut self, client: u32) {
        let capture = self.capture;
        self.windows.retain(|w| w.owner != client);
        if !self.windows.iter().any(|w| Some(w.id) == capture) {
            self.capture = None;
        }
        self.refocus();
        self.dirty = true;
    }

    /// Trusted window-manager operation, not exposed as an unauthenticated RPC.
    /// Resizing changes the viewport, preserving the client's backing surface.
    pub fn action(&mut self, id: u32, action: Action) -> Result<(), Error> {
        let w = self
            .windows
            .iter_mut()
            .find(|w| w.id == id)
            .ok_or(Error::NotFound)?;
        match action {
            Action::Move(x, y) => {
                // Dragging out of a managed layout restores the floating size
                // before applying the requested pointer-relative position.
                if let Some(rect) = w.restore.take() {
                    w.rect = rect;
                }
                w.rect.x = x.clamp(0, self.width as i32 - 32);
                w.rect.y = y.clamp(0, self.height as i32 - TITLE_HEIGHT as i32);
            }
            Action::Resize(width, height) => {
                pixel_count(width, height)?;
                w.rect.w = width;
                w.rect.h = height + TITLE_HEIGHT;
                w.restore = None;
            }
            Action::Minimize => {
                w.minimized = true;
                if self.capture == Some(id) {
                    self.capture = None;
                }
            }
            Action::Restore => {
                w.minimized = false;
                if let Some(rect) = w.restore.take() {
                    w.rect = rect;
                }
            }
            Action::Maximize => {
                if w.restore.is_none() {
                    w.restore = Some(w.rect);
                }
                w.minimized = false;
                w.rect = Rect::new(0, 0, self.width, self.height);
            }
            Action::Close => {
                self.remove(id);
                return Ok(());
            }
        }
        self.refocus();
        self.dirty = true;
        Ok(())
    }

    pub fn key(&self, code: u32, pressed: bool) -> Option<Delivery> {
        let w = self
            .windows
            .iter()
            .find(|w| Some(w.id) == self.focus && !w.minimized && w.workspace == self.workspace)?;
        Some(Delivery {
            client: w.owner,
            window: w.id,
            event: Event::Key { code, pressed },
        })
    }

    pub fn motion(&self, x: i32, y: i32) -> Option<Delivery> {
        let w = if let Some(id) = self.capture {
            self.windows
                .iter()
                .find(|w| w.id == id && !w.minimized && w.workspace == self.workspace)?
        } else {
            self.windows
                .iter()
                .rev()
                .find(|w| !w.minimized && w.workspace == self.workspace && w.rect.contains(x, y))?
        };
        Some(Delivery {
            client: w.owner,
            window: w.id,
            event: Event::Motion {
                x: x.saturating_sub(w.rect.x),
                y: y.saturating_sub(w.rect.y)
                    .saturating_sub(TITLE_HEIGHT as i32),
            },
        })
    }

    /// Press raises and focuses the top visible hit. Capture keeps releases
    /// directed to the pressed window even when the pointer leaves its bounds.
    pub fn pointer(&mut self, x: i32, y: i32, pressed: bool) -> Option<Delivery> {
        let index = if let Some(id) = self.capture {
            self.windows
                .iter()
                .position(|w| w.id == id && !w.minimized && w.workspace == self.workspace)?
        } else {
            self.windows.iter().rposition(|w| {
                !w.minimized && w.workspace == self.workspace && w.rect.contains(x, y)
            })?
        };
        if pressed && self.capture.is_none() {
            let w = self.windows.remove(index);
            self.focus = Some(w.id);
            self.workspace_focus[self.workspace] = Some(w.id);
            self.capture = Some(w.id);
            self.windows.push(w);
            self.dirty = true;
        }
        let w = if let Some(id) = self.capture {
            self.windows.iter().find(|w| w.id == id)?
        } else {
            &self.windows[index]
        };
        let result = Delivery {
            client: w.owner,
            window: w.id,
            event: Event::Pointer {
                x: x.saturating_sub(w.rect.x),
                y: y.saturating_sub(w.rect.y)
                    .saturating_sub(TITLE_HEIGHT as i32),
                pressed,
            },
        };
        if !pressed {
            self.capture = None;
        }
        Some(result)
    }

    /// Compose into the caller's off-screen canvas. `true` means it must be
    /// presented. Invalid target geometry does not consume pending damage.
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub fn render(&mut self, canvas: &mut Canvas<'_>) -> Result<bool, Error> {
        self.render_with(canvas, |c| c.clear(Rgb::new(29, 38, 59)))
    }

    pub fn render_with(
        &mut self,
        canvas: &mut Canvas<'_>,
        background: impl FnOnce(&mut Canvas<'_>),
    ) -> Result<bool, Error> {
        if canvas.width() != self.width as usize || canvas.height() != self.height as usize {
            return Err(Error::Invalid);
        }
        if !self.dirty {
            return Ok(false);
        }
        background(canvas);
        for w in &self.windows {
            if w.minimized || w.workspace != self.workspace {
                continue;
            }
            let title_bg = if Some(w.id) == self.focus {
                Rgb::new(60, 100, 150)
            } else {
                Rgb::new(55, 60, 70)
            };
            // Offset shadows and an active edge separate overlapping windows
            // without extra surfaces, alpha buffers or allocations.
            canvas.fill_rounded_rect(
                Rect::new(w.rect.x + 4, w.rect.y + 6, w.rect.w, w.rect.h),
                6,
                Rgb::new(16, 20, 28),
            );
            canvas.fill_rect(
                w.rect,
                w.front.first().copied().unwrap_or(Rgb::new(25, 28, 35)),
            );
            canvas.fill_rect(
                Rect::new(w.rect.x, w.rect.y, w.rect.w, TITLE_HEIGHT),
                title_bg,
            );
            // Only draw whole glyphs fitting the title bar; Canvas also clips
            // to the display. ASCII validation makes byte boundaries glyph boundaries.
            let count = w
                .title_len
                .min(w.rect.w.saturating_sub(80) as usize / TextStyle::Body.char_width());
            let title = core::str::from_utf8(&w.title[..count]).map_err(|_| Error::Invalid)?;
            canvas.draw_text(
                w.rect.x + 4,
                w.rect.y + 4,
                title,
                TextStyle::Body,
                Rgb::new(245, 245, 245),
                title_bg,
            );
            if w.rect.w >= 80 {
                let edge = w.rect.x + w.rect.w as i32;
                let ink = Rgb::new(245, 245, 245);
                canvas.fill_rounded_rect(
                    Rect::new(edge - 23, w.rect.y + 3, 20, 18),
                    4,
                    Rgb::new(151, 66, 78),
                );
                canvas.fill_rect(Rect::new(edge - 65, w.rect.y + 15, 10, 2), ink);
                canvas.fill_rect(Rect::new(edge - 41, w.rect.y + 7, 10, 10), ink);
                canvas.fill_rect(Rect::new(edge - 39, w.rect.y + 9, 6, 6), title_bg);
                for n in 0..10 {
                    canvas.set_pixel(edge - 17 + n, w.rect.y + 7 + n, ink);
                    canvas.set_pixel(edge - 8 - n, w.rect.y + 7 + n, ink);
                }
            }
            canvas.fill_rect(
                Rect::new(w.rect.x, w.rect.y + TITLE_HEIGHT as i32 - 1, w.rect.w, 1),
                if Some(w.id) == self.focus {
                    Rgb::new(108, 181, 220)
                } else {
                    Rgb::new(78, 87, 103)
                },
            );
            let height = w.height.min(w.rect.h.saturating_sub(TITLE_HEIGHT));
            for y in 0..height.min(
                self.height
                    .saturating_sub((w.rect.y as u32).saturating_add(TITLE_HEIGHT)),
            ) {
                let count = w.width.min(w.rect.w) as usize;
                let offset = (y * w.width) as usize;
                canvas.blit_pixels(
                    w.rect.x,
                    w.rect.y + TITLE_HEIGHT as i32 + y as i32,
                    &w.front[offset..offset + count],
                );
            }
            for inset in [5, 9, 13] {
                for delta in 0..inset - 2 {
                    canvas.set_pixel(
                        w.rect.x + w.rect.w as i32 - inset + delta,
                        w.rect.y + w.rect.h as i32 - 3 - delta,
                        Rgb::new(115, 133, 155),
                    );
                }
            }
        }
        // A small task strip makes minimized windows discoverable/restorable.
        if self.task_strip && self.workspace_window_count(self.workspace) > 0 {
            canvas.fill_rect(
                Rect::new(0, self.height as i32 - 24, self.width, 24),
                Rgb::new(24, 29, 38),
            );
        }
        for (index, w) in self
            .windows
            .iter()
            .filter(|w| self.task_strip && w.workspace == self.workspace)
            .enumerate()
        {
            let x = index as i32 * 128;
            let y = self.height as i32 - 24;
            let active = Some(w.id) == self.focus;
            let bg = if active {
                Rgb::new(52, 75, 103)
            } else {
                Rgb::new(45, 55, 70)
            };
            canvas.fill_rounded_rect(Rect::new(x + 2, y + 1, 122, 23), 4, bg);
            canvas.fill_rect(
                Rect::new(x + 12, y + 22, 100, 2),
                if active {
                    Rgb::new(108, 181, 220)
                } else if w.minimized {
                    Rgb::new(92, 99, 115)
                } else {
                    Rgb::new(70, 112, 145)
                },
            );
            let count = w.title_len.min(12);
            canvas.draw_text(
                x + 4,
                y + 4,
                core::str::from_utf8(&w.title[..count]).map_err(|_| Error::Invalid)?,
                TextStyle::Body,
                Rgb::new(240, 240, 240),
                bg,
            );
        }
        self.dirty = false;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_painter_checks_ownership_and_publishes_only_its_surface() {
        let mut display = super::Display::new(640, 480).unwrap();
        let first = display.create(7, 4, 3, "First").unwrap();
        let second = display.create(8, 4, 3, "Second").unwrap();
        display.dirty = false;
        let mut entered = false;
        assert_eq!(
            display.paint_surface(8, first, |_, _, _| entered = true),
            Err(super::Error::Ownership)
        );
        assert_eq!(
            display.paint_surface(7, u32::MAX, |_, _, _| entered = true),
            Err(super::Error::NotFound)
        );
        assert!(!entered && !display.dirty);
        display
            .paint_surface(7, first, |pixels, width, height| {
                assert_eq!((pixels.len(), width, height), (12, 4, 3));
                pixels.fill(gfx::Rgb::new(1, 2, 3));
            })
            .unwrap();
        assert!(display.dirty);
        let w = display.windows.iter().find(|w| w.id == first).unwrap();
        assert_eq!(w.front, w.back);
        assert!(w.front.iter().all(|p| *p == gfx::Rgb::new(1, 2, 3)));
        assert!(
            display
                .windows
                .iter()
                .find(|w| w.id == second)
                .unwrap()
                .front
                .iter()
                .all(|p| *p == gfx::Rgb::new(0, 0, 0))
        );
    }
    extern crate std;
    use super::*;
    use gfx::{Layout, PixelFormat};
    use std::vec;
    #[test]
    fn workspace_switch_isolates_pixels_input_and_capture_without_copying_surfaces() {
        let mut d = Display::new(240, 180).unwrap();
        let a = create(&mut d, 7);
        d.request(7, &fill(a, Rect::new(0, 0, 80, 60), Rgb::new(255, 0, 0)))
            .unwrap();
        present(&mut d, 7, a);
        assert_eq!(d.pointer(20, 50, true).unwrap().window, a);
        d.switch_workspace(1).unwrap();
        assert_eq!(d.capture, None);
        assert_eq!(d.focus(), None);
        assert_eq!(d.key(65, true), None);
        assert_eq!(d.motion(20, 50), None);
        assert_eq!(d.pointer(20, 50, false), None);
        let b = create(&mut d, 8);
        d.action(b, Action::Move(16, 16)).unwrap();
        d.request(8, &fill(b, Rect::new(0, 0, 80, 60), Rgb::new(0, 255, 0)))
            .unwrap();
        present(&mut d, 8, b);
        let surfaces = d
            .windows
            .iter()
            .map(|w| (w.id, w.front.as_ptr(), w.back.as_ptr()))
            .collect::<Vec<_>>();
        let layout = Layout::new(240, 180, 240, 3, PixelFormat::Rgb).unwrap();
        let mut pixels = vec![0; 240 * 180 * 3];
        for (workspace, expected, color) in [
            (0, Some(a), [255, 0, 0]),
            (1, Some(b), [0, 255, 0]),
            (3, None, [29, 38, 59]),
        ] {
            d.switch_workspace(workspace).unwrap();
            assert_eq!(d.focus(), expected);
            assert_eq!(d.key(65, true).map(|e| e.window), expected);
            assert_eq!(d.motion(20, 50).map(|e| e.window), expected);
            assert_eq!(d.task_at(10, 170), expected);
            assert_eq!(d.task_at(140, 170), None);
            d.render(&mut Canvas::new(&mut pixels, layout).unwrap())
                .unwrap();
            assert_eq!(
                &pixels[(50 * 240 + 20) * 3..(50 * 240 + 20) * 3 + 3],
                &color
            );
        }
        assert_eq!(
            surfaces,
            d.windows
                .iter()
                .map(|w| (w.id, w.front.as_ptr(), w.back.as_ptr()))
                .collect::<Vec<_>>()
        );
        assert_eq!(d.workspace_window_count(0), 1);
        assert_eq!(d.workspace_window_count(1), 1);
        assert_eq!(d.workspace_window_count(WORKSPACE_COUNT), 0);
    }

    #[test]
    fn workspace_move_preserves_geometry_and_minimization_and_raise_follows_window() {
        let mut d = Display::new(240, 180).unwrap();
        let a = create(&mut d, 7);
        let b = create(&mut d, 8);
        d.place(b, Rect::new(0, 0, 240, 150)).unwrap();
        let managed = d.geometry(b).unwrap();
        d.pointer(50, 50, true).unwrap();
        d.move_to_workspace(b, 2).unwrap();
        assert_eq!(d.capture, None);
        assert_eq!(d.focus(), Some(a));
        assert_eq!(d.workspace_of(b), Some(2));
        assert_eq!(d.geometry(b), Some(managed));
        d.action(b, Action::Minimize).unwrap();
        d.switch_workspace(2).unwrap();
        assert_eq!(d.focus(), None);
        assert!(d.window_info(b).unwrap().minimized);
        assert_eq!(d.task_at(10, 170), Some(b));
        d.switch_workspace(0).unwrap();
        d.raise(b).unwrap();
        assert_eq!(d.current_workspace(), 2);
        assert_eq!(d.focus(), Some(b));
        assert!(!d.window_info(b).unwrap().minimized);
        assert_eq!(d.geometry(b), Some(managed));
        assert_eq!(d.window_info(b).unwrap().workspace, 2);
        d.action(b, Action::Move(40, 50)).unwrap();
        assert_eq!(d.geometry(b), Some(Rect::new(40, 50, 80, 84)));
    }

    #[test]
    fn workspace_focus_remembers_live_windows_and_rejects_invalid_mutations() {
        let mut d = Display::new(240, 180).unwrap();
        let a = create(&mut d, 7);
        d.switch_workspace(1).unwrap();
        let b = create(&mut d, 8);
        let c = create(&mut d, 9);
        d.raise(b).unwrap();
        d.switch_workspace(0).unwrap();
        d.switch_workspace(1).unwrap();
        assert_eq!(d.focus(), Some(b));
        d.switch_workspace(0).unwrap();
        d.action(b, Action::Close).unwrap();
        d.switch_workspace(1).unwrap();
        assert_eq!(d.focus(), Some(c));
        d.switch_workspace(0).unwrap();
        d.disconnect(9);
        d.switch_workspace(1).unwrap();
        assert_eq!(d.focus(), None);
        d.raise(a).unwrap();
        d.pointer(20, 50, true).unwrap();
        let original = d.geometry(a);
        assert_eq!(d.switch_workspace(WORKSPACE_COUNT), Err(Error::Invalid));
        assert_eq!(d.move_to_workspace(a, usize::MAX), Err(Error::Invalid));
        assert_eq!(d.move_to_workspace(b, 1), Err(Error::NotFound));
        assert_eq!(d.raise(b), Err(Error::NotFound));
        assert_eq!(d.workspace_of(b), None);
        assert_eq!(d.geometry(a), original);
        assert_eq!(d.current_workspace(), 0);
        assert_eq!(d.focus(), Some(a));
        assert_eq!(d.capture, Some(a));
    }
    #[test]
    fn work_area_maximize_tile_restore_and_rejected_bounds() {
        let mut d = Display::new(1280, 800).unwrap();
        let id = create(&mut d, 7);
        let original = d.geometry(id).unwrap();
        let area = Rect::new(8, 44, 1264, 660);
        d.toggle_maximize(id, area).unwrap();
        assert_eq!(d.geometry(id), Some(area));
        d.toggle_maximize(id, area).unwrap();
        assert_eq!(d.geometry(id), Some(original));
        d.place(id, Rect::new(8, 44, 628, 660)).unwrap();
        d.place(id, Rect::new(644, 44, 628, 660)).unwrap();
        d.action(id, Action::Restore).unwrap();
        assert_eq!(d.geometry(id), Some(original));
        assert_eq!(d.place(id, Rect::new(-1, 0, 100, 100)), Err(Error::Invalid));
        assert_eq!(
            d.place(id, Rect::new(0, 0, u32::MAX, 100)),
            Err(Error::Invalid)
        );
        assert_eq!(d.geometry(id), Some(original));
    }

    fn create(d: &mut Display, client: u32) -> u32 {
        let mut packet = vec![1, 1, 0, 0, 0, 0, 0, 0];
        packet.extend(80u32.to_le_bytes());
        packet.extend(60u32.to_le_bytes());
        packet.extend(b"Demo");
        d.request(client, &packet).unwrap()
    }

    fn fill(id: u32, rect: Rect, color: Rgb) -> Vec<u8> {
        let mut packet = vec![1, 2, 0, 0];
        packet.extend(id.to_le_bytes());
        for v in [rect.x as u32, rect.y as u32, rect.w, rect.h] {
            packet.extend(v.to_le_bytes());
        }
        packet.extend([color.r, color.g, color.b]);
        packet
    }

    fn present(d: &mut Display, client: u32, id: u32) {
        let mut packet = vec![1, 3, 0, 0];
        packet.extend(id.to_le_bytes());
        d.request(client, &packet).unwrap();
    }

    #[test]
    fn parser_rejects_truncation_versions_reserved_and_unknown_opcodes() {
        let mut packet = vec![1, 1, 0, 0, 0, 0, 0, 0];
        packet.extend(80u32.to_le_bytes());
        packet.extend(60u32.to_le_bytes());
        packet.extend(b"Title");
        for n in 0..17 {
            assert_eq!(decode(&packet[..n]), Err(Error::Invalid));
        }
        for (offset, value) in [(0, 2), (1, 255), (2, 1), (3, 1), (4, 1), (16, 255)] {
            let mut corrupt = packet.clone();
            corrupt[offset] = value;
            assert_eq!(decode(&corrupt), Err(Error::Invalid));
        }
        for (w, h) in [(0u32, 1u32), (1, 0), (u32::MAX, 2), (1024, 1024)] {
            packet[8..12].copy_from_slice(&w.to_le_bytes());
            packet[12..16].copy_from_slice(&h.to_le_bytes());
            assert_eq!(decode(&packet), Err(Error::Invalid));
        }
    }

    #[test]
    fn arbitrary_packets_are_total_and_text_clips_to_surface() {
        let mut seed = 7u32;
        let mut bytes = [0u8; 300];
        for round in 0..10000 {
            for b in &mut bytes {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                *b = (seed >> 24) as u8;
            }
            if round % 2 == 0 {
                bytes[0] = 1;
                bytes[1] = (round % 7) as u8;
                bytes[2] = 0;
                bytes[3] = 0;
            }
            let _ = decode(&bytes[..round % 301]);
        }
        let mut d = Display::new(240, 180).unwrap();
        let id = create(&mut d, 1);
        let mut packet = vec![1, 5, 0, 0];
        packet.extend(id.to_le_bytes());
        packet.extend(79u32.to_le_bytes());
        packet.extend(59u32.to_le_bytes());
        packet.extend([255, 255, 255]);
        packet.extend(b"Long text at the last pixel");
        assert_eq!(d.request(2, &packet), Err(Error::Ownership));
        assert_eq!(d.request(1, &packet), Ok(id));
        packet[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(d.request(1, &packet), Err(Error::Invalid));
    }

    #[test]
    fn malformed_or_foreign_messages_do_not_change_pixels() {
        let mut d = Display::new(240, 180).unwrap();
        let id = create(&mut d, 7);
        let p = fill(id, Rect::new(0, 0, 80, 60), Rgb::new(255, 0, 0));
        assert_eq!(d.request(8, &p), Err(Error::Ownership));
        assert_eq!(d.request(0, &p), Err(Error::Ownership));
        assert_eq!(
            d.request(
                7,
                &fill(id, Rect::new(79, 59, u32::MAX, 2), Rgb::new(1, 2, 3))
            ),
            Err(Error::Invalid)
        );
        assert!(d.windows[0].back.iter().all(|p| *p == Rgb::new(0, 0, 0)));
        d.request(7, &p).unwrap();
        assert_eq!(d.windows[0].front[0], Rgb::new(0, 0, 0));
        present(&mut d, 7, id);
        assert_eq!(d.windows[0].front[0], Rgb::new(255, 0, 0));
    }

    #[test]
    fn focus_stacking_capture_and_disconnect_keep_other_client_alive() {
        let mut d = Display::new(240, 180).unwrap();
        let a = create(&mut d, 7);
        let b = create(&mut d, 8);
        assert_eq!(d.key(65, true).unwrap().client, 8);
        let down = d.pointer(20, 50, true).unwrap();
        assert_eq!((down.client, down.window), (7, a));
        assert_eq!(d.focus(), Some(a));
        let up = d.pointer(239, 179, false).unwrap();
        assert_eq!((up.client, up.window), (7, a));
        d.disconnect(7);
        assert_eq!(d.focus(), Some(b));
        assert_eq!(d.key(66, true).unwrap().client, 8);
        assert_eq!(
            d.request(7, &fill(a, Rect::new(0, 0, 1, 1), Rgb::new(1, 1, 1))),
            Err(Error::NotFound)
        );
        let c = create(&mut d, 7);
        assert_ne!(c, a);
    }

    #[test]
    fn minimize_restore_maximize_close_and_limits() {
        let mut d = Display::new(240, 180).unwrap();
        let a = create(&mut d, 7);
        let b = create(&mut d, 8);
        let original = d.windows[1].rect;
        d.action(b, Action::Maximize).unwrap();
        assert_eq!(d.windows[1].rect, Rect::new(0, 0, 240, 180));
        d.action(b, Action::Restore).unwrap();
        assert_eq!(d.windows[1].rect, original);
        d.action(b, Action::Minimize).unwrap();
        assert_eq!(d.focus(), Some(a));
        d.action(b, Action::Restore).unwrap();
        assert_eq!(d.focus(), Some(b));
        assert_eq!(d.action(b, Action::Resize(0, 10)), Err(Error::Invalid));
        d.action(b, Action::Move(i32::MAX, i32::MIN)).unwrap();
        assert_eq!((d.windows[1].rect.x, d.windows[1].rect.y), (208, 0));
        d.action(b, Action::Close).unwrap();
        assert_eq!(d.focus(), Some(a));
        for _ in 1..MAX_WINDOWS {
            create(&mut d, 7);
        }
        let mut packet = vec![1, 1, 0, 0, 0, 0, 0, 0];
        packet.extend(1u32.to_le_bytes());
        packet.extend(1u32.to_le_bytes());
        packet.push(b'x');
        assert_eq!(d.request(7, &packet), Err(Error::Exhausted));
    }

    #[test]
    fn moved_resized_closed_scene_matches_fresh_composition() {
        let layout = Layout::new(240, 180, 244, 4, PixelFormat::Bgr).unwrap();
        let mut pixels = vec![0xaa; 244 * 180 * 4];
        let mut d = Display::new(240, 180).unwrap();
        let a = create(&mut d, 7);
        let b = create(&mut d, 8);
        d.request(7, &fill(a, Rect::new(0, 0, 80, 60), Rgb::new(255, 0, 0)))
            .unwrap();
        d.request(8, &fill(b, Rect::new(0, 0, 80, 60), Rgb::new(0, 255, 0)))
            .unwrap();
        present(&mut d, 7, a);
        present(&mut d, 8, b);
        d.render(&mut Canvas::new(&mut pixels, layout).unwrap())
            .unwrap();
        assert!(
            !d.render(&mut Canvas::new(&mut pixels, layout).unwrap())
                .unwrap()
        );
        for action in [
            Action::Move(150, 90),
            Action::Resize(30, 20),
            Action::Resize(200, 150),
            Action::Minimize,
            Action::Restore,
            Action::Close,
        ] {
            d.action(b, action).unwrap();
            assert!(
                d.render(&mut Canvas::new(&mut pixels, layout).unwrap())
                    .unwrap()
            );
            let mut fresh = vec![0xaa; pixels.len()];
            d.dirty = true;
            d.render(&mut Canvas::new(&mut fresh, layout).unwrap())
                .unwrap();
            assert_eq!(pixels, fresh);
            for y in 0..180 {
                assert!(
                    pixels[(y * 244 + 240) * 4..(y + 1) * 244 * 4]
                        .iter()
                        .all(|v| *v == 0xaa)
                );
            }
        }
        assert_eq!(
            &pixels[(90 * 244 + 150) * 4..(90 * 244 + 150) * 4 + 3],
            &[59, 38, 29]
        );
    }
}
