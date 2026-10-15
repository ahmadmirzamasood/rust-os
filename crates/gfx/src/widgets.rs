//! Small reusable widgets. Each separates behaviour (a host-testable state
//! machine fed with pointer events) from drawing.

use crate::canvas::{Canvas, Rect};
use crate::text::TextStyle;
use crate::theme;

/// How a push button looks right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonLook {
    Normal,
    Hover,
    Pressed,
}

/// Push-button behaviour: a click is a left press *and* release inside the
/// button. Pressing inside and releasing outside cancels; pressing outside
/// and releasing inside does nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PushButton {
    rect: Rect,
    hover: bool,
    armed: bool,
}

impl PushButton {
    pub const fn new(rect: Rect) -> Self {
        Self {
            rect,
            hover: false,
            armed: false,
        }
    }

    pub const fn rect(&self) -> Rect {
        self.rect
    }

    pub fn look(&self) -> ButtonLook {
        match (self.hover, self.armed) {
            (true, true) => ButtonLook::Pressed,
            (true, false) => ButtonLook::Hover,
            _ => ButtonLook::Normal,
        }
    }

    /// Pointer moved to (`x`, `y`). Returns whether the look changed.
    pub fn pointer_moved(&mut self, x: i32, y: i32) -> bool {
        let before = self.look();
        self.hover = self.rect.contains(x, y);
        self.look() != before
    }

    /// Left button changed at (`x`, `y`). Returns `(clicked, look_changed)`.
    pub fn left_button(&mut self, pressed: bool, x: i32, y: i32) -> (bool, bool) {
        let before = self.look();
        self.hover = self.rect.contains(x, y);
        let clicked = if pressed {
            self.armed = self.hover;
            false
        } else {
            let clicked = self.armed && self.hover;
            self.armed = false;
            clicked
        };
        (clicked, self.look() != before)
    }
}

/// Draw a push button with a centred label.
pub fn draw_button(canvas: &mut Canvas<'_>, rect: Rect, label: &str, look: ButtonLook) {
    let fill = match look {
        ButtonLook::Normal => theme::BUTTON,
        ButtonLook::Hover => theme::BUTTON_HOVER,
        ButtonLook::Pressed => theme::BUTTON_PRESSED,
    };
    canvas.fill_rounded_rect(rect, 8, fill);
    let style = TextStyle::Strong;
    let text_w = i32::try_from(style.measure(label)).unwrap_or(i32::MAX);
    let text_h = style.line_height() as i32;
    let w = i32::try_from(rect.w).unwrap_or(i32::MAX);
    let h = i32::try_from(rect.h).unwrap_or(i32::MAX);
    canvas.draw_text(
        rect.x.saturating_add((w - text_w).max(0) / 2),
        rect.y.saturating_add((h - text_h) / 2),
        label,
        style,
        theme::TITLE,
        fill,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn button() -> PushButton {
        PushButton::new(Rect::new(10, 10, 20, 10))
    }

    #[test]
    fn press_and_release_inside_clicks() {
        let mut b = button();
        assert_eq!(b.look(), ButtonLook::Normal);
        assert!(b.pointer_moved(15, 15));
        assert_eq!(b.look(), ButtonLook::Hover);
        assert_eq!(b.left_button(true, 15, 15), (false, true));
        assert_eq!(b.look(), ButtonLook::Pressed);
        assert_eq!(b.left_button(false, 16, 16), (true, true));
        assert_eq!(b.look(), ButtonLook::Hover);
    }

    #[test]
    fn dragging_out_cancels_and_outside_press_never_clicks() {
        let mut b = button();
        b.left_button(true, 15, 15);
        b.pointer_moved(100, 100);
        assert_eq!(b.look(), ButtonLook::Normal);
        assert!(!b.left_button(false, 100, 100).0);
        // Press outside, move in, release inside: no click.
        b.left_button(true, 0, 0);
        b.pointer_moved(15, 15);
        assert_eq!(b.look(), ButtonLook::Hover);
        assert!(!b.left_button(false, 15, 15).0);
        // Right/bottom edges are outside.
        b.left_button(true, 29, 19);
        assert!(b.left_button(false, 29, 19).0);
        b.left_button(true, 30, 19);
        assert!(!b.left_button(false, 30, 19).0);
    }
}
