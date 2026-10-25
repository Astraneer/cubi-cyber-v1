//! Project-local compatibility API, not the upstream unicode-ident implementation.
//!
//! Classification uses the independently published MIT/Apache-2.0 unicode-xid
//! crate. Its Unicode 16 tables match the pinned Rust 1.90 toolchain. Upstream
//! unicode-ident 1.0.27 uses Unicode 18; this replacement intentionally does not
//! claim identical classification for identifiers introduced after Unicode 16.
#![no_std]
#![forbid(unsafe_code)]

use unicode_xid::UnicodeXID;

pub const UNICODE_VERSION: (u8, u8, u8) = (16, 0, 0);

#[inline]
pub fn is_xid_start(ch: char) -> bool {
    UnicodeXID::is_xid_start(ch)
}

#[inline]
pub fn is_xid_continue(ch: char) -> bool {
    UnicodeXID::is_xid_continue(ch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_truthful_and_ascii_matches_identifier_properties() {
        assert_eq!(UNICODE_VERSION, (16, 0, 0));
        assert_eq!(unicode_xid::UNICODE_VERSION, (16, 0, 0));
        for value in 0..=127 {
            let ch = char::from(value);
            assert_eq!(is_xid_start(ch), ch.is_ascii_alphabetic());
            assert_eq!(is_xid_continue(ch), ch.is_ascii_alphanumeric() || ch == '_');
        }
    }

    #[test]
    fn multilingual_names_and_combining_marks_are_classified() {
        for ch in ['Ж', 'я', 'λ', '漢', '\u{1c89}'] {
            assert!(is_xid_start(ch), "{ch}");
            assert!(is_xid_continue(ch), "{ch}");
        }
        for ch in ['\u{301}', '\u{200c}', '_', '9'] {
            assert!(!is_xid_start(ch), "{ch}");
            assert!(is_xid_continue(ch), "{ch}");
        }
        for ch in [' ', '-', '\0', '🦀'] {
            assert!(!is_xid_start(ch));
            assert!(!is_xid_continue(ch));
        }
    }
}
