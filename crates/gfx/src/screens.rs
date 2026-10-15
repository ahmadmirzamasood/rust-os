//! Boot welcome screen and kernel panic banner.

use crate::canvas::{Canvas, Rect};
use crate::text::TextStyle;
use crate::theme;
use crate::theme::Rgb;

/// Outcome of one boot stage, shown as a coloured dot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StageState {
    Ok,
    Degraded,
}

/// One row of the boot-status list.
#[derive(Clone, Copy, Debug)]
pub struct StatusLine<'a> {
    pub label: &'a str,
    pub value: &'a str,
    pub state: StageState,
}

/// Content of the welcome screen.
#[derive(Clone, Copy, Debug)]
pub struct Welcome<'a> {
    pub title: &'a str,
    pub subtitle: &'a str,
    pub lines: &'a [StatusLine<'a>],
    pub footer: &'a str,
}

const CARD_MAX_WIDTH: u32 = 760;
const MARGIN: u32 = 24;
const PADDING: u32 = 32;
const RADIUS: u32 = 16;
const LOGO: u32 = 40;
const ROW_GAP: u32 = 12;
const DOT: u32 = 8;

/// Where [`draw_welcome`] put the card. Exposed for tests and screenshots.
pub fn welcome_card(screen_w: usize, screen_h: usize, line_count: usize) -> Rect {
    let body = TextStyle::Body.line_height() as u32;
    let rows = u32::try_from(line_count).unwrap_or(u32::MAX);
    let height = PADDING
        + LOGO
        + 8
        + body // subtitle
        + 24
        + rows.saturating_mul(body + ROW_GAP)
        + 12
        + body // footer
        + PADDING;
    let screen_w = u32::try_from(screen_w).unwrap_or(u32::MAX);
    let screen_h = u32::try_from(screen_h).unwrap_or(u32::MAX);
    let width = CARD_MAX_WIDTH.min(screen_w.saturating_sub(2 * MARGIN).max(1));
    let x = screen_w.saturating_sub(width) / 2;
    let y = screen_h.saturating_sub(height) / 2;
    Rect::new(
        i32::try_from(x).unwrap_or(0),
        i32::try_from(y).unwrap_or(0),
        width,
        height,
    )
}

/// Draw the full-screen welcome view: background, card, logo mark, project
/// name, subtitle, one row per boot stage, and a footer.
pub fn draw_welcome(canvas: &mut Canvas<'_>, welcome: &Welcome<'_>) {
    canvas.clear(theme::BACKGROUND);
    let card = welcome_card(canvas.width(), canvas.height(), welcome.lines.len());
    canvas.fill_rounded_rect(card, RADIUS, theme::CARD);

    let pad = PADDING as i32;
    let left = card.x + pad;
    let mut y = card.y + pad;

    // Logo mark: an accent tile with a smaller card-coloured notch, original
    // to this project.
    canvas.fill_rounded_rect(Rect::new(left, y, LOGO, LOGO), 10, theme::ACCENT);
    canvas.fill_rounded_rect(Rect::new(left + 12, y + 12, 16, 16), 4, theme::CARD);

    let title_h = TextStyle::Title.line_height() as i32;
    let title_x = left + LOGO as i32 + 16;
    canvas.draw_text(
        title_x,
        y + (LOGO as i32 - title_h) / 2,
        welcome.title,
        TextStyle::Title,
        theme::TITLE,
        theme::CARD,
    );
    y += LOGO as i32 + 8;
    canvas.draw_text(
        left,
        y,
        welcome.subtitle,
        TextStyle::Body,
        theme::MUTED,
        theme::CARD,
    );
    y += TextStyle::Body.line_height() as i32 + 12;

    // Divider.
    let inner_w = card.w.saturating_sub(2 * PADDING);
    canvas.fill_rect(
        Rect::new(left, y, inner_w, 1),
        theme::CARD.blend(theme::MUTED, 70),
    );
    y += 12;

    let label_w = welcome
        .lines
        .iter()
        .map(|line| TextStyle::Body.measure(line.label))
        .max()
        .unwrap_or(0);
    let value_x = left + DOT as i32 + 12 + i32::try_from(label_w).unwrap_or(0) + 20;
    let body_h = TextStyle::Body.line_height() as i32;
    for line in welcome.lines {
        let dot = match line.state {
            StageState::Ok => theme::OK,
            StageState::Degraded => theme::WARN,
        };
        canvas.fill_rounded_rect(
            Rect::new(left, y + (body_h - DOT as i32) / 2, DOT, DOT),
            DOT / 2,
            dot,
        );
        canvas.draw_text(
            left + DOT as i32 + 12,
            y,
            line.label,
            TextStyle::Body,
            theme::MUTED,
            theme::CARD,
        );
        canvas.draw_text(
            value_x,
            y,
            line.value,
            TextStyle::Body,
            theme::TEXT,
            theme::CARD,
        );
        y += body_h + ROW_GAP as i32;
    }

    canvas.draw_text(
        left,
        y + 12,
        welcome.footer,
        TextStyle::Body,
        theme::MUTED,
        theme::CARD,
    );
}

/// Draw a red banner across the bottom of the screen with a heading and up
/// to a few lines of detail. Used by the panic handler; it only draws, so it
/// is safe to call on top of whatever is already shown.
pub fn draw_panic_banner(canvas: &mut Canvas<'_>, lines: &[&str]) {
    let body_h = TextStyle::Body.line_height() as i32;
    let title_h = TextStyle::Strong.line_height() as i32;
    let rows = i32::try_from(lines.len()).unwrap_or(0).min(8);
    let height = 20 + title_h + 8 + rows * (body_h + 6) + 14;
    let screen_h = i32::try_from(canvas.height()).unwrap_or(i32::MAX);
    let top = (screen_h - height).max(0);
    let width = u32::try_from(canvas.width()).unwrap_or(u32::MAX);
    canvas.fill_rect(
        Rect::new(0, top, width, height.unsigned_abs()),
        theme::PANIC_BG,
    );
    let mut y = top + 20;
    canvas.draw_text(
        24,
        y,
        "KERNEL PANIC - system halted. Details are on the serial port.",
        TextStyle::Strong,
        theme::PANIC_TEXT,
        theme::PANIC_BG,
    );
    y += title_h + 8;
    for line in lines.iter().take(8) {
        canvas.draw_text(
            24,
            y,
            line,
            TextStyle::Body,
            theme::PANIC_TEXT,
            theme::PANIC_BG,
        );
        y += body_h + 6;
    }
}

/// Count pixels in an RGB image exactly equal to `color`. Shared by tests and
/// the host smoke test's screenshot check.
pub fn count_color(rgb: &[u8], color: Rgb) -> usize {
    let target = [color.r, color.g, color.b];
    rgb.as_chunks::<3>()
        .0
        .iter()
        .filter(|px| **px == target)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{Layout, PixelFormat};
    use std::vec;
    use std::vec::Vec;

    fn to_rgb(buf: &[u8], layout: &Layout) -> Vec<u8> {
        // Decode a BGRx buffer with stride back to packed RGB.
        let mut out = Vec::new();
        for y in 0..layout.height() {
            for x in 0..layout.width() {
                let o = (y * layout.stride() + x) * 4;
                out.extend_from_slice(&[buf[o + 2], buf[o + 1], buf[o]]);
            }
        }
        out
    }

    fn sample_welcome() -> Welcome<'static> {
        const LINES: &[StatusLine<'static>] = &[
            StatusLine {
                label: "Kernel",
                value: "entered",
                state: StageState::Ok,
            },
            StatusLine {
                label: "Graphics",
                value: "degraded",
                state: StageState::Degraded,
            },
        ];
        Welcome {
            title: "rust-os",
            subtitle: "Experimental operating system",
            lines: LINES,
            footer: "footer",
        }
    }

    #[test]
    fn welcome_renders_expected_colors_with_padded_bgr() {
        let layout = Layout::new(800, 600, 832, 4, PixelFormat::Bgr).unwrap();
        let mut buf = vec![0xAAu8; 832 * 600 * 4];
        let mut canvas = Canvas::new(&mut buf, layout).unwrap();
        draw_welcome(&mut canvas, &sample_welcome());

        // Stride padding untouched.
        for y in 0..600 {
            let o = (y * 832 + 800) * 4;
            assert!(buf[o..o + 32 * 4].iter().all(|&b| b == 0xAA));
        }
        let rgb = to_rgb(&buf, &layout);
        assert_eq!(
            rgb[..3],
            [
                theme::BACKGROUND.r,
                theme::BACKGROUND.g,
                theme::BACKGROUND.b
            ]
        );
        assert!(count_color(&rgb, theme::TITLE) > 100, "title glyphs");
        assert!(count_color(&rgb, theme::OK) > 20, "ok dot");
        assert!(count_color(&rgb, theme::WARN) > 20, "degraded dot");
        let card = welcome_card(800, 600, 2);
        let cx = (card.x + card.w as i32 / 2) as usize;
        let cy = (card.y + 4) as usize;
        let o = (cy * 800 + cx) * 3;
        assert_eq!(rgb[o..o + 3], [theme::CARD.r, theme::CARD.g, theme::CARD.b]);
    }

    #[test]
    fn welcome_survives_tiny_screens() {
        for (w, h) in [(1, 1), (17, 9), (320, 200)] {
            let layout = Layout::new(w, h, w, 4, PixelFormat::Rgb).unwrap();
            let mut buf = vec![0u8; w * h * 4];
            let mut canvas = Canvas::new(&mut buf, layout).unwrap();
            draw_welcome(&mut canvas, &sample_welcome());
            draw_panic_banner(&mut canvas, &["a", "b"]);
        }
    }

    #[test]
    fn panic_banner_paints_bottom_band() {
        let layout = Layout::new(640, 480, 640, 4, PixelFormat::Rgb).unwrap();
        let mut buf = vec![0u8; 640 * 480 * 4];
        let mut canvas = Canvas::new(&mut buf, layout).unwrap();
        draw_panic_banner(&mut canvas, &["panicked at x.rs:1:1", "boom"]);
        let last = (479 * 640) * 4;
        assert_eq!(
            buf[last..last + 3],
            [theme::PANIC_BG.r, theme::PANIC_BG.g, theme::PANIC_BG.b]
        );
        assert_eq!(buf[..4], [0, 0, 0, 0], "top of screen untouched");
    }
}
