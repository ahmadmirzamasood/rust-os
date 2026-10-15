//! Absolute pointer state driven by relative mouse packets.

use crate::event::{EventQueue, InputEvent, MouseButton};
use crate::mouse::{Buttons, Packet};

/// Pointer position, clamped to `[0, width) x [0, height)`, and buttons.
/// Movement is applied 1:1 (no acceleration).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pointer {
    x: i32,
    y: i32,
    max_x: i32,
    max_y: i32,
    buttons: Buttons,
}

impl Pointer {
    /// A pointer at the centre of a `width` x `height` screen (at least
    /// 1x1).
    pub fn new(width: u32, height: u32) -> Self {
        let max_x = i32::try_from(width.max(1) - 1).unwrap_or(i32::MAX);
        let max_y = i32::try_from(height.max(1) - 1).unwrap_or(i32::MAX);
        Self {
            x: max_x / 2,
            y: max_y / 2,
            max_x,
            max_y,
            buttons: Buttons::default(),
        }
    }

    pub const fn position(&self) -> (i32, i32) {
        (self.x, self.y)
    }

    pub const fn buttons(&self) -> Buttons {
        self.buttons
    }

    /// Apply `packet`, pushing a motion event (if the position changed) and
    /// then one event per button that changed state.
    pub fn apply<const N: usize>(&mut self, packet: Packet, queue: &mut EventQueue<N>) {
        let x = self
            .x
            .saturating_add(i32::from(packet.dx))
            .clamp(0, self.max_x);
        let y = self
            .y
            .saturating_add(i32::from(packet.dy))
            .clamp(0, self.max_y);
        if (x, y) != (self.x, self.y) {
            self.x = x;
            self.y = y;
            queue.push(InputEvent::PointerMoved { x, y });
        }
        let old = self.buttons;
        let new = packet.buttons;
        for (button, was, is) in [
            (MouseButton::Left, old.left, new.left),
            (MouseButton::Right, old.right, new.right),
            (MouseButton::Middle, old.middle, new.middle),
        ] {
            if was != is {
                queue.push(InputEvent::PointerButton {
                    button,
                    pressed: is,
                    x,
                    y,
                });
            }
        }
        self.buttons = new;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(dx: i16, dy: i16, left: bool) -> Packet {
        Packet {
            dx,
            dy,
            buttons: Buttons {
                left,
                ..Buttons::default()
            },
            overflow: false,
        }
    }

    #[test]
    fn starts_centred_and_clamps_to_screen() {
        let mut p = Pointer::new(100, 50);
        assert_eq!(p.position(), (49, 24));
        let mut q = EventQueue::<8>::new();
        p.apply(packet(-200, -200, false), &mut q);
        assert_eq!(p.position(), (0, 0));
        p.apply(packet(255, 255, false), &mut q);
        p.apply(packet(255, 255, false), &mut q);
        assert_eq!(p.position(), (99, 49));
        // Already at the corner: no motion event.
        let before = q.len();
        p.apply(packet(10, 10, false), &mut q);
        assert_eq!(q.len(), before);
    }

    #[test]
    fn button_changes_follow_motion_with_new_position() {
        let mut p = Pointer::new(100, 100);
        let mut q = EventQueue::<8>::new();
        p.apply(packet(1, 2, true), &mut q);
        assert_eq!(q.pop(), Some(InputEvent::PointerMoved { x: 50, y: 51 }));
        assert_eq!(
            q.pop(),
            Some(InputEvent::PointerButton {
                button: MouseButton::Left,
                pressed: true,
                x: 50,
                y: 51
            })
        );
        p.apply(packet(0, 0, true), &mut q);
        assert!(q.is_empty(), "held button repeats nothing");
        p.apply(packet(0, 0, false), &mut q);
        assert!(matches!(
            q.pop(),
            Some(InputEvent::PointerButton { pressed: false, .. })
        ));
    }

    #[test]
    fn degenerate_screen_sizes() {
        let p = Pointer::new(0, 0);
        assert_eq!(p.position(), (0, 0));
        let mut p = Pointer::new(u32::MAX, u32::MAX);
        let mut q = EventQueue::<2>::new();
        p.apply(packet(i16::MAX, i16::MAX, false), &mut q);
        assert!(p.position().0 > 0);
    }
}
