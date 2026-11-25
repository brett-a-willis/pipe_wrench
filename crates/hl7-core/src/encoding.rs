//! Character encoding resolution from `MSH-18` (G-2, D-22).

use std::borrow::Cow;

use encoding_rs::Encoding;

/// Resolved text encoding for displaying and editing a record (G-2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCharset {
    /// Active `encoding_rs` decoder/encoder (defaults to Windows-1252).
    pub encoding: &'static Encoding,
    /// Raw charset token declared in `MSH-18`, if present.
    pub declared: Option<String>,
    /// `true` when `MSH-18` named an unrecognized charset and fell back to Windows-1252.
    pub is_unknown_fallback: bool,
}

impl Default for ResolvedCharset {
    fn default() -> Self {
        Self {
            encoding: encoding_rs::WINDOWS_1252,
            declared: None,
            is_unknown_fallback: false,
        }
    }
}

impl ResolvedCharset {
    /// Resolve the charset from an optional `MSH-18` byte slice (G-2).
    ///
    /// Empty or absent `MSH-18` defaults to Windows-1252. Unknown values fall back to
    /// Windows-1252 with `is_unknown_fallback = true`.
    pub fn from_msh18(msh18: Option<&[u8]>, repetition_delim: u8) -> Self {
        let Some(raw) = msh18 else {
            return Self::default();
        };
        let first_rep = match memchr::memchr(repetition_delim, raw) {
            Some(pos) => &raw[..pos],
            None => raw,
        };
        let token = String::from_utf8_lossy(first_rep).trim().to_string();
        if token.is_empty() {
            return Self::default();
        }
        let upper = token.to_ascii_uppercase();
        let encoding = match upper.as_str() {
            "UNICODE UTF-8" | "UTF-8" | "UTF8" | "UNICODE" => Some(encoding_rs::UTF_8),
            "ASCII" | "8859/1" | "ISO-8859-1" | "ISO8859-1" | "WINDOWS-1252" | "CP1252" => {
                Some(encoding_rs::WINDOWS_1252)
            }
            "8859/2" | "ISO-8859-2" => Some(encoding_rs::ISO_8859_2),
            "8859/3" | "ISO-8859-3" => Some(encoding_rs::ISO_8859_3),
            "8859/4" | "ISO-8859-4" => Some(encoding_rs::ISO_8859_4),
            "8859/5" | "ISO-8859-5" => Some(encoding_rs::ISO_8859_5),
            "8859/6" | "ISO-8859-6" => Some(encoding_rs::ISO_8859_6),
            "8859/7" | "ISO-8859-7" => Some(encoding_rs::ISO_8859_7),
            "8859/8" | "ISO-8859-8" => Some(encoding_rs::ISO_8859_8),
            "8859/15" | "ISO-8859-15" => Some(encoding_rs::ISO_8859_15),
            _ => None,
        };

        match encoding {
            Some(enc) => Self {
                encoding: enc,
                declared: Some(token),
                is_unknown_fallback: false,
            },
            None => Self {
                encoding: encoding_rs::WINDOWS_1252,
                declared: Some(token),
                is_unknown_fallback: true,
            },
        }
    }

    /// Decode bytes for display using the resolved charset without modifying stored bytes.
    pub fn decode_display<'a>(&self, bytes: &'a [u8]) -> Cow<'a, str> {
        let (cow, _) = self.encoding.decode_without_bom_handling(bytes);
        cow
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // G-2
    #[test]
    fn resolves_default_utf8_and_unknown_fallback() {
        let def = ResolvedCharset::from_msh18(None, b'~');
        assert_eq!(def.encoding, encoding_rs::WINDOWS_1252);
        assert!(!def.is_unknown_fallback);

        let utf8 = ResolvedCharset::from_msh18(Some(b"UNICODE UTF-8"), b'~');
        assert_eq!(utf8.encoding, encoding_rs::UTF_8);
        assert!(!utf8.is_unknown_fallback);
        assert_eq!(
            utf8.decode_display("M\u{00fc}ller".as_bytes()),
            "M\u{00fc}ller"
        );

        let latin1 = ResolvedCharset::from_msh18(Some(b"8859/1"), b'~');
        assert_eq!(latin1.encoding, encoding_rs::WINDOWS_1252);
        assert!(!latin1.is_unknown_fallback);
        assert_eq!(latin1.decode_display(&[0xE9]), "\u{00e9}");

        let unknown = ResolvedCharset::from_msh18(Some(b"EXOTIC-999"), b'~');
        assert_eq!(unknown.encoding, encoding_rs::WINDOWS_1252);
        assert!(unknown.is_unknown_fallback);
        assert_eq!(unknown.declared.as_deref(), Some("EXOTIC-999"));
    }
}
