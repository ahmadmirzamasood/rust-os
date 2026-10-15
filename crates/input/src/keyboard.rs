//! PS/2 keyboard decoding: scancode set 1.
//!
//! The i8042 controller translates the keyboard's native set 2 into set 1
//! when its translation bit is on (the default, and how the kernel programs
//! it). Set 1 encodes a press as a one-byte "make" code and a release as the
//! same code with bit 7 set; `0xE0` prefixes extended keys (arrows, right
//! Ctrl/Alt, ...) and `0xE1 1D 45 E1 9D C5` is the Pause key, which has no
//! release.
//!
//! Characters follow the US layout. Caps Lock and Num Lock are tracked as
//! toggles that start off; the keyboard LEDs are not driven.

/// Keys this decoder names. Anything else decodes as [`Key::Unknown`] with
/// the raw scancode in [`KeyEvent::scancode`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Key {
    Unknown,
    Escape,
    Digit1,
    Digit2,
    Digit3,
    Digit4,
    Digit5,
    Digit6,
    Digit7,
    Digit8,
    Digit9,
    Digit0,
    Minus,
    Equal,
    Backspace,
    Tab,
    Q,
    W,
    E,
    R,
    T,
    Y,
    U,
    I,
    O,
    P,
    BracketLeft,
    BracketRight,
    Enter,
    ControlLeft,
    A,
    S,
    D,
    F,
    G,
    H,
    J,
    K,
    L,
    Semicolon,
    Quote,
    Backquote,
    ShiftLeft,
    Backslash,
    Z,
    X,
    C,
    V,
    B,
    N,
    M,
    Comma,
    Period,
    Slash,
    ShiftRight,
    KeypadMultiply,
    AltLeft,
    Space,
    CapsLock,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    NumLock,
    ScrollLock,
    Keypad7,
    Keypad8,
    Keypad9,
    KeypadMinus,
    Keypad4,
    Keypad5,
    Keypad6,
    KeypadPlus,
    Keypad1,
    Keypad2,
    Keypad3,
    Keypad0,
    KeypadPeriod,
    IntlBackslash,
    F11,
    F12,
    KeypadEnter,
    ControlRight,
    KeypadDivide,
    AltRight,
    Home,
    ArrowUp,
    PageUp,
    ArrowLeft,
    ArrowRight,
    End,
    ArrowDown,
    PageDown,
    Insert,
    Delete,
    MetaLeft,
    MetaRight,
    ContextMenu,
    PrintScreen,
    Pause,
}

impl Key {
    /// Short display name.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Unknown => "Unknown",
            Self::Escape => "Esc",
            Self::Digit1 => "1",
            Self::Digit2 => "2",
            Self::Digit3 => "3",
            Self::Digit4 => "4",
            Self::Digit5 => "5",
            Self::Digit6 => "6",
            Self::Digit7 => "7",
            Self::Digit8 => "8",
            Self::Digit9 => "9",
            Self::Digit0 => "0",
            Self::Minus => "-",
            Self::Equal => "=",
            Self::Backspace => "Backspace",
            Self::Tab => "Tab",
            Self::Q => "Q",
            Self::W => "W",
            Self::E => "E",
            Self::R => "R",
            Self::T => "T",
            Self::Y => "Y",
            Self::U => "U",
            Self::I => "I",
            Self::O => "O",
            Self::P => "P",
            Self::BracketLeft => "[",
            Self::BracketRight => "]",
            Self::Enter => "Enter",
            Self::ControlLeft => "LCtrl",
            Self::A => "A",
            Self::S => "S",
            Self::D => "D",
            Self::F => "F",
            Self::G => "G",
            Self::H => "H",
            Self::J => "J",
            Self::K => "K",
            Self::L => "L",
            Self::Semicolon => ";",
            Self::Quote => "'",
            Self::Backquote => "`",
            Self::ShiftLeft => "LShift",
            Self::Backslash => "\\",
            Self::Z => "Z",
            Self::X => "X",
            Self::C => "C",
            Self::V => "V",
            Self::B => "B",
            Self::N => "N",
            Self::M => "M",
            Self::Comma => ",",
            Self::Period => ".",
            Self::Slash => "/",
            Self::ShiftRight => "RShift",
            Self::KeypadMultiply => "KP*",
            Self::AltLeft => "LAlt",
            Self::Space => "Space",
            Self::CapsLock => "CapsLock",
            Self::F1 => "F1",
            Self::F2 => "F2",
            Self::F3 => "F3",
            Self::F4 => "F4",
            Self::F5 => "F5",
            Self::F6 => "F6",
            Self::F7 => "F7",
            Self::F8 => "F8",
            Self::F9 => "F9",
            Self::F10 => "F10",
            Self::NumLock => "NumLock",
            Self::ScrollLock => "ScrollLock",
            Self::Keypad7 => "KP7",
            Self::Keypad8 => "KP8",
            Self::Keypad9 => "KP9",
            Self::KeypadMinus => "KP-",
            Self::Keypad4 => "KP4",
            Self::Keypad5 => "KP5",
            Self::Keypad6 => "KP6",
            Self::KeypadPlus => "KP+",
            Self::Keypad1 => "KP1",
            Self::Keypad2 => "KP2",
            Self::Keypad3 => "KP3",
            Self::Keypad0 => "KP0",
            Self::KeypadPeriod => "KP.",
            Self::IntlBackslash => "Intl\\",
            Self::F11 => "F11",
            Self::F12 => "F12",
            Self::KeypadEnter => "KPEnter",
            Self::ControlRight => "RCtrl",
            Self::KeypadDivide => "KP/",
            Self::AltRight => "RAlt",
            Self::Home => "Home",
            Self::ArrowUp => "Up",
            Self::PageUp => "PgUp",
            Self::ArrowLeft => "Left",
            Self::ArrowRight => "Right",
            Self::End => "End",
            Self::ArrowDown => "Down",
            Self::PageDown => "PgDn",
            Self::Insert => "Insert",
            Self::Delete => "Delete",
            Self::MetaLeft => "LMeta",
            Self::MetaRight => "RMeta",
            Self::ContextMenu => "Menu",
            Self::PrintScreen => "PrtSc",
            Self::Pause => "Pause",
        }
    }

    /// Key for a set-1 make code without prefix.
    fn plain(code: u8) -> Self {
        use Key::*;
        const TABLE: [Key; 0x59] = [
            Unknown,
            Escape,
            Digit1,
            Digit2,
            Digit3,
            Digit4,
            Digit5,
            Digit6, // 0x00
            Digit7,
            Digit8,
            Digit9,
            Digit0,
            Minus,
            Equal,
            Backspace,
            Tab, // 0x08
            Q,
            W,
            E,
            R,
            T,
            Y,
            U,
            I, // 0x10
            O,
            P,
            BracketLeft,
            BracketRight,
            Enter,
            ControlLeft,
            A,
            S, // 0x18
            D,
            F,
            G,
            H,
            J,
            K,
            L,
            Semicolon, // 0x20
            Quote,
            Backquote,
            ShiftLeft,
            Backslash,
            Z,
            X,
            C,
            V, // 0x28
            B,
            N,
            M,
            Comma,
            Period,
            Slash,
            ShiftRight,
            KeypadMultiply, // 0x30
            AltLeft,
            Space,
            CapsLock,
            F1,
            F2,
            F3,
            F4,
            F5, // 0x38
            F6,
            F7,
            F8,
            F9,
            F10,
            NumLock,
            ScrollLock,
            Keypad7, // 0x40
            Keypad8,
            Keypad9,
            KeypadMinus,
            Keypad4,
            Keypad5,
            Keypad6,
            KeypadPlus,
            Keypad1, // 0x48
            Keypad2,
            Keypad3,
            Keypad0,
            KeypadPeriod,
            Unknown,
            Unknown,
            IntlBackslash,
            F11, // 0x50
            F12, // 0x58
        ];
        TABLE.get(usize::from(code)).copied().unwrap_or(Unknown)
    }

    /// Key for a set-1 make code after an `0xE0` prefix.
    fn extended(code: u8) -> Self {
        match code {
            0x1C => Self::KeypadEnter,
            0x1D => Self::ControlRight,
            0x35 => Self::KeypadDivide,
            0x37 => Self::PrintScreen,
            0x38 => Self::AltRight,
            0x47 => Self::Home,
            0x48 => Self::ArrowUp,
            0x49 => Self::PageUp,
            0x4B => Self::ArrowLeft,
            0x4D => Self::ArrowRight,
            0x4F => Self::End,
            0x50 => Self::ArrowDown,
            0x51 => Self::PageDown,
            0x52 => Self::Insert,
            0x53 => Self::Delete,
            0x5B => Self::MetaLeft,
            0x5C => Self::MetaRight,
            0x5D => Self::ContextMenu,
            _ => Self::Unknown,
        }
    }

    /// US-layout character, if the key produces one.
    fn character(self, m: Modifiers) -> Option<char> {
        use Key::*;
        let letter = |c: char| {
            Some(if m.shift != m.caps_lock {
                c.to_ascii_uppercase()
            } else {
                c
            })
        };
        let pair = |plain: char, shifted: char| Some(if m.shift { shifted } else { plain });
        let keypad = |c: char| if m.num_lock { Some(c) } else { None };
        match self {
            A => letter('a'),
            B => letter('b'),
            C => letter('c'),
            D => letter('d'),
            E => letter('e'),
            F => letter('f'),
            G => letter('g'),
            H => letter('h'),
            I => letter('i'),
            J => letter('j'),
            K => letter('k'),
            L => letter('l'),
            M => letter('m'),
            N => letter('n'),
            O => letter('o'),
            P => letter('p'),
            Q => letter('q'),
            R => letter('r'),
            S => letter('s'),
            T => letter('t'),
            U => letter('u'),
            V => letter('v'),
            W => letter('w'),
            X => letter('x'),
            Y => letter('y'),
            Z => letter('z'),
            Digit1 => pair('1', '!'),
            Digit2 => pair('2', '@'),
            Digit3 => pair('3', '#'),
            Digit4 => pair('4', '$'),
            Digit5 => pair('5', '%'),
            Digit6 => pair('6', '^'),
            Digit7 => pair('7', '&'),
            Digit8 => pair('8', '*'),
            Digit9 => pair('9', '('),
            Digit0 => pair('0', ')'),
            Minus => pair('-', '_'),
            Equal => pair('=', '+'),
            BracketLeft => pair('[', '{'),
            BracketRight => pair(']', '}'),
            Semicolon => pair(';', ':'),
            Quote => pair('\'', '"'),
            Backquote => pair('`', '~'),
            Backslash | IntlBackslash => pair('\\', '|'),
            Comma => pair(',', '<'),
            Period => pair('.', '>'),
            Slash => pair('/', '?'),
            Space => Some(' '),
            Tab => Some('\t'),
            Enter | KeypadEnter => Some('\n'),
            Backspace => Some('\u{8}'),
            KeypadMultiply => Some('*'),
            KeypadMinus => Some('-'),
            KeypadPlus => Some('+'),
            KeypadDivide => Some('/'),
            Keypad0 => keypad('0'),
            Keypad1 => keypad('1'),
            Keypad2 => keypad('2'),
            Keypad3 => keypad('3'),
            Keypad4 => keypad('4'),
            Keypad5 => keypad('5'),
            Keypad6 => keypad('6'),
            Keypad7 => keypad('7'),
            Keypad8 => keypad('8'),
            Keypad9 => keypad('9'),
            KeypadPeriod => keypad('.'),
            _ => None,
        }
    }
}

/// Modifier state at the time of an event (after applying it).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub meta: bool,
    pub caps_lock: bool,
    pub num_lock: bool,
}

/// One key press or release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyEvent {
    pub key: Key,
    /// Raw make code; `0xE0xx` for extended keys, `0xE11D` for Pause.
    pub scancode: u16,
    pub pressed: bool,
    /// A press for a key that was already down (typematic repeat).
    pub repeat: bool,
    pub modifiers: Modifiers,
    /// Character produced by a press, if any (US layout).
    pub ch: Option<char>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Prefix {
    None,
    /// After `0xE0`.
    Extended,
    /// Inside the Pause sequence; counts the bytes still to swallow.
    Pause(u8),
}

/// Stateful set-1 decoder. Feed it bytes in order with [`Decoder::feed`].
#[derive(Clone, Debug)]
pub struct Decoder {
    prefix: Prefix,
    down: [u64; 2],
    shift: [bool; 2],
    ctrl: [bool; 2],
    alt: [bool; 2],
    meta: [bool; 2],
    caps_lock: bool,
    num_lock: bool,
    ignored: u32,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder {
    pub const fn new() -> Self {
        Self {
            prefix: Prefix::None,
            down: [0; 2],
            shift: [false; 2],
            ctrl: [false; 2],
            alt: [false; 2],
            meta: [false; 2],
            caps_lock: false,
            num_lock: false,
            ignored: 0,
        }
    }

    /// Forget any half-received sequence (call after bytes were lost).
    /// Key and modifier state is kept, so held keys release normally.
    pub fn resync(&mut self) {
        self.prefix = Prefix::None;
    }

    /// Bytes ignored so far (controller responses, error codes, unknown
    /// prefixed codes).
    pub const fn ignored(&self) -> u32 {
        self.ignored
    }

    pub fn modifiers(&self) -> Modifiers {
        Modifiers {
            shift: self.shift[0] || self.shift[1],
            ctrl: self.ctrl[0] || self.ctrl[1],
            alt: self.alt[0] || self.alt[1],
            meta: self.meta[0] || self.meta[1],
            caps_lock: self.caps_lock,
            num_lock: self.num_lock,
        }
    }

    /// Decode one byte. Returns an event once a complete code has arrived.
    pub fn feed(&mut self, byte: u8) -> Option<KeyEvent> {
        match self.prefix {
            Prefix::Pause(left) => {
                self.prefix = if left > 1 {
                    Prefix::Pause(left - 1)
                } else {
                    Prefix::None
                };
                if left == 1 {
                    return Some(KeyEvent {
                        key: Key::Pause,
                        scancode: 0xE11D,
                        pressed: true,
                        repeat: false,
                        modifiers: self.modifiers(),
                        ch: None,
                    });
                }
                return None;
            }
            Prefix::Extended => {
                self.prefix = Prefix::None;
                let code = byte & 0x7F;
                // 0xE0 2A / 0xE0 36 are "fake shifts" around PrintScreen and
                // some navigation keys; they carry no key of their own.
                if code == 0x2A || code == 0x36 {
                    self.ignored = self.ignored.wrapping_add(1);
                    return None;
                }
                let key = Key::extended(code);
                return Some(self.event(key, 0xE000 | u16::from(code), byte & 0x80 == 0));
            }
            Prefix::None => {}
        }
        match byte {
            0xE0 => {
                self.prefix = Prefix::Extended;
                None
            }
            0xE1 => {
                self.prefix = Prefix::Pause(5);
                None
            }
            // Error/overrun (0x00, 0xFF) and command responses: acknowledge
            // 0xFA, resend 0xFE, echo 0xEE. None is a set-1 key code in use.
            0x00 | 0xFF | 0xFA | 0xFE | 0xEE => {
                self.ignored = self.ignored.wrapping_add(1);
                None
            }
            _ => {
                let code = byte & 0x7F;
                Some(self.event(Key::plain(code), u16::from(code), byte & 0x80 == 0))
            }
        }
    }

    fn event(&mut self, key: Key, scancode: u16, pressed: bool) -> KeyEvent {
        let index = key as usize;
        let bit = 1u64 << (index % 64);
        let word = &mut self.down[index / 64];
        let was_down = *word & bit != 0;
        if key != Key::Unknown {
            if pressed {
                *word |= bit;
            } else {
                *word &= !bit;
            }
        }
        let repeat = pressed && was_down && key != Key::Unknown;
        match key {
            Key::ShiftLeft => self.shift[0] = pressed,
            Key::ShiftRight => self.shift[1] = pressed,
            Key::ControlLeft => self.ctrl[0] = pressed,
            Key::ControlRight => self.ctrl[1] = pressed,
            Key::AltLeft => self.alt[0] = pressed,
            Key::AltRight => self.alt[1] = pressed,
            Key::MetaLeft => self.meta[0] = pressed,
            Key::MetaRight => self.meta[1] = pressed,
            Key::CapsLock if pressed && !repeat => self.caps_lock = !self.caps_lock,
            Key::NumLock if pressed && !repeat => self.num_lock = !self.num_lock,
            _ => {}
        }
        let modifiers = self.modifiers();
        KeyEvent {
            key,
            scancode,
            pressed,
            repeat,
            modifiers,
            ch: if pressed {
                key.character(modifiers)
            } else {
                None
            },
        }
    }

    /// Whether `key` is currently held.
    pub fn is_down(&self, key: Key) -> bool {
        let index = key as usize;
        self.down[index / 64] & (1 << (index % 64)) != 0
    }
}

const _: () = assert!((Key::Pause as usize) < 128, "key bitset holds 128 keys");

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    fn decode(bytes: &[u8]) -> Vec<KeyEvent> {
        let mut d = Decoder::new();
        bytes.iter().filter_map(|&b| d.feed(b)).collect()
    }

    #[test]
    fn press_and_release_of_a_letter() {
        let events = decode(&[0x1E, 0x9E]);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].key, Key::A);
        assert!(events[0].pressed);
        assert_eq!(events[0].ch, Some('a'));
        assert_eq!(events[0].scancode, 0x1E);
        assert!(!events[1].pressed);
        assert_eq!(events[1].ch, None);
    }

    #[test]
    fn shift_and_caps_lock_change_case() {
        // LShift down, B, LShift up, CapsLock, B, Shift+1
        let events = decode(&[0x2A, 0x30, 0xB0, 0xAA, 0x3A, 0xBA, 0x30, 0xB0, 0x2A, 0x02]);
        let chars: Vec<char> = events.iter().filter_map(|e| e.ch).collect();
        assert_eq!(chars, ['B', 'B', '!']);
        assert!(events.last().unwrap().modifiers.caps_lock);
        assert!(events.last().unwrap().modifiers.shift);
    }

    #[test]
    fn extended_keys_and_fake_shifts() {
        let events = decode(&[0xE0, 0x48, 0xE0, 0xC8, 0xE0, 0x2A, 0xE0, 0x37, 0xE0, 0x1D]);
        let keys: Vec<(Key, bool)> = events.iter().map(|e| (e.key, e.pressed)).collect();
        assert_eq!(
            keys,
            [
                (Key::ArrowUp, true),
                (Key::ArrowUp, false),
                (Key::PrintScreen, true),
                (Key::ControlRight, true)
            ]
        );
        assert_eq!(events[0].scancode, 0xE048);
        assert!(events[3].modifiers.ctrl);
    }

    #[test]
    fn pause_sequence_is_one_press() {
        let events = decode(&[0xE1, 0x1D, 0x45, 0xE1, 0x9D, 0xC5, 0x1E]);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].key, Key::Pause);
        assert_eq!(events[1].key, Key::A);
    }

    #[test]
    fn repeats_are_flagged_and_responses_ignored() {
        let mut d = Decoder::new();
        assert!(!d.feed(0x1E).unwrap().repeat);
        assert!(d.feed(0xFA).is_none());
        assert!(d.feed(0x1E).unwrap().repeat);
        assert!(d.is_down(Key::A));
        assert!(!d.feed(0x9E).unwrap().pressed);
        assert!(!d.is_down(Key::A));
        assert_eq!(d.ignored(), 1);
    }

    #[test]
    fn resync_drops_a_dangling_prefix() {
        let mut d = Decoder::new();
        assert!(d.feed(0xE0).is_none());
        d.resync();
        assert_eq!(d.feed(0x48).unwrap().key, Key::Keypad8);
    }

    #[test]
    fn every_byte_sequence_is_handled_without_panicking() {
        let mut d = Decoder::new();
        for a in 0..=255u8 {
            for b in 0..=255u8 {
                let _ = d.feed(a);
                let _ = d.feed(b);
            }
        }
    }

    #[test]
    fn names_are_unique_enough() {
        assert_eq!(Key::plain(0x58), Key::F12);
        assert_eq!(Key::plain(0x7F), Key::Unknown);
        assert_eq!(Key::extended(0x53).name(), "Delete");
    }
}
