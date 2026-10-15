//! PS/2 mouse packet decoding (standard three-byte protocol).
//!
//! Byte 0: bit 0 left, bit 1 right, bit 2 middle, bit 3 always 1, bit 4 X
//! sign, bit 5 Y sign, bit 6 X overflow, bit 7 Y overflow. Bytes 1 and 2:
//! low eight bits of the X and Y movement (9-bit two's complement with the
//! sign bits). Y grows *upward* on the device; packets returned here use
//! screen orientation (Y grows downward).
//!
//! Synchronization: a byte without bit 3 cannot start a packet and is
//! discarded, and [`PacketDecoder::resync`] drops a partial packet after the
//! caller lost bytes. Packets with an overflow bit keep their buttons but
//! report no movement, because the movement value is meaningless.

/// Mouse buttons held during a packet.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Buttons {
    pub left: bool,
    pub right: bool,
    pub middle: bool,
}

/// One decoded packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Packet {
    pub dx: i16,
    /// Positive is down on screen.
    pub dy: i16,
    pub buttons: Buttons,
    /// The device reported an overflow; `dx`/`dy` are zero.
    pub overflow: bool,
}

#[derive(Clone, Debug, Default)]
pub struct PacketDecoder {
    bytes: [u8; 3],
    len: usize,
    discarded: u32,
}

impl PacketDecoder {
    pub const fn new() -> Self {
        Self {
            bytes: [0; 3],
            len: 0,
            discarded: 0,
        }
    }

    /// Drop any partial packet (call after bytes were lost).
    pub fn resync(&mut self) {
        self.discarded = self.discarded.wrapping_add(self.len as u32);
        self.len = 0;
    }

    /// Bytes thrown away to stay synchronized.
    pub const fn discarded(&self) -> u32 {
        self.discarded
    }

    pub fn feed(&mut self, byte: u8) -> Option<Packet> {
        if self.len == 0 && byte & 0x08 == 0 {
            self.discarded = self.discarded.wrapping_add(1);
            return None;
        }
        self.bytes[self.len] = byte;
        self.len += 1;
        if self.len < 3 {
            return None;
        }
        self.len = 0;
        let [flags, x, y] = self.bytes;
        let overflow = flags & 0xC0 != 0;
        let extend = |low: u8, negative: bool| i16::from(low) - if negative { 256 } else { 0 };
        let (dx, dy) = if overflow {
            (0, 0)
        } else {
            (extend(x, flags & 0x10 != 0), -extend(y, flags & 0x20 != 0))
        };
        Some(Packet {
            dx,
            dy,
            buttons: Buttons {
                left: flags & 0x01 != 0,
                right: flags & 0x02 != 0,
                middle: flags & 0x04 != 0,
            },
            overflow,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_motion_and_buttons() {
        let mut d = PacketDecoder::new();
        assert_eq!(d.feed(0x09), None);
        assert_eq!(d.feed(5), None);
        let p = d.feed(3).unwrap();
        assert_eq!((p.dx, p.dy), (5, -3), "device up is screen up");
        assert!(p.buttons.left && !p.buttons.right && !p.buttons.middle);
        // Negative X (sign bit 4), negative Y (bit 5) = screen down.
        let p = [0x38, 0xFB, 0xFE]
            .into_iter()
            .find_map(|b| d.feed(b))
            .unwrap();
        assert_eq!((p.dx, p.dy), (-5, 2));
        let p = [0x1E, 0x00, 0x00]
            .into_iter()
            .find_map(|b| d.feed(b))
            .unwrap();
        assert_eq!(p.dx, -256);
        assert!(p.buttons.right && p.buttons.middle);
    }

    #[test]
    fn overflow_keeps_buttons_but_drops_motion() {
        let mut d = PacketDecoder::new();
        let p = [0x49, 0x7F, 0x7F]
            .into_iter()
            .find_map(|b| d.feed(b))
            .unwrap();
        assert!(p.overflow && p.buttons.left);
        assert_eq!((p.dx, p.dy), (0, 0));
    }

    #[test]
    fn resynchronizes_on_bad_first_byte_and_after_loss() {
        let mut d = PacketDecoder::new();
        // Stray data bytes without bit 3 are skipped.
        assert_eq!(d.feed(0x05), None);
        assert_eq!(d.feed(0x00), None);
        assert_eq!(d.discarded(), 2);
        // A partial packet is dropped on resync.
        d.feed(0x08);
        d.feed(0x10);
        d.resync();
        assert_eq!(d.discarded(), 4);
        let p = [0x08, 0x01, 0x01]
            .into_iter()
            .find_map(|b| d.feed(b))
            .unwrap();
        assert_eq!((p.dx, p.dy), (1, -1));
    }

    #[test]
    fn arbitrary_bytes_never_panic() {
        let mut d = PacketDecoder::new();
        let mut x = 0x1234_5678u32;
        for _ in 0..100_000 {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            if let Some(p) = d.feed(x as u8) {
                assert!((-256..=255).contains(&p.dx));
                assert!((-255..=256).contains(&p.dy));
            }
        }
    }
}
