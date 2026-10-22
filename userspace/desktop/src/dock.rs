//! Pinned application dock. The same geometry drives painting and hit testing.
use gfx::{Canvas, Rect, Rgb, TextStyle};
pub const LABELS: [&str; 6] = ["Computer", "Files", "Trash", "Settings", "About", "Tasks"];
const CELL: i32 = 76;

pub fn bounds(width: u32, height: u32) -> Rect {
    Rect::new(
        (width as i32 - CELL * 6 - 16) / 2,
        height as i32 - 84,
        (CELL * 6 + 16) as u32,
        74,
    )
}
pub fn item(width: u32, height: u32, index: usize) -> Rect {
    let b = bounds(width, height);
    Rect::new(b.x + 8 + index as i32 * CELL, b.y + 4, CELL as u32, 66)
}
pub fn hit(width: u32, height: u32, x: i32, y: i32) -> Option<usize> {
    (0..6).find(|&i| item(width, height, i).contains(x, y))
}
pub fn paint(
    canvas: &mut Canvas<'_>,
    running: [bool; 6],
    focused: Option<usize>,
    hover: Option<usize>,
) {
    let b = bounds(canvas.width() as u32, canvas.height() as u32);
    canvas.fill_rounded_rect(
        Rect::new(b.x + 3, b.y + 4, b.w, b.h),
        14,
        Rgb::new(13, 18, 28),
    );
    canvas.fill_rounded_rect(b, 14, Rgb::new(34, 45, 64));
    for (i, label) in LABELS.iter().enumerate() {
        let r = item(canvas.width() as u32, canvas.height() as u32, i);
        let active = focused == Some(i);
        let bg = if hover == Some(i) {
            Rgb::new(63, 85, 113)
        } else if active {
            Rgb::new(49, 69, 96)
        } else {
            Rgb::new(34, 45, 64)
        };
        canvas.fill_rounded_rect(Rect::new(r.x + 3, r.y + 1, r.w - 6, r.h - 2), 9, bg);
        let accent = [
            Rgb::new(100, 188, 226),
            Rgb::new(238, 191, 107),
            Rgb::new(173, 188, 209),
            Rgb::new(162, 147, 227),
            Rgb::new(122, 199, 179),
            Rgb::new(112, 198, 158),
        ][i];
        let (x, y) = (r.x + 23, r.y + 8);
        canvas.fill_rounded_rect(Rect::new(x, y, 30, 28), 5, accent);
        let ink = Rgb::new(27, 39, 57);
        match i {
            0 => {
                canvas.fill_rect(Rect::new(x + 4, y + 4, 22, 15), ink);
                canvas.fill_rect(Rect::new(x + 13, y + 19, 4, 5), ink);
            }
            1 => {
                canvas.fill_rect(Rect::new(x + 4, y + 9, 22, 14), ink);
                canvas.fill_rect(Rect::new(x + 4, y + 5, 10, 5), ink);
            }
            2 => {
                canvas.fill_rect(Rect::new(x + 6, y + 6, 18, 2), ink);
                for dx in [9, 14, 19] {
                    canvas.fill_rect(Rect::new(x + dx, y + 11, 2, 11), ink);
                }
            }
            3 => {
                for (dy, dx) in [(6, 9), (13, 19), (20, 12)] {
                    canvas.fill_rect(Rect::new(x + 4, y + dy, 22, 2), ink);
                    canvas.fill_rect(Rect::new(x + dx, y + dy - 2, 4, 6), ink);
                }
            }
            4 => {
                canvas.fill_rect(Rect::new(x + 14, y + 5, 3, 3), ink);
                canvas.fill_rect(Rect::new(x + 14, y + 11, 3, 12), ink);
            }
            _ => {
                for (dx, h) in [(6, 9), (13, 18), (20, 13)] {
                    canvas.fill_rect(Rect::new(x + dx, y + 24 - h, 4, h as u32), ink);
                }
            }
        }
        let text_x = r.x + (CELL - TextStyle::Body.measure(label) as i32) / 2;
        canvas.draw_text(
            text_x,
            r.y + 40,
            label,
            TextStyle::Body,
            Rgb::new(235, 241, 249),
            bg,
        );
        if running[i] {
            canvas.fill_rounded_rect(
                Rect::new(r.x + 32, r.y + 60, 12, 3),
                1,
                if active {
                    accent
                } else {
                    Rgb::new(125, 143, 168)
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_pinned_apps_have_stable_hit_targets_and_edges_do_not_launch() {
        for (w, h) in [(640, 480), (1280, 800), (1365, 1024)] {
            for i in 0..6 {
                let r = item(w, h, i);
                assert_eq!(hit(w, h, r.x + 30, r.y + 30), Some(i));
            }
            assert_eq!(hit(w, h, 0, 0), None);
            assert_eq!(hit(w, h, bounds(w, h).x - 1, h as i32 - 40), None);
        }
    }
}
