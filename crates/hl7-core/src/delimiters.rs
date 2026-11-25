//! HL7 v2 delimiter detection, validation, and escape-sequence codec (FO-9, FO-10, §5.5, EDIT-4).

use crate::error::DelimiterError;

/// The five HL7 v2 encoding characters declared in a header segment (`MSH`, `FHS`, `BHS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Delimiters {
    /// Field separator (4th byte of header, default `|`).
    pub field: u8,
    /// Component separator (5th byte of header, default `^`).
    pub component: u8,
    /// Repetition separator (6th byte of header, default `~`).
    pub repetition: u8,
    /// Escape character (7th byte of header, default `\`).
    pub escape: u8,
    /// Subcomponent separator (8th byte of header, default `&`).
    pub subcomponent: u8,
}

impl Default for Delimiters {
    fn default() -> Self {
        Self::STANDARD
    }
}

impl Delimiters {
    /// Standard HL7 v2 delimiters: `|^~\&`.
    pub const STANDARD: Self = Self {
        field: b'|',
        component: b'^',
        repetition: b'~',
        escape: b'\\',
        subcomponent: b'&',
    };

    /// Extract delimiters positionally from a header segment (`MSH`, `FHS`, `BHS`)
    /// without matching literal delimiter characters (FO-9, FO-10, D-3).
    ///
    /// Also tolerates short `MSH-2` encoding fields (< 4 chars before the next field separator,
    /// e.g. `MSH|^~|...`), filling any omitted positions with standard non-conflicting defaults.
    pub fn from_header_bytes(header: &[u8]) -> Option<Self> {
        if header.len() < 5 {
            return None;
        }
        let tag = &header[0..3];
        if tag != b"MSH" && tag != b"FHS" && tag != b"BHS" {
            return None;
        }
        let field = header[3];
        if !is_valid_delimiter_byte(field) {
            return None;
        }
        let rest = &header[4..];
        let msh2_end = rest
            .iter()
            .position(|&b| b == field)
            .unwrap_or(rest.len())
            .min(4);
        let msh2 = &rest[..msh2_end];
        // Require either a full 4-char MSH-2 or a short MSH-2 explicitly terminated by `field`
        // (rejecting truncated headers like `MSH|^~` with no closing separator).
        if msh2.is_empty() || (msh2_end < 4 && rest.get(msh2_end) != Some(&field)) {
            return None;
        }

        let defaults = *b"^~\\&";
        let mut bytes = [field, 0u8, 0u8, 0u8, 0u8];
        for idx in 0..4 {
            if let Some(&b) = msh2.get(idx) {
                bytes[idx + 1] = b;
            } else {
                // Pick the standard default for this slot if unused, otherwise first unused default/fallback.
                let preferred = defaults[idx];
                let chosen = if !bytes[..=idx].contains(&preferred) {
                    preferred
                } else {
                    defaults
                        .iter()
                        .copied()
                        .chain(*b"!@#$")
                        .find(|b| !bytes[..=idx].contains(b))?
                };
                bytes[idx + 1] = chosen;
            }
        }
        Self::from_five_bytes(bytes).ok()
    }

    /// Construct from five bytes in order: field, component, repetition, escape, subcomponent.
    ///
    /// Rejects non-printable/whitespace/control bytes and duplicate bytes (EDIT-9, D-3, D-11).
    pub fn from_five_bytes(bytes: [u8; 5]) -> Result<Self, DelimiterError> {
        for &b in &bytes {
            if !is_valid_delimiter_byte(b) {
                return Err(DelimiterError::InvalidByte(b));
            }
        }
        for i in 0..bytes.len() {
            for j in (i + 1)..bytes.len() {
                if bytes[i] == bytes[j] {
                    return Err(DelimiterError::DuplicateByte(bytes[i]));
                }
            }
        }
        Ok(Self {
            field: bytes[0],
            component: bytes[1],
            repetition: bytes[2],
            escape: bytes[3],
            subcomponent: bytes[4],
        })
    }

    /// Return the five delimiter bytes in canonical order `[field, component, repetition, escape, subcomponent]`.
    pub fn as_bytes(&self) -> [u8; 5] {
        [
            self.field,
            self.component,
            self.repetition,
            self.escape,
            self.subcomponent,
        ]
    }

    /// Returns `true` if `byte` is one of the five active delimiters.
    pub fn contains(&self, byte: u8) -> bool {
        byte == self.field
            || byte == self.component
            || byte == self.repetition
            || byte == self.escape
            || byte == self.subcomponent
    }

    /// Returns the first active delimiter byte present in `bytes`, if any (EDIT-4..EDIT-6).
    pub fn first_delimiter_in(&self, bytes: &[u8]) -> Option<u8> {
        bytes.iter().copied().find(|&b| self.contains(b))
    }

    /// Format the five delimiters as a 5-character string for display in prompts/messages.
    pub fn display_chars(&self) -> String {
        self.as_bytes().iter().map(|&b| char::from(b)).collect()
    }

    /// Encode active delimiter bytes into HL7 escape sequences (`\F\`, `\S\`, `\R\`, `\E\`, `\T\`) (EDIT-4).
    pub fn escape_bytes(&self, raw: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(raw.len());
        for &b in raw {
            let code = if b == self.field {
                Some(b'F')
            } else if b == self.component {
                Some(b'S')
            } else if b == self.repetition {
                Some(b'R')
            } else if b == self.escape {
                Some(b'E')
            } else if b == self.subcomponent {
                Some(b'T')
            } else {
                None
            };
            if let Some(c) = code {
                out.push(self.escape);
                out.push(c);
                out.push(self.escape);
            } else {
                out.push(b);
            }
        }
        out
    }

    /// Decode HL7 escape sequences (`\F\`, `\S\`, `\T\`, `\R\`, `\E\`, `\Xhh..\`, `\.br\`) for
    /// read-only display (§5.5). Never mutates stored bytes and never panics on malformed input (G-7).
    pub fn decode_escapes(&self, raw: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(raw.len());
        let mut i = 0;
        while i < raw.len() {
            if raw[i] == self.escape {
                if let Some(rel_end) = memchr::memchr(self.escape, &raw[i + 1..]) {
                    let seq = &raw[i + 1..i + 1 + rel_end];
                    if let Some(decoded) = self.decode_single_escape(seq) {
                        out.extend_from_slice(&decoded);
                        i += rel_end + 2;
                        continue;
                    }
                }
            }
            out.push(raw[i]);
            i += 1;
        }
        out
    }

    fn decode_single_escape(&self, seq: &[u8]) -> Option<Vec<u8>> {
        match seq {
            b"F" => Some(vec![self.field]),
            b"S" => Some(vec![self.component]),
            b"T" => Some(vec![self.subcomponent]),
            b"R" => Some(vec![self.repetition]),
            b"E" => Some(vec![self.escape]),
            b".br" => Some(vec![b'\n']),
            _ if seq.len() >= 3
                && (seq[0] == b'X' || seq[0] == b'x')
                && (seq.len() - 1).is_multiple_of(2) =>
            {
                let hex = &seq[1..];
                let mut bytes = Vec::with_capacity(hex.len() / 2);
                for pair in hex.as_chunks::<2>().0 {
                    let hi = hex_nibble(pair[0])?;
                    let lo = hex_nibble(pair[1])?;
                    bytes.push((hi << 4) | lo);
                }
                Some(bytes)
            }
            _ => None,
        }
    }
}

/// Valid delimiter byte: printable non-whitespace ASCII (`0x21..=0x7E`) (D-3, D-11).
pub(crate) fn is_valid_delimiter_byte(byte: u8) -> bool {
    (0x21..=0x7E).contains(&byte)
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'A'..=b'F' => Some(b - b'A' + 10),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // FO-9, FO-10, D-3
    #[test]
    fn positional_header_detection_standard_and_alternate() {
        let std = Delimiters::from_header_bytes(b"MSH|^~\\&|APP").unwrap();
        assert_eq!(std, Delimiters::STANDARD);

        let alt = Delimiters::from_header_bytes(b"MSH;:~\\&;APP").unwrap();
        assert_eq!(alt.field, b';');
        assert_eq!(alt.component, b':');
        assert_eq!(alt.repetition, b'~');
        assert_eq!(alt.escape, b'\\');
        assert_eq!(alt.subcomponent, b'&');

        // Pipe-less alternate header (FO-10)
        let pipeless = Delimiters::from_header_bytes(b"FHS#@!$%#APP").unwrap();
        assert_eq!(pipeless.as_bytes(), *b"#@!$%");

        // Prose must not match (D-3)
        assert!(Delimiters::from_header_bytes(b"MSH is not HL7").is_none());
        // Too short
        assert!(Delimiters::from_header_bytes(b"MSH|^~").is_none());
        // Non-header tag
        assert!(Delimiters::from_header_bytes(b"PID|^~\\&|1").is_none());
    }

    // D-11
    #[test]
    fn from_five_bytes_rejects_duplicates_and_control_chars() {
        assert!(Delimiters::from_five_bytes(*b"|^~\\&").is_ok());
        assert_eq!(
            Delimiters::from_five_bytes(*b"||~\\&"),
            Err(DelimiterError::DuplicateByte(b'|'))
        );
        assert_eq!(
            Delimiters::from_five_bytes(*b"|^~\r&"),
            Err(DelimiterError::InvalidByte(b'\r'))
        );
        assert_eq!(
            Delimiters::from_five_bytes(*b" ^~\\&"),
            Err(DelimiterError::InvalidByte(b' '))
        );
    }

    // EDIT-4, §5.5
    #[test]
    fn escape_and_decode_round_trip() {
        let d = Delimiters::STANDARD;
        let escaped = d.escape_bytes(b"A|B^C~D\\E&F");
        assert_eq!(escaped, b"A\\F\\B\\S\\C\\R\\D\\E\\E\\T\\F");
        assert_eq!(d.decode_escapes(&escaped), b"A|B^C~D\\E&F");

        // \Xhh..\ and \.br\ (§5.5)
        let decoded = d.decode_escapes(b"Line1\\.br\\Line2 \\X4142\\");
        assert_eq!(decoded, b"Line1\nLine2 AB");

        // Unterminated / unknown escapes preserved verbatim (G-7)
        assert_eq!(
            d.decode_escapes(b"Keep \\Z\\ and \\unterminated"),
            b"Keep \\Z\\ and \\unterminated"
        );
    }
}
