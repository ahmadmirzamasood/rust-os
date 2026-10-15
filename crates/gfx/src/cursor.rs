//! The mouse pointer: a classic arrow, hot spot at its tip (top-left).

use crate::canvas::{Canvas, Rect};
use crate::theme;

/// `#` outline, `.` fill, space transparent.
const ARROW: [&str; 19] = [
    "#           ",
    "##          ",
    "#.#         ",
    "#..#        ",
    "#...#       ",
    "#....#      ",
    "#.....#     ",
    "#......#    ",
    "#.......#   ",
    "#........#  ",
    "#.........# ",
    "#......#####",
    "#...#..#    ",
    "#..##..#    ",
    "#.# #..#    ",
    "##   #..#   ",
    "#    #..#   ",
    "      #..#  ",
    "       ##   ",
];

pub const WIDTH: u32 = 12;
pub const HEIGHT: u32 = ARROW.len() as u32;

/// Pixels the pointer may cover when its hot spot is at (`x`, `y`).
pub const fn bounds(x: i32, y: i32) -> Rect {
    Rect::new(x, y, WIDTH, HEIGHT)
}

/// Draw the pointer with its hot spot at (`x`, `y`). Only the arrow's own
/// pixels are written, and drawing is clipped like every canvas operation.
pub fn draw(canvas: &mut Canvas<'_>, x: i32, y: i32) {
    for (dy, row) in ARROW.iter().enumerate() {
        for (dx, cell) in row.bytes().enumerate() {
            let color = match cell {
                b'#' => theme::CURSOR_OUTLINE,
                b'.' => theme::CURSOR_FILL,
                _ => continue,
            };
            canvas.set_pixel(
                x.saturating_add(dx as i32),
                y.saturating_add(dy as i32),
                color,
            );
        }
    }
}

/// Number of fill pixels in the arrow (for screenshot checks).
pub fn fill_pixels() -> usize {
    ARROW
        .iter()
        .map(|row| row.bytes().filter(|&c| c == b'.').count())
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{Layout, PixelFormat};
    use crate::screens::count_color;
    use std::vec;

    #[test]
    fn rows_fit_the_declared_width() {
        assert!(ARROW.iter().all(|row| row.len() == WIDTH as usize));
        assert!(fill_pixels() > 40);
    }

    #[test]
    fn draws_tip_at_hot_spot_and_clips() {
        let layout = Layout::new(40, 40, 40, 3, PixelFormat::Rgb).unwrap();
        let mut buf = vec![0u8; 40 * 40 * 3];
        let mut canvas = Canvas::new(&mut buf, layout).unwrap();
        draw(&mut canvas, 5, 7);
        let o = (7 * 40 + 5) * 3;
        let c = theme::CURSOR_OUTLINE;
        assert_eq!(buf[o..o + 3], [c.r, c.g, c.b]);
        assert_eq!(count_color(&buf, theme::CURSOR_FILL), fill_pixels());
        // Partly and fully off-screen pointers do not panic.
        let mut canvas = Canvas::new(&mut buf, layout).unwrap();
        for (x, y) in [(35, 35), (-5, -5), (i32::MAX, i32::MAX), (i32::MIN, 0)] {
            draw(&mut canvas, x, y);
        }
    }
}
