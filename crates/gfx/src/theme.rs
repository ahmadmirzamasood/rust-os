//! Colours used by the boot screens.
//!
//! The host-side boot smoke test reads these same constants to check a QEMU
//! screenshot, so a pixel-format bug (for example red and blue swapped) shows
//! up as a test failure. Keep `BACKGROUND` asymmetric in red and blue for that
//! reason.

/// A 24-bit sRGB colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Mix `self` toward `other`; `alpha` 0 keeps `self`, 255 gives `other`.
    pub const fn blend(self, other: Rgb, alpha: u8) -> Rgb {
        const fn mix(a: u8, b: u8, t: u8) -> u8 {
            let t = t as u32;
            // Result is at most 255 because it is a weighted mean of two u8s.
            ((a as u32 * (255 - t) + b as u32 * t + 127) / 255) as u8
        }
        Rgb::new(
            mix(self.r, other.r, alpha),
            mix(self.g, other.g, alpha),
            mix(self.b, other.b, alpha),
        )
    }
}

/// Screen background behind the boot card.
pub const BACKGROUND: Rgb = Rgb::new(0x1d, 0x26, 0x3b);
/// Boot card surface.
pub const CARD: Rgb = Rgb::new(0x27, 0x33, 0x4d);
/// Logo mark and divider accent.
pub const ACCENT: Rgb = Rgb::new(0x6c, 0xa8, 0xf0);
/// Project name.
pub const TITLE: Rgb = Rgb::new(0xf4, 0xf6, 0xfa);
/// Status values.
pub const TEXT: Rgb = Rgb::new(0xc9, 0xd2, 0xe3);
/// Labels, subtitle and footer.
pub const MUTED: Rgb = Rgb::new(0x8a, 0x97, 0xb0);
/// Status dot: stage succeeded.
pub const OK: Rgb = Rgb::new(0x7d, 0xd3, 0x9b);
/// Status dot: stage degraded or skipped.
pub const WARN: Rgb = Rgb::new(0xf2, 0xc1, 0x4e);
/// Input panel surface (darker than the card so the two read as separate).
pub const PANEL: Rgb = Rgb::new(0x22, 0x2c, 0x43);
/// Push button at rest.
pub const BUTTON: Rgb = Rgb::new(0x35, 0x44, 0x63);
/// Push button with the pointer over it.
pub const BUTTON_HOVER: Rgb = Rgb::new(0x44, 0x57, 0x7d);
/// Push button held down.
pub const BUTTON_PRESSED: Rgb = Rgb::new(0x4f, 0x8c, 0xd9);
/// Pointer fill. Pure white appears nowhere else, so the smoke test can find
/// the pointer in a screenshot.
pub const CURSOR_FILL: Rgb = Rgb::new(0xff, 0xff, 0xff);
/// Pointer outline.
pub const CURSOR_OUTLINE: Rgb = Rgb::new(0x0b, 0x0e, 0x14);
/// Kernel panic banner.
pub const PANIC_BG: Rgb = Rgb::new(0x8c, 0x1d, 0x2b);
/// Kernel panic banner text.
pub const PANIC_TEXT: Rgb = Rgb::new(0xff, 0xf1, 0xf1);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blend_endpoints_and_midpoint() {
        let a = Rgb::new(0, 100, 255);
        let b = Rgb::new(255, 100, 0);
        assert_eq!(a.blend(b, 0), a);
        assert_eq!(a.blend(b, 255), b);
        assert_eq!(a.blend(b, 128), Rgb::new(128, 100, 127));
    }

    #[test]
    fn background_detects_channel_swaps() {
        assert_ne!(BACKGROUND.r, BACKGROUND.b);
    }
}
