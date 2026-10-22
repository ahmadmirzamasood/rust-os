//! Workspace switcher geometry shared by the guest shell and input tests.
use crate::display::{Display, WORKSPACE_COUNT};
use gfx::{Canvas, Rect, Rgb, TextStyle};

pub const ACTIVE: Rgb = Rgb::new(56, 107, 150);
const WIDTH: i32 = 176;

pub fn item(width: u32, _height: u32, index: usize) -> Rect {
    Rect::new(
        (width as i32 - WIDTH) / 2 + 2 + index as i32 * 43,
        4,
        40,
        28,
    )
}

pub fn hit(width: u32, height: u32, x: i32, y: i32) -> Option<usize> {
    (0..WORKSPACE_COUNT).find(|&i| item(width, height, i).contains(x, y))
}

pub fn paint(canvas: &mut Canvas<'_>, display: &Display, hover: Option<usize>) {
    for i in 0..WORKSPACE_COUNT {
        let r = item(canvas.width() as u32, canvas.height() as u32, i);
        let bg = if i == display.current_workspace() {
            ACTIVE
        } else if hover == Some(i) {
            Rgb::new(58, 73, 94)
        } else {
            Rgb::new(37, 47, 64)
        };
        canvas.fill_rounded_rect(r, 5, bg);
        canvas.draw_text(
            r.x + 16,
            r.y + 3,
            ["1", "2", "3", "4"][i],
            TextStyle::Body,
            Rgb::new(236, 242, 248),
            bg,
        );
        let count = display.workspace_window_count(i).min(6);
        for n in 0..count {
            canvas.fill_rect(
                Rect::new(r.x + 8 + n as i32 * 4, r.y + 24, 2, 2),
                Rgb::new(158, 189, 220),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switcher_targets_fit_panel_and_do_not_overlap_applications_or_clock() {
        for (width, height) in [(640, 480), (1280, 800), (1365, 1024)] {
            for i in 0..WORKSPACE_COUNT {
                let r = item(width, height, i);
                assert!(r.x >= 220);
                assert!(r.x + r.w as i32 <= width as i32 - 190);
                assert_eq!(hit(width, height, r.x + 20, r.y + 10), Some(i));
                assert_eq!(hit(width, height, r.x + 40, r.y + 10), None);
            }
            assert_eq!(hit(width, height, 0, 0), None);
            assert_eq!(hit(width, height, width as i32 / 2, 36), None);
        }
    }
}
