//! HL7 v2 field address parsing and canonical formatting (§0, §5.2, §12.2, D-8, D-9).
//!
//! Canonical HL7 v2 path notation: `SEGMENT-FIELD(.COMPONENT)(.SUBCOMPONENT)`
//! with optional repetition `(R)` after the field number:
//! - `SEG-F` (e.g. `OBX-5`, `PID-3`)
//! - `SEG-F(R)` (e.g. `PID-3(1)`, `OBX-5(3)`)
//! - `SEG-F.C` (e.g. `MSH-9.1`, `PID-3.1`)
//! - `SEG-F(R).C` (e.g. `PID-3(2).1`, `PID-5(2).1`)
//! - `SEG-F.C.S` (e.g. `PID-3.4.1`)
//! - `SEG-F(R).C.S` (e.g. `PID-3(2).4.1`)
//!
//! If no repetition is specified, the first repetition is assumed when resolving components
//! or subcomponents (D-8). Legacy hyphen-separated component/subcomponent input (`PID-3-4-1`)
//! is also accepted by `FromStr` and normalized to canonical dot notation on `Display`.

use std::fmt;
use std::num::NonZeroU16;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::AddressError;

/// Parsed HL7 v2 field address (§5.2, D-8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FieldAddress {
    /// 3-byte ASCII segment tag (e.g. `*b"PID"`).
    pub segment: [u8; 3],
    /// 1-based field index.
    pub field: NonZeroU16,
    /// Optional 1-based repetition index (when `None`, first repetition is assumed when splitting).
    pub repetition: Option<NonZeroU16>,
    /// Optional 1-based component index.
    pub component: Option<NonZeroU16>,
    /// Optional 1-based subcomponent index (requires `component` to be `Some`).
    pub subcomponent: Option<NonZeroU16>,
}

impl FieldAddress {
    /// Construct and validate a [`FieldAddress`].
    pub fn new(
        segment: [u8; 3],
        field: NonZeroU16,
        repetition: Option<NonZeroU16>,
        component: Option<NonZeroU16>,
        subcomponent: Option<NonZeroU16>,
    ) -> Result<Self, AddressError> {
        if !is_valid_segment_tag(&segment) {
            return Err(AddressError::InvalidSegmentTag {
                tag: String::from_utf8_lossy(&segment).into_owned(),
            });
        }
        if subcomponent.is_some() && component.is_none() {
            return Err(AddressError::InvalidFormat {
                input: String::from_utf8_lossy(&segment).into_owned(),
                reason: "subcomponent requires a component index",
            });
        }
        Ok(Self {
            segment,
            field,
            repetition,
            component,
            subcomponent,
        })
    }

    /// Return the 3-character segment tag as a `&str`.
    pub fn segment_str(&self) -> &str {
        std::str::from_utf8(&self.segment).unwrap_or("???")
    }

    /// Effective 1-based repetition index (defaults to `1` when omitted per D-8).
    pub fn effective_repetition(&self) -> NonZeroU16 {
        self.repetition.unwrap_or(NonZeroU16::MIN)
    }

    /// Returns `true` for `MSH-1`, `MSH-2`, `FHS-1`, `FHS-2`, `BHS-1`, `BHS-2` (§5.2, EDIT-3, D-9).
    pub fn is_header_delimiter_field(&self) -> bool {
        (self.segment == *b"MSH" || self.segment == *b"FHS" || self.segment == *b"BHS")
            && self.field.get() <= 2
    }
}

impl FromStr for FieldAddress {
    type Err = AddressError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.trim();
        let Some((seg_part, rest)) = trimmed.split_once('-') else {
            return Err(AddressError::InvalidFormat {
                input: s.to_string(),
                reason: "expected SEGMENT-FIELD[(R)][.COMPONENT[.SUBCOMPONENT]]",
            });
        };

        let tag_bytes = seg_part.as_bytes();
        if tag_bytes.len() != 3 {
            return Err(AddressError::InvalidSegmentTag {
                tag: seg_part.to_string(),
            });
        }
        let segment = [tag_bytes[0], tag_bytes[1], tag_bytes[2]];
        if !is_valid_segment_tag(&segment) {
            return Err(AddressError::InvalidSegmentTag {
                tag: seg_part.to_string(),
            });
        }

        // Split `rest` on `.` (canonical) or `-` (legacy fallback), rejecting mixed or >3 parts.
        let subparts: Vec<&str> = if rest.contains('.') {
            if rest.contains('-') {
                return Err(AddressError::InvalidFormat {
                    input: s.to_string(),
                    reason: "cannot mix '.' and '-' component separators",
                });
            }
            rest.split('.').collect()
        } else {
            rest.split('-').collect()
        };

        if subparts.is_empty() || subparts.len() > 3 {
            return Err(AddressError::InvalidFormat {
                input: s.to_string(),
                reason: "expected SEGMENT-FIELD[(R)][.COMPONENT[.SUBCOMPONENT]]",
            });
        }

        let (field, repetition) = parse_field_and_rep(subparts[0], s)?;
        let component = match subparts.get(1) {
            Some(c) => Some(parse_nonzero(c, s)?),
            None => None,
        };
        let subcomponent = match subparts.get(2) {
            Some(sub) => Some(parse_nonzero(sub, s)?),
            None => None,
        };

        Self::new(segment, field, repetition, component, subcomponent)
    }
}

impl fmt::Display for FieldAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", self.segment_str(), self.field)?;
        if let Some(rep) = self.repetition {
            write!(f, "({rep})")?;
        }
        if let Some(comp) = self.component {
            write!(f, ".{comp}")?;
        }
        if let Some(sub) = self.subcomponent {
            write!(f, ".{sub}")?;
        }
        Ok(())
    }
}

impl Serialize for FieldAddress {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for FieldAddress {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse::<Self>().map_err(serde::de::Error::custom)
    }
}

fn is_valid_segment_tag(tag: &[u8; 3]) -> bool {
    tag[0].is_ascii_uppercase()
        && tag[1].is_ascii_alphanumeric()
        && tag[2].is_ascii_alphanumeric()
        && !tag[1].is_ascii_lowercase()
        && !tag[2].is_ascii_lowercase()
}

fn parse_field_and_rep(
    part: &str,
    original: &str,
) -> Result<(NonZeroU16, Option<NonZeroU16>), AddressError> {
    if let Some(open) = part.find('(') {
        if !part.ends_with(')') {
            return Err(AddressError::InvalidFormat {
                input: original.to_string(),
                reason: "unclosed repetition parenthesis",
            });
        }
        let field_str = &part[..open];
        let rep_str = &part[open + 1..part.len() - 1];
        let field = parse_nonzero(field_str, original)?;
        let rep = parse_nonzero(rep_str, original)?;
        Ok((field, Some(rep)))
    } else if let Some(open) = part.find('[') {
        if !part.ends_with(']') {
            return Err(AddressError::InvalidFormat {
                input: original.to_string(),
                reason: "unclosed repetition bracket",
            });
        }
        let field_str = &part[..open];
        let rep_str = &part[open + 1..part.len() - 1];
        let field = parse_nonzero(field_str, original)?;
        let rep = parse_nonzero(rep_str, original)?;
        Ok((field, Some(rep)))
    } else {
        Ok((parse_nonzero(part, original)?, None))
    }
}

/// Parse an HL7 filter field path that supports both concrete [`FieldAddress`] syntax
/// (`PID-3(1).1`, `PID-3[1].1`) and wildcard repetition syntax (`PID-3(*).1`, `PID-3[*].1`,
/// `OBX(*)-5`, `OBX-5(*)`).
///
/// Returns `(address, wildcard_repetition)` where `wildcard_repetition == true` indicates that
/// all `~`-separated repetitions of the addressed field should be evaluated.
pub fn parse_filter_field_path(input: &str) -> Result<(FieldAddress, bool), AddressError> {
    let trimmed = input.trim();
    let Some((raw_seg, rest)) = trimmed.split_once('-') else {
        return Err(AddressError::InvalidFormat {
            input: input.to_string(),
            reason: "expected SEGMENT-FIELD[(R|*)][.COMPONENT[.SUBCOMPONENT]]",
        });
    };

    // Accept optional segment wildcard `OBX(*)-5` / `OBX[*]-5` (filters already match all segments of that tag).
    let seg_part = raw_seg
        .strip_suffix("(*)")
        .or_else(|| raw_seg.strip_suffix("[*]"))
        .unwrap_or(raw_seg);

    // Detect field-level wildcard repetition `PID-3(*).1` / `PID-3[*].1` / `OBX-5(*)`.
    let (normalized_rest, wildcard_rep) = if let Some(replaced) = strip_wildcard_rep(rest) {
        (replaced, true)
    } else {
        (rest.to_string(), false)
    };

    let normalized = format!("{seg_part}-{normalized_rest}");
    let address: FieldAddress = normalized.parse()?;
    Ok((address, wildcard_rep))
}

fn strip_wildcard_rep(rest: &str) -> Option<String> {
    // Only replace `(*)` or `[*]` in the field portion (before the first `.` or `-`).
    let split_idx = rest.find(['.', '-']).unwrap_or(rest.len());
    let (field_part, tail) = rest.split_at(split_idx);
    let base = field_part
        .strip_suffix("(*)")
        .or_else(|| field_part.strip_suffix("[*]"))?;
    Some(format!("{base}{tail}"))
}

fn parse_nonzero(s: &str, original: &str) -> Result<NonZeroU16, AddressError> {
    s.parse::<NonZeroU16>()
        .map_err(|_| AddressError::InvalidIndex {
            input: original.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    // §0, §5.2, D-8
    #[test]
    fn parse_and_format_all_canonical_address_forms() {
        let canonical_cases = [
            "MSH-9.1",
            "PID-3",
            "PID-3.1",
            "PID-3.4.1",
            "OBX-5",
            "PID-3(1)",
            "PID-3(2).1",
            "OBX-5(3)",
            "PID-3(2).4.1",
            "PID-5(2)",
            "PID-5.1",
            "PID-5(2).1",
            "MSH-1",
            "MSH-2",
            "Z01-12",
        ];
        for case in canonical_cases {
            let parsed: FieldAddress = case.parse().unwrap();
            assert_eq!(parsed.to_string(), case);
        }

        // Verify PID-3(2).4.1 breakdown
        let full: FieldAddress = "PID-3(2).4.1".parse().unwrap();
        assert_eq!(full.segment_str(), "PID");
        assert_eq!(full.field.get(), 3);
        assert_eq!(full.repetition.map(NonZeroU16::get), Some(2));
        assert_eq!(full.component.map(NonZeroU16::get), Some(4));
        assert_eq!(full.subcomponent.map(NonZeroU16::get), Some(1));

        // Omitted repetition defaults to 1
        let no_rep: FieldAddress = "PID-3.4.1".parse().unwrap();
        assert_eq!(no_rep.repetition, None);
        assert_eq!(no_rep.effective_repetition().get(), 1);
    }

    // D-9
    #[test]
    fn identifies_header_delimiter_fields() {
        for addr in ["MSH-1", "MSH-2", "FHS-1", "FHS-2", "BHS-1", "BHS-2"] {
            assert!(addr
                .parse::<FieldAddress>()
                .unwrap()
                .is_header_delimiter_field());
        }
        for addr in ["MSH-3", "FHS-3", "BHS-3", "PID-1", "PID-2"] {
            assert!(!addr
                .parse::<FieldAddress>()
                .unwrap()
                .is_header_delimiter_field());
        }
    }

    // G-7
    #[test]
    fn rejects_malformed_addresses_without_panicking() {
        for bad in [
            "",
            "PID",
            "PI-1",
            "PIDX-1",
            "pid-1",
            "PID-0",
            "PID-5(0)",
            "PID-5(2",
            "PID-5.0",
            "PID-5.1.0",
            "PID-5.1.2.3",
            "PID-5.1-2",
        ] {
            assert!(
                bad.parse::<FieldAddress>().is_err(),
                "expected error for {bad}"
            );
        }
    }
}
