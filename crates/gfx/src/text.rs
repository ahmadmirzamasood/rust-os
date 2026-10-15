//! Bitmap text styles and an allocation-free formatting buffer.

use core::fmt;
use noto_sans_mono_bitmap::{
    FontWeight, RasterHeight, RasterizedChar, get_raster, get_raster_width,
};

/// The text styles the boot screens use. Each maps to one monospace raster.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextStyle {
    /// Regular, 16 px.
    Body,
    /// Bold, 16 px.
    Strong,
    /// Bold, 32 px.
    Title,
}

impl TextStyle {
    const fn weight(self) -> FontWeight {
        match self {
            Self::Body => FontWeight::Regular,
            Self::Strong | Self::Title => FontWeight::Bold,
        }
    }

    const fn raster_height(self) -> RasterHeight {
        match self {
            Self::Body | Self::Strong => RasterHeight::Size16,
            Self::Title => RasterHeight::Size32,
        }
    }

    /// Advance per character in pixels (the font is monospace).
    pub const fn char_width(self) -> usize {
        get_raster_width(self.weight(), self.raster_height())
    }

    /// Height of one line of glyphs in pixels.
    pub const fn line_height(self) -> usize {
        self.raster_height().val()
    }

    /// Width of `text` in pixels.
    pub fn measure(self, text: &str) -> usize {
        text.chars().count().saturating_mul(self.char_width())
    }

    /// Glyph for `c`, falling back to `?` for characters outside the font.
    pub(crate) fn glyph(self, c: char) -> Option<RasterizedChar> {
        get_raster(c, self.weight(), self.raster_height())
            .or_else(|| get_raster('?', self.weight(), self.raster_height()))
    }
}

/// Fixed-capacity UTF-8 buffer implementing [`fmt::Write`].
///
/// Output that does not fit is dropped at a character boundary and recorded in
/// [`FmtBuf::truncated`], so formatting never fails and never allocates. This
/// is what the kernel uses to build status and panic text.
#[derive(Clone, Debug)]
pub struct FmtBuf<const N: usize> {
    bytes: [u8; N],
    len: usize,
    truncated: bool,
}

impl<const N: usize> FmtBuf<N> {
    pub const fn new() -> Self {
        Self {
            bytes: [0; N],
            len: 0,
            truncated: false,
        }
    }

    pub fn as_str(&self) -> &str {
        // Only whole UTF-8 characters are ever copied in, so this cannot fail.
        core::str::from_utf8(&self.bytes[..self.len]).unwrap_or("")
    }

    /// Whether some output was dropped for lack of space.
    pub const fn truncated(&self) -> bool {
        self.truncated
    }

    pub fn clear(&mut self) {
        self.len = 0;
        self.truncated = false;
    }
}

impl<const N: usize> Default for FmtBuf<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> fmt::Write for FmtBuf<N> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for c in s.chars() {
            let mut utf8 = [0u8; 4];
            let encoded = c.encode_utf8(&mut utf8).as_bytes();
            let end = self.len + encoded.len();
            if end > N {
                self.truncated = true;
                break;
            }
            self.bytes[self.len..end].copy_from_slice(encoded);
            self.len = end;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::fmt::Write;

    #[test]
    fn fmtbuf_truncates_at_char_boundary() {
        let mut buf = FmtBuf::<5>::new();
        write!(buf, "ab\u{e9}cd").unwrap(); // "é" is two bytes
        assert_eq!(buf.as_str(), "ab\u{e9}c");
        assert!(buf.truncated());
        buf.clear();
        write!(buf, "{}", 42).unwrap();
        assert_eq!(buf.as_str(), "42");
        assert!(!buf.truncated());
    }

    #[test]
    fn styles_have_expected_metrics() {
        assert_eq!(TextStyle::Body.line_height(), 16);
        assert_eq!(TextStyle::Title.line_height(), 32);
        assert!(TextStyle::Title.char_width() > TextStyle::Body.char_width());
        assert_eq!(
            TextStyle::Body.measure("abcd"),
            4 * TextStyle::Body.char_width()
        );
    }

    #[test]
    fn unknown_characters_fall_back() {
        assert!(TextStyle::Body.glyph('\u{4e2d}').is_some());
    }
}
