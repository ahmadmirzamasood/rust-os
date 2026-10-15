//! Main-loop side of input: drains the PS/2 byte rings, decodes them and
//! queues [`InputEvent`]s. Never runs in interrupt context.

use input::event::{EventQueue, InputEvent};
use input::keyboard::Decoder;
use input::mouse::PacketDecoder;
use input::pointer::Pointer;

use crate::drivers::ps2::{KEYBOARD_BYTES, MOUSE_BYTES};

/// Decoded events waiting for the UI.
pub const EVENT_CAPACITY: usize = 64;
/// Bytes taken from each ring per [`Pipeline::poll`], so one burst cannot
/// starve rendering. Rings are bounded anyway; anything left waits for the
/// next iteration.
const BYTES_PER_POLL: usize = 256;

#[derive(Debug)]
pub struct Pipeline {
    keyboard: Decoder,
    mouse: PacketDecoder,
    pointer: Pointer,
    queue: EventQueue<EVENT_CAPACITY>,
    keyboard_dropped_seen: u32,
    mouse_dropped_seen: u32,
    resyncs: u32,
}

impl Pipeline {
    pub fn new(screen_width: u32, screen_height: u32) -> Self {
        Self {
            keyboard: Decoder::new(),
            mouse: PacketDecoder::new(),
            pointer: Pointer::new(screen_width, screen_height),
            queue: EventQueue::new(),
            keyboard_dropped_seen: KEYBOARD_BYTES.dropped(),
            mouse_dropped_seen: MOUSE_BYTES.dropped(),
            resyncs: 0,
        }
    }

    /// Decode what the interrupt handlers queued so far.
    pub fn poll(&mut self) {
        let dropped = KEYBOARD_BYTES.dropped();
        if dropped != self.keyboard_dropped_seen {
            self.keyboard_dropped_seen = dropped;
            self.keyboard.resync();
            self.resyncs = self.resyncs.wrapping_add(1);
        }
        for _ in 0..BYTES_PER_POLL {
            let Some(byte) = KEYBOARD_BYTES.pop() else {
                break;
            };
            if let Some(event) = self.keyboard.feed(byte) {
                self.queue.push(InputEvent::Key(event));
            }
        }

        let dropped = MOUSE_BYTES.dropped();
        if dropped != self.mouse_dropped_seen {
            self.mouse_dropped_seen = dropped;
            self.mouse.resync();
            self.resyncs = self.resyncs.wrapping_add(1);
        }
        for _ in 0..BYTES_PER_POLL {
            let Some(byte) = MOUSE_BYTES.pop() else {
                break;
            };
            if let Some(packet) = self.mouse.feed(byte) {
                self.pointer.apply(packet, &mut self.queue);
            }
        }
    }

    pub fn next_event(&mut self) -> Option<InputEvent> {
        self.queue.pop()
    }

    /// Whether bytes or events are waiting.
    pub fn has_pending(&self) -> bool {
        !self.queue.is_empty() || !KEYBOARD_BYTES.is_empty() || !MOUSE_BYTES.is_empty()
    }

    pub fn pointer(&self) -> &Pointer {
        &self.pointer
    }

    pub fn stats(&self) -> Stats {
        Stats {
            keyboard_pending: KEYBOARD_BYTES.len(),
            keyboard_capacity: KEYBOARD_BYTES.capacity(),
            keyboard_dropped: KEYBOARD_BYTES.dropped(),
            mouse_pending: MOUSE_BYTES.len(),
            mouse_capacity: MOUSE_BYTES.capacity(),
            mouse_dropped: MOUSE_BYTES.dropped(),
            events_dropped: self.queue.dropped(),
            motion_coalesced: self.queue.coalesced(),
            resyncs: self.resyncs,
            keyboard_ignored: self.keyboard.ignored(),
            mouse_discarded: self.mouse.discarded(),
        }
    }
}

/// Queue and decoder counters, for the debug overlay and the serial log.
#[derive(Debug, Clone, Copy)]
pub struct Stats {
    pub keyboard_pending: usize,
    pub keyboard_capacity: usize,
    pub keyboard_dropped: u32,
    pub mouse_pending: usize,
    pub mouse_capacity: usize,
    pub mouse_dropped: u32,
    pub events_dropped: u32,
    pub motion_coalesced: u32,
    pub resyncs: u32,
    pub keyboard_ignored: u32,
    pub mouse_discarded: u32,
}
