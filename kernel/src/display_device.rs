//! Bounded copy-only input handoff. Only the event-loop thread produces events.
use crate::sync::IrqCell;
use input::event::InputEvent;

#[derive(Debug)]
struct State {
    pid: u32,
    queue: [[u8; 16]; 64],
    head: usize,
    len: usize,
    restore_diagnostics: bool,
}
static STATE: IrqCell<State> = IrqCell::new(State {
    pid: 0,
    queue: [[0; 16]; 64],
    head: 0,
    len: 0,
    restore_diagnostics: false,
});
pub fn claim(pid: u32) {
    STATE.with(|s| {
        if s.pid != pid {
            s.head = 0;
            s.len = 0;
            s.pid = pid;
        }
    });
}
pub fn active() -> bool {
    STATE.with(|s| s.pid != 0)
}
pub fn release(pid: u32) {
    STATE.with(|s| {
        if s.pid == pid {
            s.pid = 0;
            s.len = 0;
            s.restore_diagnostics = true;
        }
    });
}
/// Remember release even when the service claimed/exited between UI iterations.
pub fn take_restore() -> bool {
    STATE.with(|s| core::mem::take(&mut s.restore_diagnostics))
}
pub fn pop() -> Option<[u8; 16]> {
    STATE.with(|s| {
        if s.len == 0 {
            return None;
        }
        let v = s.queue[s.head];
        s.head = (s.head + 1) % 64;
        s.len -= 1;
        Some(v)
    })
}
pub fn push(event: &InputEvent) {
    STATE.with(|s| {
        if s.pid == 0 {
            return;
        }
        // Overflow resets the stream: userspace cancels all capture before
        // consuming subsequent input, so dropped releases cannot stick a drag.
        if s.len == 64 {
            s.head = 0;
            s.len = 1;
            s.queue[0] = [0; 16];
            s.queue[0][0] = 4;
        }
        let mut bytes = [0; 16];
        match *event {
            InputEvent::Key(k) => {
                bytes[0] = 1;
                bytes[1] = u8::from(k.pressed);
                bytes[4..8].copy_from_slice(&u32::from(k.scancode).to_le_bytes());
                bytes[8..12].copy_from_slice(&(k.ch.map_or(0, |c| c as u32)).to_le_bytes());
            }
            InputEvent::PointerMoved { x, y } => {
                bytes[0] = 2;
                bytes[4..8].copy_from_slice(&x.to_le_bytes());
                bytes[8..12].copy_from_slice(&y.to_le_bytes());
            }
            InputEvent::PointerButton {
                button,
                pressed,
                x,
                y,
            } => {
                bytes[0] = 3;
                bytes[1] = u8::from(pressed);
                bytes[2] = match button {
                    input::event::MouseButton::Left => 0,
                    input::event::MouseButton::Right => 1,
                    input::event::MouseButton::Middle => 2,
                };
                bytes[4..8].copy_from_slice(&x.to_le_bytes());
                bytes[8..12].copy_from_slice(&y.to_le_bytes());
            }
        }
        s.queue[(s.head + s.len) % 64] = bytes;
        s.len += 1;
    });
}
