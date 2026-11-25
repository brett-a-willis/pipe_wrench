//! HL7 v2 ACK generation, ACK-from-file framing, and ACK/NAK evaluation
//! (NET-15, NET-16, NET-26, D-15..D-17).

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::delimiters::Delimiters;
use crate::scanner::MllpEnvelope;

static ACK_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Standard HL7 v2 Table 0008 ACK codes for `MSA-1` (Original Mode `AA`/`AE`/`AR` and
/// Enhanced Mode `CA`/`CE`/`CR` used by Epic Bridges and modern interface engines) (NET-16).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AckCode {
    /// Original Mode: Application Accept (`AA`).
    #[default]
    AA,
    /// Original Mode: Application Error (`AE`).
    AE,
    /// Original Mode: Application Reject (`AR`).
    AR,
    /// Enhanced Mode: Commit Accept (`CA`).
    CA,
    /// Enhanced Mode: Commit Error (`CE`).
    CE,
    /// Enhanced Mode: Commit Reject (`CR`).
    CR,
}

impl AckCode {
    /// Return the 2-character code as a byte slice.
    pub fn as_bytes(&self) -> &'static [u8] {
        match self {
            Self::AA => b"AA",
            Self::AE => b"AE",
            Self::AR => b"AR",
            Self::CA => b"CA",
            Self::CE => b"CE",
            Self::CR => b"CR",
        }
    }

    /// Return `true` if this code is a positive acknowledgment (`AA` or `CA`).
    pub fn is_accept(&self) -> bool {
        matches!(self, Self::AA | Self::CA)
    }

    /// Parse from ASCII byte representation (case-insensitive, trimmed).
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let trimmed = bytes.trim_ascii();
        if trimmed.eq_ignore_ascii_case(b"AA") {
            Some(Self::AA)
        } else if trimmed.eq_ignore_ascii_case(b"AE") {
            Some(Self::AE)
        } else if trimmed.eq_ignore_ascii_case(b"AR") {
            Some(Self::AR)
        } else if trimmed.eq_ignore_ascii_case(b"CA") {
            Some(Self::CA)
        } else if trimmed.eq_ignore_ascii_case(b"CE") {
            Some(Self::CE)
        } else if trimmed.eq_ignore_ascii_case(b"CR") {
            Some(Self::CR)
        } else {
            None
        }
    }
}

/// Result of evaluating an incoming ACK against a sent message (NET-26).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AckResult {
    /// Inbound message is a valid ACK.
    Ack,
    /// Inbound message is a NAK (error, reject, mismatched ID, or unparsable).
    Nak,
}

impl AckResult {
    /// Returns `true` if this is an ACK.
    pub fn is_ack(&self) -> bool {
        matches!(self, Self::Ack)
    }
}

/// Mode used to determine if a response is an ACK or NAK (NET-26).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum AckMode {
    /// Simple raw frame or envelope-stripped byte matching (NET-26, D-17).
    /// Default match bytes: `0b 06 1c 0d` (matches `0x06` when envelope-stripped).
    Simple {
        /// Bytes to compare against (either raw frame or envelope-stripped payload).
        expected_bytes: Vec<u8>,
    },
    /// Standard HL7 v2 parsing mode (NET-26).
    #[default]
    Hl7,
    /// Legacy substring match on a configured token (NET-26).
    LegacySubstring {
        /// Substring token that must appear in the response payload.
        token: String,
    },
}

/// Format the current time as HL7 timestamp `YYYYMMDDHHMMSS` (or fall back to seconds).
pub fn current_hl7_timestamp() -> String {
    let now = OffsetDateTime::now_utc();
    format!(
        "{:04}{:02}{:02}{:02}{:02}{:02}",
        now.year(),
        now.month() as u8,
        now.day(),
        now.hour(),
        now.minute(),
        now.second()
    )
}

/// Generate a unique Control ID for outgoing ACKs (NET-16) or MSH-10 rewrites (NET-31).
pub fn generate_unique_control_id() -> String {
    let count = ACK_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("ACK{:010}_{}", count, Uuid::new_v4().simple())
}

/// Build a generated HL7 ACK record from an inbound message (NET-16, D-16).
///
/// Rules:
/// - Delimiters preserved from inbound header (or standard delimiters if non-standard/missing).
/// - MSH sending app/facility (`MSH-3/4`) swapped with receiving app/facility (`MSH-5/6`).
/// - MSH-7 timestamp set to current time.
/// - MSH-9 message type is `ACK^<trigger>` (e.g. `ACK^A01`) or bare `ACK` if trigger is absent.
/// - MSH-10 is a fresh unique control ID.
/// - MSH-11 (processing ID) and MSH-12 (version ID) copied from inbound MSH.
/// - MSA segment: `MSA|<code: AA/AE/AR>|<inbound MSH-10>`.
/// - Never panics on malformed input (G-7, D-16).
pub fn build_hl7_ack(
    inbound_bytes: &[u8],
    code: AckCode,
    custom_control_id: Option<&str>,
) -> Vec<u8> {
    let delims = Delimiters::from_header_bytes(inbound_bytes).unwrap_or(Delimiters::STANDARD);
    let fs = delims.field;
    let cs = delims.component;

    // Best-effort extraction of MSH fields from the first segment.
    let first_seg = match inbound_bytes.split(|&b| b == b'\r').next() {
        Some(s) => s,
        None => inbound_bytes,
    };

    // Inbound MSH format: MSH|^~\&|FIELD3|FIELD4|FIELD5...
    // first_seg[0..3] == "MSH"
    // first_seg[3] == fs (MSH-1)
    // first_seg[4..8] == delims[1..5] (MSH-2)
    // first_seg[8] == fs (separator before MSH-3)
    // first_seg[9..] contains MSH-3|MSH-4|MSH-5...
    let fields: Vec<&[u8]> =
        if first_seg.len() >= 9 && &first_seg[0..3] == b"MSH" && first_seg[8] == fs {
            first_seg[9..].split(|&b| b == fs).collect()
        } else {
            Vec::new()
        };

    let get_field = |idx: usize| -> &[u8] {
        // In fields:
        // index 0 is MSH-3
        // index 1 is MSH-4
        // index 2 is MSH-5
        // index (N - 3) is MSH-N
        if idx >= 3 && idx - 3 < fields.len() {
            fields[idx - 3]
        } else {
            b""
        }
    };

    let inbound_msh3 = get_field(3);
    let inbound_msh4 = get_field(4);
    let inbound_msh5 = get_field(5);
    let inbound_msh6 = get_field(6);
    let inbound_msh9 = get_field(9);
    let inbound_msh10 = get_field(10);
    let inbound_msh11 = get_field(11);
    let inbound_msh12 = get_field(12);

    // Swap sending and receiving application/facility.
    let ack_msh3 = inbound_msh5;
    let ack_msh4 = inbound_msh6;
    let ack_msh5 = inbound_msh3;
    let ack_msh6 = inbound_msh4;

    // Build MSH-9: ACK^<trigger> or bare ACK.
    let mut ack_msh9 = Vec::from(b"ACK".as_slice());
    if let Some(trigger) = inbound_msh9.split(|&b| b == cs).nth(1) {
        if !trigger.is_empty() {
            ack_msh9.push(cs);
            ack_msh9.extend_from_slice(trigger);
        }
    }

    let timestamp = current_hl7_timestamp();
    let control_id = custom_control_id
        .map(|s| s.to_string())
        .unwrap_or_else(generate_unique_control_id);

    let msh11 = if inbound_msh11.is_empty() {
        b"P".as_slice()
    } else {
        inbound_msh11
    };
    let msh12 = if inbound_msh12.is_empty() {
        b"2.5".as_slice()
    } else {
        inbound_msh12
    };

    let mut out = Vec::with_capacity(256);
    // MSH
    out.extend_from_slice(b"MSH");
    out.push(fs);
    out.extend_from_slice(&delims.as_bytes()[1..]); // component, rep, escape, subcomp
    out.push(fs);
    out.extend_from_slice(ack_msh3);
    out.push(fs);
    out.extend_from_slice(ack_msh4);
    out.push(fs);
    out.extend_from_slice(ack_msh5);
    out.push(fs);
    out.extend_from_slice(ack_msh6);
    out.push(fs);
    out.extend_from_slice(timestamp.as_bytes());
    out.push(fs);
    // MSH-8 empty
    out.push(fs);
    out.extend_from_slice(&ack_msh9);
    out.push(fs);
    out.extend_from_slice(control_id.as_bytes());
    out.push(fs);
    out.extend_from_slice(msh11);
    out.push(fs);
    out.extend_from_slice(msh12);
    out.push(b'\r');

    // MSA
    out.extend_from_slice(b"MSA");
    out.push(fs);
    out.extend_from_slice(code.as_bytes());
    out.push(fs);
    out.extend_from_slice(inbound_msh10);

    out
}

/// Frame an ACK payload from file (or null ACK on missing file) per NET-15 & D-15.
///
/// Rules:
/// - If `file_contents` is `None` (file missing), send `SB + 0x00 + ED + EB` (null ACK, D-15).
/// - If `file_contents` is `Some(bytes)`:
///   - If already starts with `SB` and ends with `ED + EB`, return raw bytes.
///   - Otherwise, strip any stray `SB`/`ED`/`EB`, ensure trailing `CR`, wrap as `SB + payload + ED + EB` (NET-2).
pub fn frame_ack_from_file(file_contents: Option<&[u8]>, envelope: MllpEnvelope) -> Vec<u8> {
    let Some(raw) = file_contents else {
        // Null ACK: SB + 0x00 + ED + EB (NET-15, D-15)
        return vec![
            envelope.start_block,
            0x00,
            envelope.end_data,
            envelope.end_block,
        ];
    };

    let sb = envelope.start_block;
    let ed = envelope.end_data;
    let eb = envelope.end_block;

    // Check if already framed properly with no stray LF bytes (§12.5, D-15)
    if raw.len() >= 3
        && raw.first() == Some(&sb)
        && raw[raw.len() - 2..] == [ed, eb]
        && !raw[1..raw.len() - 2].contains(&b'\n')
    {
        return raw.to_vec();
    }

    // Normalize `\r\n` / `\n` to strict `\r` and frame as `<SB><payload><CR><ED><EB>` (NET-2).
    envelope.frame_outbound(raw)
}

/// Inspect an HL7 v2 message's `MSH-15` (Accept Acknowledgment Type) and `MSH-16`
/// (Application Acknowledgment Type) using the dynamic field separator at byte index 3 (`MSH-1`).
///
/// Returns `true` when Enhanced Mode indicates no acknowledgment is requested:
/// `MSH-15=NE` and `MSH-16=NE`, or `MSH-15` empty/`NE` and `MSH-16=NE`.
pub fn msh_suppresses_ack(raw: &[u8]) -> bool {
    let first_seg = raw
        .split(|&b| b == b'\r' || b == b'\n')
        .next()
        .unwrap_or(raw);
    if first_seg.len() < 9 || &first_seg[0..3] != b"MSH" {
        return false;
    }
    let fs = first_seg[3];
    if first_seg[8] != fs {
        return false;
    }
    // `first_seg[9..].split(fs)` starts at MSH-3 (index 0):
    // MSH-15 is index 12, MSH-16 is index 13.
    let fields: Vec<&[u8]> = first_seg[9..].split(|&b| b == fs).collect();
    let msh15 = fields.get(12).map_or(&[][..], |b| b.trim_ascii());
    let msh16 = fields.get(13).map_or(&[][..], |b| b.trim_ascii());
    msh16.eq_ignore_ascii_case(b"NE") && (msh15.is_empty() || msh15.eq_ignore_ascii_case(b"NE"))
}

/// Extract `MSA-2` (Message Control ID) from an HL7 v2 ACK/NAK response payload using
/// the response's dynamic `MSH-1` field separator.
pub fn extract_msa2_control_id(response_payload: &[u8]) -> Option<String> {
    let delims = Delimiters::from_header_bytes(response_payload)?;
    let fs = delims.field;
    for seg in response_payload.split(|&b| b == b'\r' || b == b'\n') {
        if seg.len() >= 4 && &seg[0..3] == b"MSA" && seg[3] == fs {
            let fields: Vec<&[u8]> = seg[4..].split(|&b| b == fs).collect();
            let msa2 = fields.get(1)?.trim_ascii();
            return Some(String::from_utf8_lossy(msa2).into_owned());
        }
    }
    None
}

/// Determine whether a received response payload is an ACK or NAK (NET-26, D-17).
///
/// `response_payload`: The envelope-stripped bytes received over MLLP.
/// `sent_control_id`: The MSH-10 value of the message that was sent.
/// `mode`: The active evaluation mode (`Hl7`, `Simple`, `LegacySubstring`).
pub fn evaluate_ack(response_payload: &[u8], sent_control_id: &str, mode: &AckMode) -> AckResult {
    match mode {
        AckMode::Simple { expected_bytes } => {
            // Strip envelope bytes if expected_bytes contains them (D-17)
            let mut pattern = expected_bytes.as_slice();
            if pattern.starts_with(&[0x0B]) {
                pattern = &pattern[1..];
            }
            if pattern.ends_with(&[0x1C, 0x0D]) {
                pattern = &pattern[..pattern.len() - 2];
            }
            if response_payload == pattern {
                AckResult::Ack
            } else {
                AckResult::Nak
            }
        }
        AckMode::LegacySubstring { token } => {
            if token.is_empty() {
                return AckResult::Nak;
            }
            let text = String::from_utf8_lossy(response_payload);
            if text.contains(token) {
                AckResult::Ack
            } else {
                AckResult::Nak
            }
        }
        AckMode::Hl7 => {
            // Parse response into segments
            let delims = match Delimiters::from_header_bytes(response_payload) {
                Some(d) => d,
                None => return AckResult::Nak,
            };
            let fs = delims.field;

            // Find MSA segment (tolerating CR, LF, or CRLF segment breaks)
            for seg in response_payload.split(|&b| b == b'\r' || b == b'\n') {
                if seg.len() >= 4 && &seg[0..3] == b"MSA" && seg[3] == fs {
                    let fields: Vec<&[u8]> = seg[4..].split(|&b| b == fs).collect();
                    if fields.len() < 2 {
                        return AckResult::Nak;
                    }
                    let msa1 = fields[0].trim_ascii();
                    let msa2 = fields[1].trim_ascii();

                    // Verify MSA-2 equals sent MSH-10 (NET-26)
                    if msa2 != sent_control_id.trim().as_bytes() {
                        return AckResult::Nak;
                    }

                    // Check MSA-1 code (Table 0008): AA or CA -> ACK; AE, AR, CE, CR -> NAK
                    return match AckCode::from_bytes(msa1) {
                        Some(code) if code.is_accept() => AckResult::Ack,
                        _ => AckResult::Nak,
                    };
                }
            }
            AckResult::Nak
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // NET-16, D-16
    #[test]
    fn build_hl7_ack_swaps_apps_and_copies_control_id() {
        let inbound = b"MSH|^~\\&|SEND_APP|SEND_FAC|RECV_APP|RECV_FAC|20260101120000||ADT^A01|MSG999|P|2.5\rPID|1||P01\r";
        let ack_bytes = build_hl7_ack(inbound, AckCode::AA, Some("ACK_CTRL_1"));

        let ack_str = String::from_utf8_lossy(&ack_bytes);
        assert!(ack_str.starts_with("MSH|^~\\&|RECV_APP|RECV_FAC|SEND_APP|SEND_FAC|"));
        assert!(ack_str.contains("|ACK^A01|ACK_CTRL_1|P|2.5\r"));
        assert!(ack_str.ends_with("MSA|AA|MSG999"));

        // When trigger event absent
        let inbound_bare = b"MSH|^~\\&|S|F|R|F|20260101||ADT|MSG888|P|2.5\r";
        let ack_bare = build_hl7_ack(inbound_bare, AckCode::AE, Some("ACK_CTRL_2"));
        let bare_str = String::from_utf8_lossy(&ack_bare);
        assert!(bare_str.contains("|ACK|ACK_CTRL_2|"));
        assert!(bare_str.ends_with("MSA|AE|MSG888"));
    }

    // NET-15, D-15
    #[test]
    fn frame_ack_from_file_and_null_ack() {
        let env = MllpEnvelope::DEFAULT;

        // Missing file -> Null ACK
        let null_ack = frame_ack_from_file(None, env);
        assert_eq!(null_ack, vec![0x0B, 0x00, 0x1C, 0x0D]);

        // Unframed content -> framed per NET-2
        let unframed = b"MSH|^~\\&|A\r\nMSA|AA|123";
        let framed = frame_ack_from_file(Some(unframed), env);
        assert_eq!(framed[0], 0x0B);
        assert_eq!(framed[framed.len() - 2..], [0x1C, 0x0D]);
        // \r\n has \r kept, \n stripped or kept as is, ends with \r
        assert!(framed.starts_with(&[0x0B, b'M', b'S', b'H']));
        assert!(framed.ends_with(&[b'\r', 0x1C, 0x0D]));

        // Already framed content -> preserved verbatim
        let already = vec![0x0B, b'A', b'C', b'K', 0x1C, 0x0D];
        let res = frame_ack_from_file(Some(&already), env);
        assert_eq!(res, already);
    }

    // NET-26, D-17
    #[test]
    fn evaluate_ack_modes() {
        // 1. Simple ACK mode with default 0b 06 1c 0d matching payload 0x06
        let simple_mode = AckMode::Simple {
            expected_bytes: vec![0x0B, 0x06, 0x1C, 0x0D],
        };
        assert_eq!(evaluate_ack(&[0x06], "ID1", &simple_mode), AckResult::Ack);
        assert_eq!(evaluate_ack(&[0x15], "ID1", &simple_mode), AckResult::Nak);

        // 2. Legacy substring mode
        let legacy_mode = AckMode::LegacySubstring {
            token: "MSA|AA".to_string(),
        };
        assert_eq!(
            evaluate_ack(b"MSH|^~\\&\rMSA|AA|ID1", "ID1", &legacy_mode),
            AckResult::Ack
        );
        assert_eq!(
            evaluate_ack(b"MSH|^~\\&\rMSA|AE|ID1", "ID1", &legacy_mode),
            AckResult::Nak
        );

        // 3. HL7 mode
        let hl7_mode = AckMode::Hl7;
        let valid_ack = b"MSH|^~\\&|R|F|S|F|20260101||ACK|ACK1|P|2.5\rMSA|AA|SENT_ID_100";
        assert_eq!(
            evaluate_ack(valid_ack, "SENT_ID_100", &hl7_mode),
            AckResult::Ack
        );

        // Mismatched control ID -> NAK
        assert_eq!(
            evaluate_ack(valid_ack, "WRONG_ID", &hl7_mode),
            AckResult::Nak
        );

        // NAK codes
        let naks = [b"AE", b"AR", b"CE", b"CR"];
        for code in naks {
            let nak_msg = format!(
                "MSH|^~\\&|R|F|S|F|20260101||ACK|ACK1|P|2.5\rMSA|{}|SENT_ID_100",
                std::str::from_utf8(code).unwrap()
            );
            assert_eq!(
                evaluate_ack(nak_msg.as_bytes(), "SENT_ID_100", &hl7_mode),
                AckResult::Nak
            );
        }

        // Enhanced Mode (Epic Bridges) CA Commit Accept (with CRLF and optional ERR) -> ACK
        let enhanced_ca =
            b"MSH|^~\\&|EPIC|FAC|SND|FAC|20260101||ACK^A01|ACK2|P|2.5.1\r\nMSA|CA|SENT_ID_100\r\n";
        assert_eq!(
            evaluate_ack(enhanced_ca, "SENT_ID_100", &hl7_mode),
            AckResult::Ack
        );

        // Build Enhanced Mode CA / CE / CR ACKs directly
        let inbound =
            b"MSH|^~\\&|SND|FAC|EPIC|FAC|20260101||ADT^A01|SENT_ID_100|P|2.5.1\rPID|1||P1\r";
        let built_ca = build_hl7_ack(inbound, AckCode::CA, Some("ACK_CA_1"));
        assert!(String::from_utf8_lossy(&built_ca).ends_with("MSA|CA|SENT_ID_100"));
        assert_eq!(
            evaluate_ack(&built_ca, "SENT_ID_100", &hl7_mode),
            AckResult::Ack
        );
        let built_ce = build_hl7_ack(inbound, AckCode::CE, Some("ACK_CE_1"));
        assert!(String::from_utf8_lossy(&built_ce).ends_with("MSA|CE|SENT_ID_100"));
        assert_eq!(
            evaluate_ack(&built_ce, "SENT_ID_100", &hl7_mode),
            AckResult::Nak
        );
        let built_cr = build_hl7_ack(inbound, AckCode::CR, Some("ACK_CR_1"));
        assert!(String::from_utf8_lossy(&built_cr).ends_with("MSA|CR|SENT_ID_100"));
        assert_eq!(
            evaluate_ack(&built_cr, "SENT_ID_100", &hl7_mode),
            AckResult::Nak
        );

        // Garbage or missing MSA -> NAK
        assert_eq!(
            evaluate_ack(b"GARBAGE_NOT_HL7", "SENT_ID_100", &hl7_mode),
            AckResult::Nak
        );
    }

    #[test]
    fn msh15_msh16_ack_suppression_and_msa2_extraction_respect_dynamic_delimiters() {
        // MSH-15=NE and MSH-16=NE -> suppresses ACK
        let both_ne =
            b"MSH|^~\\&|SND|FAC|RCV|FAC|20261123||ADT^A01|ID1|P|2.5.1|||NE|NE\rPID|1||123\r";
        assert!(msh_suppresses_ack(both_ne));

        // MSH-15 empty and MSH-16=NE -> suppresses ACK
        let empty_ne =
            b"MSH|^~\\&|SND|FAC|RCV|FAC|20261123||ADT^A01|ID2|P|2.5.1||||NE\rPID|1||123\r";
        assert!(msh_suppresses_ack(empty_ne));

        // Custom field delimiter `:` at MSH-1
        let custom_ne =
            b"MSH:^~\\&:SND:FAC:RCV:FAC:20261123::ADT^A01:ID3:P:2.5.1:::NE:NE\rPID:1::123\r";
        assert!(msh_suppresses_ack(custom_ne));

        // MSH-15=AL / MSH-16=NE -> does NOT suppress (accept ACK requested)
        let al_ne = b"MSH|^~\\&|SND|FAC|RCV|FAC|20261123||ADT^A01|ID4|P|2.5.1|||AL|NE\r";
        assert!(!msh_suppresses_ack(al_ne));

        // Missing MSH-15/16 -> does NOT suppress
        let standard = b"MSH|^~\\&|SND|FAC|RCV|FAC|20261123||ADT^A01|ID5|P|2.5.1\r";
        assert!(!msh_suppresses_ack(standard));

        // MSA-2 extraction with standard and custom delimiters
        let ack_std = b"MSH|^~\\&|RCV|FAC|SND|FAC|20261123||ACK|A1|P|2.5.1\rMSA|AA|ID1\r";
        assert_eq!(extract_msa2_control_id(ack_std).as_deref(), Some("ID1"));
        let ack_custom = b"MSH:^~\\&:RCV:FAC:SND:FAC:20261123::ACK:A2:P:2.5.1\rMSA:AA:ID3\r";
        assert_eq!(extract_msa2_control_id(ack_custom).as_deref(), Some("ID3"));
    }
}
