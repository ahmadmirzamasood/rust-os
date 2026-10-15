//! Input events and the bounded queue between decoding and the UI.

use crate::keyboard::KeyEvent;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

impl MouseButton {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Middle => "middle",
        }
    }
}

/// A decoded input event. Pointer positions are absolute screen pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputEvent {
    Key(KeyEvent),
    PointerMoved {
        x: i32,
        y: i32,
    },
    PointerButton {
        button: MouseButton,
        pressed: bool,
        x: i32,
        y: i32,
    },
}

/// Fixed-capacity FIFO of events, used only in normal (non-interrupt)
/// context.
///
/// Overflow policy:
/// - consecutive [`InputEvent::PointerMoved`] events are always merged into
///   the newest one (positions are absolute, so nothing is lost but
///   intermediate points);
/// - otherwise, pushing into a full queue drops the *oldest* event and counts
///   it in [`EventQueue::dropped`], so the UI sees the most recent input.
#[derive(Clone, Debug)]
pub struct EventQueue<const N: usize> {
    events: [Option<InputEvent>; N],
    head: usize,
    len: usize,
    dropped: u32,
    coalesced: u32,
}

impl<const N: usize> Default for EventQueue<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> EventQueue<N> {
    const CAPACITY_OK: () = assert!(N > 0);

    pub const fn new() -> Self {
        let () = Self::CAPACITY_OK;
        Self {
            events: [None; N],
            head: 0,
            len: 0,
            dropped: 0,
            coalesced: 0,
        }
    }

    pub fn push(&mut self, event: InputEvent) {
        if let InputEvent::PointerMoved { .. } = event
            && self.len > 0
        {
            let newest = (self.head + self.len - 1) % N;
            if let Some(InputEvent::PointerMoved { .. }) = self.events[newest] {
                self.events[newest] = Some(event);
                self.coalesced = self.coalesced.wrapping_add(1);
                return;
            }
        }
        if self.len == N {
            self.events[self.head] = None;
            self.head = (self.head + 1) % N;
            self.len -= 1;
            self.dropped = self.dropped.wrapping_add(1);
        }
        self.events[(self.head + self.len) % N] = Some(event);
        self.len += 1;
    }

    pub fn pop(&mut self) -> Option<InputEvent> {
        if self.len == 0 {
            return None;
        }
        let event = self.events[self.head].take();
        self.head = (self.head + 1) % N;
        self.len -= 1;
        event
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Events dropped because the queue was full.
    pub const fn dropped(&self) -> u32 {
        self.dropped
    }

    /// Motion events merged into a newer one.
    pub const fn coalesced(&self) -> u32 {
        self.coalesced
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keyboard::Decoder;

    fn key(byte: u8) -> InputEvent {
        InputEvent::Key(Decoder::new().feed(byte).unwrap())
    }

    fn moved(x: i32) -> InputEvent {
        InputEvent::PointerMoved { x, y: 0 }
    }

    #[test]
    fn fifo_with_motion_coalescing() {
        let mut q = EventQueue::<4>::new();
        q.push(moved(1));
        q.push(moved(2));
        q.push(key(0x1E));
        q.push(moved(3));
        q.push(moved(4));
        assert_eq!(q.len(), 3);
        assert_eq!(q.coalesced(), 2);
        assert_eq!(q.pop(), Some(moved(2)));
        assert_eq!(q.pop(), Some(key(0x1E)));
        assert_eq!(q.pop(), Some(moved(4)));
        assert_eq!(q.pop(), None);
    }

    #[test]
    fn full_queue_drops_oldest() {
        let mut q = EventQueue::<2>::new();
        q.push(key(0x10));
        q.push(key(0x11));
        q.push(key(0x12));
        assert_eq!(q.dropped(), 1);
        assert_eq!(q.pop(), Some(key(0x11)));
        assert_eq!(q.pop(), Some(key(0x12)));
        assert!(q.is_empty());
    }

    #[test]
    fn long_bursts_stay_bounded() {
        let mut q = EventQueue::<8>::new();
        for i in 0..10_000u32 {
            q.push(if i % 3 == 0 {
                key(0x1E)
            } else {
                moved(i as i32)
            });
            assert!(q.len() <= 8);
        }
        let mut n = 0;
        while q.pop().is_some() {
            n += 1;
        }
        assert_eq!(n, 8);
        assert!(q.dropped() > 0);
    }
}
