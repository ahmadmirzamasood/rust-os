//! Bounded application search for the guest shell's Applications menu.
//!
//! Catalog IDs are stable: the six built-in views come first, followed by
//! shutdown, restart and the unavailable sleep action. Search never launches
//! an entry; the shell retains capability checks and power confirmation.

pub const QUERY_CAPACITY: usize = 32;

#[derive(Clone, Copy, Debug)]
pub struct Entry {
    pub label: &'static str,
    pub description: &'static str,
    keywords: &'static str,
}

pub const ENTRIES: [Entry; 9] = [
    Entry {
        label: "Computer",
        description: "Mounted guest volumes",
        keywords: "disk storage devices",
    },
    Entry {
        label: "Files",
        description: "Browse files and folders",
        keywords: "documents directory explorer",
    },
    Entry {
        label: "Trash",
        description: "Restore or remove deleted files",
        keywords: "recycle bin",
    },
    Entry {
        label: "Settings",
        description: "Desktop appearance and motion",
        keywords: "preferences accent accessibility",
    },
    Entry {
        label: "About",
        description: "System version and build",
        keywords: "information rust os",
    },
    Entry {
        label: "Task Manager",
        description: "Applications and processes",
        keywords: "tasks monitor cpu memory",
    },
    Entry {
        label: "Shut Down",
        description: "Finish work and turn off",
        keywords: "",
    },
    Entry {
        label: "Restart",
        description: "Finish work and restart",
        keywords: "",
    },
    Entry {
        label: "Sleep",
        description: "Unavailable: resume support missing",
        keywords: "",
    },
];

/// Fixed storage avoids allocation and bounds per-keystroke search work.
#[derive(Debug, Default)]
pub struct Launcher {
    bytes: [u8; QUERY_CAPACITY],
    len: usize,
}

impl Launcher {
    pub fn query(&self) -> &str {
        // Only printable ASCII can enter the private buffer.
        core::str::from_utf8(&self.bytes[..self.len]).expect("launcher query is ASCII")
    }

    /// Append one printable ASCII byte, returning whether the query changed.
    /// Control bytes, non-ASCII input and excess input leave it unchanged.
    pub fn input_ascii(&mut self, byte: u8) -> bool {
        if !(b' '..=b'~').contains(&byte) || self.len == QUERY_CAPACITY {
            return false;
        }
        self.bytes[self.len] = byte;
        self.len += 1;
        true
    }

    pub fn backspace(&mut self) -> bool {
        if self.len == 0 {
            return false;
        }
        self.len -= 1;
        self.bytes[self.len] = 0;
        true
    }

    pub fn clear(&mut self) -> bool {
        let changed = self.len != 0;
        self.bytes.fill(0);
        self.len = 0;
        changed
    }

    pub fn result_count(&self) -> usize {
        ENTRIES
            .iter()
            .enumerate()
            .filter(|(id, entry)| self.matches(*id, entry))
            .count()
    }

    /// Translate a visible search row into its stable catalog ID.
    pub fn result(&self, index: usize) -> Option<usize> {
        ENTRIES
            .iter()
            .enumerate()
            .filter(|(id, entry)| self.matches(*id, entry))
            .nth(index)
            .map(|(id, _)| id)
    }

    fn matches(&self, id: usize, entry: &Entry) -> bool {
        self.query().split_ascii_whitespace().all(|token| {
            contains_ignore_ascii_case(entry.label, token)
                // Power actions must match their actual label. Their explanatory
                // text must not cause incidental searches such as "work" or
                // "support" to offer shutdown, restart or unavailable sleep.
                || (id < 6
                    && (contains_ignore_ascii_case(entry.description, token)
                        || contains_ignore_ascii_case(entry.keywords, token)))
        })
    }
}

fn contains_ignore_ascii_case(text: &str, token: &str) -> bool {
    // Tokens are nonempty because split_ascii_whitespace discards separators.
    !token.is_empty()
        && text
            .as_bytes()
            .windows(token.len())
            .any(|window| window.eq_ignore_ascii_case(token.as_bytes()))
}

/// Convert a pressed US set-1 scancode to search text. Case folding makes Shift
/// and Caps Lock unnecessary here; extended/navigation/function codes never
/// become characters. The runtime handles editing and navigation separately.
pub fn scancode_ascii(code: u32) -> Option<u8> {
    match code {
        0x02..=0x0a => Some(b'1' + (code - 0x02) as u8),
        0x0b => Some(b'0'),
        0x0c => Some(b'-'),
        0x10..=0x19 => Some(b"qwertyuiop"[(code - 0x10) as usize]),
        0x1e..=0x26 => Some(b"asdfghjkl"[(code - 0x1e) as usize]),
        0x2c..=0x32 => Some(b"zxcvbnm"[(code - 0x2c) as usize]),
        0x39 => Some(b' '),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn search(text: &str) -> Launcher {
        let mut launcher = Launcher::default();
        for byte in text.bytes() {
            assert!(launcher.input_ascii(byte));
        }
        launcher
    }

    #[test]
    fn blank_query_keeps_catalog_order_and_clear_restores_it() {
        let mut launcher = Launcher::default();
        assert_eq!(launcher.result_count(), ENTRIES.len());
        for id in 0..ENTRIES.len() {
            assert_eq!(launcher.result(id), Some(id));
        }
        assert_eq!(launcher.result(ENTRIES.len()), None);
        assert_eq!(search("   ").result_count(), ENTRIES.len());
        assert!(!launcher.clear());
        assert!(launcher.input_ascii(b'z'));
        assert!(launcher.clear());
        assert_eq!(launcher.query(), "");
        assert_eq!(launcher.result_count(), ENTRIES.len());
    }

    #[test]
    fn case_insensitive_tokens_match_labels_descriptions_and_aliases() {
        let launcher = search("  FiLeS  FoLdEr  ");
        assert_eq!(launcher.result_count(), 1);
        assert_eq!(launcher.result(0), Some(1));
        assert_eq!(search("CPU MONITOR").result(0), Some(5));
        assert_eq!(search("accent").result(0), Some(3));
        assert_eq!(search("version").result(0), Some(4));
        let launcher = search("files");
        assert_eq!(launcher.result_count(), 2);
        assert_eq!(launcher.result(0), Some(1));
        assert_eq!(launcher.result(1), Some(2));
        assert_eq!(search("missing-application").result_count(), 0);
        assert_eq!(search("missing-application").result(0), None);
        assert_eq!(search("files cpu").result_count(), 0);
    }

    #[test]
    fn power_search_uses_labels_and_preserves_catalog_ids() {
        for (query, id) in [("shut down", 6), ("RESTART", 7), ("sleep", 8)] {
            let launcher = search(query);
            assert_eq!(launcher.result_count(), 1);
            assert_eq!(launcher.result(0), Some(id));
        }
        for query in ["work", "turn off", "support", "resume"] {
            let launcher = search(query);
            for row in 0..launcher.result_count() {
                assert!(launcher.result(row).is_some_and(|id| id < 6));
            }
        }
    }

    #[test]
    fn query_is_bounded_ascii_and_backspace_recovers_from_no_results() {
        let mut launcher = Launcher::default();
        assert!(!launcher.backspace());
        for byte in [0, b'\n', 0x7f, 0x80, 0xff] {
            assert!(!launcher.input_ascii(byte));
        }
        for _ in 0..QUERY_CAPACITY {
            assert!(launcher.input_ascii(b'x'));
        }
        assert_eq!(launcher.query().len(), QUERY_CAPACITY);
        assert!(!launcher.input_ascii(b'y'));
        assert_eq!(launcher.result_count(), 0);
        for _ in 0..QUERY_CAPACITY {
            assert!(launcher.backspace());
        }
        assert_eq!(launcher.query(), "");
        assert!(!launcher.backspace());
        assert_eq!(launcher.result_count(), ENTRIES.len());
        let mut launcher = search("filesx");
        assert_eq!(launcher.result_count(), 0);
        assert!(launcher.backspace());
        assert_eq!(launcher.result(0), Some(1));
    }

    #[test]
    fn scancodes_cover_us_search_keys_without_navigation_or_release_codes() {
        for (codes, expected) in [
            ((0x02..=0x0a), &b"123456789"[..]),
            ((0x10..=0x19), &b"qwertyuiop"[..]),
            ((0x1e..=0x26), &b"asdfghjkl"[..]),
            ((0x2c..=0x32), &b"zxcvbnm"[..]),
        ] {
            for (code, byte) in codes.zip(expected.iter()) {
                assert_eq!(scancode_ascii(code), Some(*byte));
            }
        }
        assert_eq!(scancode_ascii(0x0b), Some(b'0'));
        assert_eq!(scancode_ascii(0x0c), Some(b'-'));
        assert_eq!(scancode_ascii(0x39), Some(b' '));
        for code in [0x01, 0x0e, 0x0f, 0x1c, 0x2a, 0x3b, 0xe048, 0x90, u32::MAX] {
            assert_eq!(scancode_ascii(code), None);
        }
    }
}
