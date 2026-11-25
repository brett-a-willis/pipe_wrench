//! HL7 v2 timestamp parsing (`YYYY[MM[DD[HH[MM[SS[.S+]]]]]][+/-ZZZZ]`) and typed sort key
//! comparisons (`SORT-1..SORT-4`).

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};
use time::{Date, Month, PrimitiveDateTime, Time, UtcOffset};

/// Comparable instant parsed from an HL7 v2 timestamp (`SORT-1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Hl7Timestamp {
    /// Nanoseconds since the Unix epoch in UTC.
    pub unix_nanos: i128,
}

impl Hl7Timestamp {
    /// Parse an HL7 v2 timestamp (`YYYY[MM[DD[HH[MM[SS[.S+]]]]]][+/-ZZZZ]`) (`SORT-1`).
    ///
    /// Returns `None` if the input is empty or malformed.
    pub fn parse(input: &str) -> Option<Self> {
        let s = input.trim();
        if s.len() < 4 {
            return None;
        }

        // Split off optional timezone offset (+ZZZZ or -ZZZZ).
        let (datetime_part, tz_offset) = split_timezone(s)?;

        // Split off optional fractional seconds (.S+).
        let (digits, nanos) = match datetime_part.split_once('.') {
            Some((d, frac)) => {
                if d.len() != 14 || frac.is_empty() || !frac.bytes().all(|b| b.is_ascii_digit()) {
                    return None;
                }
                let mut padded = [b'0'; 9];
                for (dst, src) in padded.iter_mut().zip(frac.bytes().take(9)) {
                    *dst = src;
                }
                let n: u32 = std::str::from_utf8(&padded).ok()?.parse().ok()?;
                (d, n)
            }
            None => (datetime_part, 0u32),
        };

        if !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        if !matches!(digits.len(), 4 | 6 | 8 | 10 | 12 | 14) {
            return None;
        }

        let year: i32 = digits[0..4].parse().ok()?;
        let month_num: u8 = if digits.len() >= 6 {
            digits[4..6].parse().ok()?
        } else {
            1
        };
        let day: u8 = if digits.len() >= 8 {
            digits[6..8].parse().ok()?
        } else {
            1
        };
        let hour: u8 = if digits.len() >= 10 {
            digits[8..10].parse().ok()?
        } else {
            0
        };
        let minute: u8 = if digits.len() >= 12 {
            digits[10..12].parse().ok()?
        } else {
            0
        };
        let second: u8 = if digits.len() >= 14 {
            digits[12..14].parse().ok()?
        } else {
            0
        };

        let month = Month::try_from(month_num).ok()?;
        let date = Date::from_calendar_date(year, month, day).ok()?;
        let time = Time::from_hms_nano(hour, minute, second, nanos).ok()?;
        let odt = PrimitiveDateTime::new(date, time).assume_offset(tz_offset);

        Some(Self {
            unix_nanos: odt.unix_timestamp_nanos(),
        })
    }

    /// Parse from raw byte slice.
    pub fn parse_bytes(bytes: &[u8]) -> Option<Self> {
        let s = std::str::from_utf8(bytes).ok()?;
        Self::parse(s)
    }
}

fn split_timezone(s: &str) -> Option<(&str, UtcOffset)> {
    if let Some(pos) = s.rfind(['+', '-']) {
        let (dt, tz) = s.split_at(pos);
        if tz.len() != 5 || !tz[1..].bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let sign: i8 = if tz.starts_with('-') { -1 } else { 1 };
        let hh: i8 = tz[1..3].parse().ok()?;
        let mm: i8 = tz[3..5].parse().ok()?;
        let offset = UtcOffset::from_hms(sign * hh, sign * mm, 0).ok()?;
        Some((dt, offset))
    } else {
        Some((s, UtcOffset::UTC))
    }
}

/// Column or sort key data type (`SORT-2`, `SORT-3`, `RPT-1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum SortKeyType {
    /// Lexicographic string comparison.
    #[default]
    String,
    /// Numeric floating-point comparison.
    Number,
    /// Chronological HL7 timestamp comparison.
    Date,
}

/// Sort direction (`SORT-1..SORT-4`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum SortDirection {
    /// Ascending order (smallest / earliest first).
    #[default]
    Ascending,
    /// Descending order (largest / latest first).
    Descending,
}

impl SortDirection {
    /// Toggle between `Ascending` and `Descending` (`SORT-3`).
    pub fn toggle(self) -> Self {
        match self {
            Self::Ascending => Self::Descending,
            Self::Descending => Self::Ascending,
        }
    }
}

/// Compare two string values according to `key_type` and `direction` (`SORT-1..SORT-4`).
///
/// Missing (empty) or unparsable values **always sort last** regardless of direction (`SORT-1`).
pub fn compare_typed_values(
    a: &str,
    b: &str,
    key_type: SortKeyType,
    direction: SortDirection,
) -> Ordering {
    let apply_dir = |ord: Ordering| match direction {
        SortDirection::Ascending => ord,
        SortDirection::Descending => ord.reverse(),
    };

    let a_trim = a.trim();
    let b_trim = b.trim();

    match key_type {
        SortKeyType::String => match (a_trim.is_empty(), b_trim.is_empty()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            (false, false) => apply_dir(a_trim.cmp(b_trim)),
        },
        SortKeyType::Number => {
            let na = (!a_trim.is_empty())
                .then(|| a_trim.parse::<f64>().ok())
                .flatten();
            let nb = (!b_trim.is_empty())
                .then(|| b_trim.parse::<f64>().ok())
                .flatten();
            match (na, nb) {
                (Some(va), Some(vb)) => apply_dir(va.total_cmp(&vb)),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            }
        }
        SortKeyType::Date => {
            let da = Hl7Timestamp::parse(a_trim);
            let db = Hl7Timestamp::parse(b_trim);
            match (da, db) {
                (Some(ta), Some(tb)) => apply_dir(ta.cmp(&tb)),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // SORT-1
    #[test]
    fn parses_mixed_precision_and_timezone_timestamps_and_sorts_missing_last() {
        let t_year = Hl7Timestamp::parse("2026").unwrap();
        let t_day = Hl7Timestamp::parse("20260115").unwrap();
        let t_sec = Hl7Timestamp::parse("20260115120000").unwrap();
        let t_frac = Hl7Timestamp::parse("20260115120000.125").unwrap();
        assert!(t_year < t_day && t_day < t_sec && t_sec < t_frac);

        // 12:00:00-0500 == 17:00:00+0000
        let t_est = Hl7Timestamp::parse("20260115120000-0500").unwrap();
        let t_utc = Hl7Timestamp::parse("20260115170000+0000").unwrap();
        assert_eq!(t_est, t_utc);

        // Invalid/missing sort last in both Ascending and Descending
        assert_eq!(
            compare_typed_values("20260115", "", SortKeyType::Date, SortDirection::Ascending),
            Ordering::Less
        );
        assert_eq!(
            compare_typed_values(
                "20260115",
                "NOT_A_DATE",
                SortKeyType::Date,
                SortDirection::Descending
            ),
            Ordering::Less
        );
    }
}
