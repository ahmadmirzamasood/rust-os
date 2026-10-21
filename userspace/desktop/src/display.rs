//! Bounded software display core. No hardware, shared mappings or raw pointers.
//! The transport must supply authenticated client identities, never client bytes.

extern crate alloc;

use alloc::vec::Vec;
use gfx::{Canvas, Rect, Rgb, TextStyle};

pub const MAX_WINDOWS: usize = 8;
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
    capture: Option<u32>,
    dirty: bool,
    width: u32,
    height: u32,
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
            capture: None,
            dirty: true,
            width,
            height,
        })
    }

    pub fn cancel_capture(&mut self) {
        self.capture = None;
    }

    pub fn focus(&self) -> Option<u32> {
        self.focus
    }

    pub fn geometry(&self, id: u32) -> Option<Rect> {
        self.windows.iter().find(|w| w.id == id).map(|w| w.rect)
    }

    pub fn task_at(&self, x: i32, y: i32) -> Option<u32> {
        if x < 0 || y < self.height as i32 - 24 || y >= self.height as i32 {
            return None;
        }
        self.windows.get(x as usize / 128).map(|w| w.id)
    }

    pub fn raise(&mut self, id: u32) -> Result<(), Error> {
        let index = self
            .windows
            .iter()
            .position(|w| w.id == id)
            .ok_or(Error::NotFound)?;
        let mut window = self.windows.remove(index);
        window.minimized = false;
        self.windows.push(window);
        self.focus = Some(id);
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
            .find(|w| !w.minimized)
            .map(|w| w.id);
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
                w.rect.x = x.clamp(0, self.width as i32 - 32);
                w.rect.y = y.clamp(0, self.height as i32 - TITLE_HEIGHT as i32);
                w.restore = None;
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
        let w = self.windows.iter().find(|w| Some(w.id) == self.focus)?;
        Some(Delivery {
            client: w.owner,
            window: w.id,
            event: Event::Key { code, pressed },
        })
    }

    pub fn motion(&self, x: i32, y: i32) -> Option<Delivery> {
        let w = if let Some(id) = self.capture {
            self.windows.iter().find(|w| w.id == id)?
        } else {
            self.windows
                .iter()
                .rev()
                .find(|w| !w.minimized && w.rect.contains(x, y))?
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
            self.windows.iter().position(|w| w.id == id)?
        } else {
            self.windows
                .iter()
                .rposition(|w| !w.minimized && w.rect.contains(x, y))?
        };
        if pressed && self.capture.is_none() {
            let w = self.windows.remove(index);
            self.focus = Some(w.id);
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
            if w.minimized {
                continue;
            }
            let title_bg = if Some(w.id) == self.focus {
                Rgb::new(60, 100, 150)
            } else {
                Rgb::new(55, 60, 70)
            };
            canvas.fill_rect(w.rect, Rgb::new(25, 28, 35));
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
                canvas.fill_rect(Rect::new(edge - 65, w.rect.y + 15, 10, 2), ink);
                canvas.fill_rect(Rect::new(edge - 41, w.rect.y + 7, 10, 10), ink);
                canvas.fill_rect(Rect::new(edge - 39, w.rect.y + 9, 6, 6), title_bg);
                for n in 0..10 {
                    canvas.set_pixel(edge - 17 + n, w.rect.y + 7 + n, ink);
                    canvas.set_pixel(edge - 8 - n, w.rect.y + 7 + n, ink);
                }
            }
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
        }
        // A small task strip makes minimized windows discoverable/restorable.
        for (index, w) in self.windows.iter().enumerate() {
            let x = index as i32 * 128;
            let y = self.height as i32 - 24;
            let bg = Rgb::new(45, 55, 70);
            canvas.fill_rect(Rect::new(x, y, 124, 24), bg);
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
    extern crate std;
    use super::*;
    use gfx::{Layout, PixelFormat};
    use std::vec;

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
