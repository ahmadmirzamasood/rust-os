//! Clipped drawing primitives over a framebuffer byte slice.

use crate::layout::{Layout, LayoutError};
use crate::text::TextStyle;
use crate::theme::Rgb;

/// A rectangle in screen pixels. It may extend past any screen edge; drawing
/// clips it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }

    /// Whether the point (`px`, `py`) lies inside (right/bottom edges
    /// excluded).
    pub fn contains(&self, px: i32, py: i32) -> bool {
        let (px, py) = (i64::from(px), i64::from(py));
        let (x, y) = (i64::from(self.x), i64::from(self.y));
        px >= x && py >= y && px < x + i64::from(self.w) && py < y + i64::from(self.h)
    }

    /// Whether the two rectangles share at least one pixel.
    pub fn intersects(&self, other: &Rect) -> bool {
        let (ax, ay) = (i64::from(self.x), i64::from(self.y));
        let (bx, by) = (i64::from(other.x), i64::from(other.y));
        self.w > 0
            && self.h > 0
            && other.w > 0
            && other.h > 0
            && ax < bx + i64::from(other.w)
            && bx < ax + i64::from(self.w)
            && ay < by + i64::from(other.h)
            && by < ay + i64::from(self.h)
    }
}

/// A drawing surface over `buf`, described by a validated [`Layout`].
///
/// Only the visible `width` pixels of each row are ever written; stride
/// padding and bytes past the geometry are left untouched.
#[derive(Debug)]
pub struct Canvas<'a> {
    buf: &'a mut [u8],
    layout: Layout,
}

impl<'a> Canvas<'a> {
    /// Wrap `buf`, failing if it is shorter than the layout requires.
    pub fn new(buf: &'a mut [u8], layout: Layout) -> Result<Self, LayoutError> {
        layout.check_buffer(buf.len())?;
        Ok(Self { buf, layout })
    }

    pub const fn layout(&self) -> Layout {
        self.layout
    }

    pub const fn width(&self) -> usize {
        self.layout.width()
    }

    pub const fn height(&self) -> usize {
        self.layout.height()
    }

    /// Fill the whole visible area.
    pub fn clear(&mut self, color: Rgb) {
        let px = self.layout.encode(color);
        self.fill_span_rows(0, 0, self.width() as i64, self.height() as i64, &px);
    }

    /// Fill `rect`, clipped to the screen.
    pub fn fill_rect(&mut self, rect: Rect, color: Rgb) {
        let px = self.layout.encode(color);
        let (x0, y0) = (i64::from(rect.x), i64::from(rect.y));
        self.fill_span_rows(x0, y0, x0 + i64::from(rect.w), y0 + i64::from(rect.h), &px);
    }

    /// Fill `rect` with circular corners of `radius` pixels.
    pub fn fill_rounded_rect(&mut self, rect: Rect, radius: u32, color: Rgb) {
        let px = self.layout.encode(color);
        let radius = radius.min(rect.w / 2).min(rect.h / 2);
        let (x0, y0) = (i64::from(rect.x), i64::from(rect.y));
        let x1 = x0 + i64::from(rect.w);
        for row in 0..rect.h {
            let inset = i64::from(corner_inset(row, rect.h, radius));
            let y = y0 + i64::from(row);
            self.fill_span_rows(x0 + inset, y, x1 - inset, y + 1, &px);
        }
    }

    /// Draw `text` with its top-left corner at (`x`, `y`), anti-aliased
    /// against `bg`. Returns the x coordinate after the last character.
    /// Only glyph pixels are written, so `bg` should match what is already
    /// on screen.
    pub fn draw_text(
        &mut self,
        x: i32,
        y: i32,
        text: &str,
        style: TextStyle,
        fg: Rgb,
        bg: Rgb,
    ) -> i32 {
        let advance = i64::try_from(style.char_width()).unwrap_or(i64::MAX);
        let mut pen = i64::from(x);
        for c in text.chars() {
            if pen >= self.width() as i64 {
                break;
            }
            if let Some(glyph) = style.glyph(c) {
                for (dy, row) in glyph.raster().iter().enumerate() {
                    for (dx, &alpha) in row.iter().enumerate() {
                        if alpha != 0 {
                            let px = self.layout.encode(bg.blend(fg, alpha));
                            self.put(pen + dx as i64, i64::from(y) + dy as i64, &px);
                        }
                    }
                }
            }
            pen = pen.saturating_add(advance);
        }
        i32::try_from(pen).unwrap_or(i32::MAX)
    }

    /// Set one pixel if (`x`, `y`) is on screen.
    pub fn set_pixel(&mut self, x: i32, y: i32, color: Rgb) {
        let px = self.layout.encode(color);
        self.put(i64::from(x), i64::from(y), &px);
    }

    /// Copy `rect` (clipped to the screen) from `src`, a buffer with this
    /// canvas's layout, for example a back buffer. Returns `false` and
    /// copies nothing if `src` is too short for the layout.
    pub fn blit_from(&mut self, src: &[u8], rect: Rect) -> bool {
        if self.layout.check_buffer(src.len()).is_err() {
            return false;
        }
        let (w, h) = (self.width() as i64, self.height() as i64);
        let (x0, y0) = (i64::from(rect.x), i64::from(rect.y));
        let x1 = (x0 + i64::from(rect.w)).clamp(0, w);
        let y1 = (y0 + i64::from(rect.h)).clamp(0, h);
        let (x0, y0) = (x0.clamp(0, w), y0.clamp(0, h));
        if x0 >= x1 || y0 >= y1 {
            return true;
        }
        let bpp = self.layout.bytes_per_pixel();
        let stride = self.layout.stride();
        // Same bounds argument as `fill_span_rows`; both buffers passed
        // `check_buffer` for the same layout.
        for y in y0 as usize..y1 as usize {
            let start = (y * stride + x0 as usize) * bpp;
            let end = (y * stride + x1 as usize) * bpp;
            self.buf[start..end].copy_from_slice(&src[start..end]);
        }
        true
    }

    /// Copy a row of RGB pixels, clipped horizontally and vertically. This
    /// avoids repeating rectangle clipping for every pixel of a surface.
    pub fn blit_pixels(&mut self, x: i32, y: i32, pixels: &[Rgb]) {
        if y < 0 || y as usize >= self.height() {
            return;
        }
        let skip = (-(i64::from(x))).max(0) as usize;
        if skip >= pixels.len() {
            return;
        }
        let start_x = x.max(0) as usize;
        if start_x >= self.width() {
            return;
        }
        let count = (pixels.len() - skip).min(self.width() - start_x);
        let bpp = self.layout.bytes_per_pixel();
        let offset = (y as usize * self.layout.stride() + start_x) * bpp;
        for (target, color) in self.buf[offset..offset + count * bpp]
            .chunks_exact_mut(bpp)
            .zip(&pixels[skip..skip + count])
        {
            target.copy_from_slice(&self.layout.encode(*color)[..bpp]);
        }
    }
    /// Write one encoded pixel if (`x`, `y`) is on screen.
    fn put(&mut self, x: i64, y: i64, px: &[u8; 4]) {
        self.fill_span_rows(x, y, x + 1, y + 1, px);
    }

    /// Fill the half-open box [x0, x1) × [y0, y1), clipped to the screen.
    fn fill_span_rows(&mut self, x0: i64, y0: i64, x1: i64, y1: i64, px: &[u8; 4]) {
        let (w, h) = (self.width() as i64, self.height() as i64);
        let (x0, x1) = (x0.clamp(0, w), x1.clamp(0, w));
        let (y0, y1) = (y0.clamp(0, h), y1.clamp(0, h));
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let bpp = self.layout.bytes_per_pixel();
        let stride = self.layout.stride();
        let px = &px[..bpp];
        // x < width <= stride and y < height, and Layout::new checked that
        // (stride * (height - 1) + width) * bpp fits in usize and the buffer
        // is at least that long, so these offsets cannot overflow or go out
        // of bounds. Slicing would panic rather than corrupt memory anyway.
        for y in y0 as usize..y1 as usize {
            let row = y * stride;
            let start = (row + x0 as usize) * bpp;
            let end = (row + x1 as usize) * bpp;
            for chunk in self.buf[start..end].chunks_exact_mut(bpp) {
                chunk.copy_from_slice(px);
            }
        }
    }
}

/// Horizontal inset of `row` in a box of height `h` with corner `radius`.
fn corner_inset(row: u32, h: u32, radius: u32) -> u32 {
    if radius == 0 {
        return 0;
    }
    // Distance in rows from the corner circle's centre line, counted 1..=radius.
    let d = if row < radius {
        radius - row
    } else if row >= h - radius {
        row - (h - radius) + 1
    } else {
        return 0;
    };
    // Sample at the pixel centre: dy = d - 0.5. In doubled units:
    // half-width = sqrt((2r)^2 - (2d - 1)^2) / 2.
    let r2 = u64::from(radius) * 2;
    let dy2 = u64::from(d) * 2 - 1;
    let half = isqrt(r2 * r2 - dy2 * dy2) / 2;
    radius - half as u32
}

fn isqrt(n: u64) -> u64 {
    if n < 2 {
        return n;
    }
    let mut x = n;
    let mut y = x.div_ceil(2);
    while y < x {
        x = y;
        y = (x + n / x) / 2;
    }
    x
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::PixelFormat;
    use std::vec;

    const PAD: u8 = 0xAA;

    fn canvas_buf(layout: &Layout) -> std::vec::Vec<u8> {
        vec![PAD; layout.stride() * layout.height() * layout.bytes_per_pixel()]
    }

    #[test]
    fn rejects_short_buffer() {
        let layout = Layout::new(8, 8, 8, 4, PixelFormat::Rgb).unwrap();
        let mut buf = vec![0u8; 8 * 8 * 4 - 1];
        assert!(matches!(
            Canvas::new(&mut buf, layout),
            Err(LayoutError::BufferTooSmall { .. })
        ));
    }

    #[test]
    fn pixel_rows_clip_preserve_padding_and_encode_bgr() {
        let layout = Layout::new(3, 2, 5, 4, PixelFormat::Bgr).unwrap();
        let mut bytes = canvas_buf(&layout);
        let colors = [Rgb::new(1, 2, 3), Rgb::new(4, 5, 6), Rgb::new(7, 8, 9)];
        let mut canvas = Canvas::new(&mut bytes, layout).unwrap();
        canvas.blit_pixels(-1, 1, &colors);
        canvas.blit_pixels(i32::MIN, 0, &colors);
        canvas.blit_pixels(i32::MAX, 0, &colors);
        canvas.blit_pixels(0, -1, &colors);
        canvas.blit_pixels(0, 2, &colors);
        assert_eq!(&bytes[20..28], &[6, 5, 4, 0, 9, 8, 7, 0]);
        assert!(bytes[..20].iter().all(|b| *b == PAD));
        assert!(bytes[28..].iter().all(|b| *b == PAD));
    }

    #[test]
    fn clear_respects_stride_padding_and_format() {
        let layout = Layout::new(3, 2, 5, 4, PixelFormat::Bgr).unwrap();
        let mut buf = canvas_buf(&layout);
        Canvas::new(&mut buf, layout)
            .unwrap()
            .clear(Rgb::new(1, 2, 3));
        for y in 0..2 {
            for x in 0..5 {
                let o = (y * 5 + x) * 4;
                let expect = if x < 3 { [3, 2, 1, 0] } else { [PAD; 4] };
                assert_eq!(buf[o..o + 4], expect, "pixel ({x},{y})");
            }
        }
    }

    #[test]
    fn rect_clips_at_every_edge() {
        let layout = Layout::new(4, 4, 4, 1, PixelFormat::Gray).unwrap();
        let mut buf = canvas_buf(&layout);
        let mut canvas = Canvas::new(&mut buf, layout).unwrap();
        canvas.fill_rect(Rect::new(-10, -10, 12, 12), Rgb::new(255, 255, 255));
        canvas.fill_rect(Rect::new(3, 3, u32::MAX, u32::MAX), Rgb::new(0, 0, 0));
        canvas.fill_rect(
            Rect::new(i32::MAX, i32::MIN, u32::MAX, u32::MAX),
            Rgb::new(0, 0, 0),
        );
        let expect = [
            255, 255, PAD, PAD, //
            255, 255, PAD, PAD, //
            PAD, PAD, PAD, PAD, //
            PAD, PAD, PAD, 0,
        ];
        assert_eq!(buf, expect);
    }

    #[test]
    fn text_is_clipped_and_advances() {
        let layout = Layout::new(100, 10, 100, 1, PixelFormat::Gray).unwrap();
        let mut buf = canvas_buf(&layout);
        let mut canvas = Canvas::new(&mut buf, layout).unwrap();
        let white = Rgb::new(255, 255, 255);
        let black = Rgb::new(0, 0, 0);
        let end = canvas.draw_text(-3, -5, "WWWWWW", TextStyle::Body, white, black);
        assert_eq!(end, -3 + 6 * TextStyle::Body.char_width() as i32);
        assert!(buf.iter().any(|&b| b != PAD), "some glyph pixels visible");
        // Far off-screen text writes nothing and does not panic.
        let mut buf2 = canvas_buf(&layout);
        let mut canvas = Canvas::new(&mut buf2, layout).unwrap();
        canvas.draw_text(
            i32::MAX - 1,
            i32::MAX - 1,
            "abc",
            TextStyle::Title,
            white,
            black,
        );
        canvas.draw_text(i32::MIN, i32::MIN, "abc", TextStyle::Title, white, black);
        assert!(buf2.iter().all(|&b| b == PAD));
    }

    #[test]
    fn rounded_corners_leave_corner_pixel_unpainted() {
        let layout = Layout::new(20, 20, 20, 1, PixelFormat::Gray).unwrap();
        let mut buf = canvas_buf(&layout);
        let mut canvas = Canvas::new(&mut buf, layout).unwrap();
        canvas.fill_rounded_rect(Rect::new(0, 0, 20, 20), 6, Rgb::new(255, 255, 255));
        assert_eq!(buf[0], PAD, "top-left corner");
        assert_eq!(buf[19], PAD, "top-right corner");
        assert_eq!(buf[19 * 20], PAD, "bottom-left corner");
        assert_eq!(buf[10 * 20 + 10], 255, "centre");
        assert_eq!(buf[10], 255, "middle of top edge");
    }

    #[test]
    fn rect_geometry() {
        let r = Rect::new(10, 20, 5, 4);
        assert!(r.contains(10, 20) && r.contains(14, 23));
        assert!(!r.contains(15, 20) && !r.contains(10, 24) && !r.contains(9, 21));
        assert!(!Rect::new(i32::MAX, i32::MAX, u32::MAX, u32::MAX).contains(0, 0));
        assert!(r.intersects(&Rect::new(14, 23, 10, 10)));
        assert!(!r.intersects(&Rect::new(15, 20, 10, 10)));
        assert!(!r.intersects(&Rect::new(10, 20, 0, 10)));
    }

    #[test]
    fn blit_copies_only_the_clipped_rect() {
        let layout = Layout::new(4, 3, 6, 1, PixelFormat::Gray).unwrap();
        let src: std::vec::Vec<u8> = (0..18).collect();
        let mut buf = canvas_buf(&layout);
        let mut canvas = Canvas::new(&mut buf, layout).unwrap();
        assert!(canvas.blit_from(&src, Rect::new(-1, 1, 3, 10)));
        assert!(!canvas.blit_from(&src[..5], Rect::new(0, 0, 4, 3)));
        let expect = [
            PAD, PAD, PAD, PAD, PAD, PAD, //
            6, 7, PAD, PAD, PAD, PAD, //
            12, 13, PAD, PAD, PAD, PAD,
        ];
        assert_eq!(buf, expect);
    }

    #[test]
    fn isqrt_is_exact_floor() {
        for n in 0..10_000u64 {
            let r = isqrt(n);
            assert!(r * r <= n && (r + 1) * (r + 1) > n, "n={n}");
        }
    }
}
