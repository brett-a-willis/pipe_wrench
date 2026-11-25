//! MLLP envelope configuration and tokio-util `MllpCodec` (NET-1..NET-4).

use std::net::SocketAddr;

use bytes::{Buf, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

use crate::error::NetError;

use serde::{Deserialize, Serialize};

/// Safe default maximum MLLP frame size (10 MiB) to guard against rogue/unterminated streams.
pub const DEFAULT_MAX_FRAME_BYTES: usize = 10 * 1024 * 1024;

/// Maximum number of raw bytes captured into a sanitized diagnostic preview for malformed packets.
pub const MAX_RAW_PREVIEW_BYTES: usize = 512;

/// Diagnostic record describing a malformed MLLP frame or unexpected/stray bytes received over TCP/IP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MalformedPacketInfo {
    /// HL7 timestamp when the malformed packet was observed.
    pub timestamp: String,
    /// Resolved remote peer socket address (`IP:port`).
    pub peer_addr: SocketAddr,
    /// Optional configured hostname (when the sender connected via a DNS name).
    pub configured_host: Option<String>,
    /// Specific failure reason (e.g., `"Missing MLLP start byte 0x0B"`).
    pub reason: String,
    /// Bounded, sanitized raw byte preview (capped at [`MAX_RAW_PREVIEW_BYTES`]).
    pub raw_preview: String,
    /// Total raw bytes observed.
    pub byte_len: usize,
}

/// Format up to `max_bytes` of raw wire bytes into a safe, readable ASCII preview with explicit
/// control-byte tokens (`<VT>`, `<FS>`, `<CR>`, `<LF>`, `<TAB>`, `<ACK>`, `<NAK>`, `\xNN`).
pub fn format_raw_byte_preview(bytes: &[u8], max_bytes: usize) -> String {
    let cap = max_bytes.max(1);
    let slice = &bytes[..bytes.len().min(cap)];
    let mut out = String::with_capacity(slice.len() + 32);
    for &b in slice {
        match b {
            0x0B => out.push_str("<VT>"),
            0x1C => out.push_str("<FS>"),
            0x0D => out.push_str("<CR>"),
            0x0A => out.push_str("<LF>"),
            0x09 => out.push_str("<TAB>"),
            0x06 => out.push_str("<ACK>"),
            0x15 => out.push_str("<NAK>"),
            0x20..=0x7E => out.push(char::from(b)),
            other => {
                use std::fmt::Write;
                let _ = write!(out, "\\x{other:02X}");
            }
        }
    }
    if bytes.len() > cap {
        use std::fmt::Write;
        let _ = write!(out, " ... ({} bytes total)", bytes.len());
    }
    out
}

/// Classify raw unterminated or unframed bytes into a specific human-readable MLLP failure reason.
pub fn classify_malformed_bytes(bytes: &[u8], envelope: MllpEnvelope, context: &str) -> String {
    let sb = envelope.start_block;
    let ed = envelope.end_data;
    let eb = envelope.end_block;
    if bytes.contains(&sb) {
        format!("Incomplete MLLP frame (missing 0x{ed:02X} 0x{eb:02X} before {context})")
    } else if looks_like_unframed_hl7(bytes) {
        format!(
            "Unframed raw HL7 payload (missing MLLP 0x{sb:02X} and 0x{ed:02X} 0x{eb:02X} before {context})"
        )
    } else {
        format!("Missing MLLP start byte 0x{sb:02X} (unexpected/stray bytes before {context})")
    }
}

fn looks_like_unframed_hl7(bytes: &[u8]) -> bool {
    let trimmed = bytes.trim_ascii_start();
    if trimmed.len() < 4 {
        return false;
    }
    let tag = &trimmed[0..3];
    (tag == b"MSH" || tag == b"FHS" || tag == b"BHS") && trimmed[3].is_ascii_punctuation()
}

/// Configurable MLLP envelope framing bytes (NET-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MllpEnvelope {
    /// Start Block byte (default `0x0B`).
    pub start_block: u8,
    /// End Data byte (default `0x1C`).
    pub end_data: u8,
    /// End Block byte (default `0x0D`).
    pub end_block: u8,
}

impl Default for MllpEnvelope {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl MllpEnvelope {
    /// Standard MLLP framing characters: `0x0B`, `0x1C`, `0x0D`.
    pub const DEFAULT: Self = Self {
        start_block: 0x0B,
        end_data: 0x1C,
        end_block: 0x0D,
    };

    /// Parse a single envelope byte from string formatted as hex (`0x0B` or `0B`),
    /// decimal (`11`), or octal (`0o13` or `13o`).
    pub fn parse_byte(input: &str) -> Option<u8> {
        let s = input.trim();
        if s.is_empty() {
            return None;
        }
        if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
            u8::from_str_radix(hex, 16).ok()
        } else if let Some(oct) = s.strip_prefix("0o").or_else(|| s.strip_prefix("0O")) {
            u8::from_str_radix(oct, 8).ok()
        } else if let Some(oct) = s.strip_suffix('o').or_else(|| s.strip_suffix('O')) {
            u8::from_str_radix(oct, 8).ok()
        } else if s.chars().all(|c| c.is_ascii_digit()) {
            s.parse::<u8>().ok()
        } else if s.chars().all(|c| c.is_ascii_hexdigit()) && s.len() <= 2 {
            u8::from_str_radix(s, 16).ok()
        } else {
            None
        }
    }

    /// Reset all three markers to standard defaults (NET-1).
    pub fn reset_defaults(&mut self) {
        *self = Self::DEFAULT;
    }
}

/// An extracted inbound MLLP frame payload with metadata (NET-3, NET-4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundFrame {
    /// The extracted HL7 payload with envelope bytes stripped.
    pub payload: Vec<u8>,
    /// `true` if the frame was missing the leading Start Block marker (NET-4).
    pub missing_start_block: bool,
    /// Bounded leading non-Start-Block bytes skipped before the Start Block marker (empty if clean).
    pub leading_garbage: Vec<u8>,
}

/// Asynchronous tokio-util codec for MLLP framing (NET-2, NET-3, NET-4).
///
/// Can be used with any `AsyncRead + AsyncWrite` (e.g. `TcpStream` or `TlsStream`).
#[derive(Debug, Clone)]
pub struct MllpCodec {
    envelope: MllpEnvelope,
    max_frame_bytes: usize,
}

impl Default for MllpCodec {
    fn default() -> Self {
        Self::new(MllpEnvelope::DEFAULT, DEFAULT_MAX_FRAME_BYTES)
    }
}

impl MllpCodec {
    /// Create a new codec with specific envelope characters and max frame size.
    pub fn new(envelope: MllpEnvelope, max_frame_bytes: usize) -> Self {
        Self {
            envelope,
            max_frame_bytes,
        }
    }

    /// Return the active envelope framing configuration.
    pub fn envelope(&self) -> MllpEnvelope {
        self.envelope
    }

    /// Return the maximum allowed frame bytes.
    pub fn max_frame_bytes(&self) -> usize {
        self.max_frame_bytes
    }
}

impl Encoder<&[u8]> for MllpCodec {
    type Error = NetError;

    /// Outbound framing (NET-2):
    /// - Strip any existing Start Block, End Data, and End Block bytes from payload.
    /// - Normalize any `\r\n` or standalone `\n` segment line endings to strict HL7 v2 `\r` (`0x0D`).
    /// - Strip duplicate trailing `\r` bytes so the frame ends cleanly with `<CR><ED><EB>` (`0x0D 0x1C 0x0D`).
    fn encode(&mut self, item: &[u8], dst: &mut BytesMut) -> Result<(), Self::Error> {
        let core_env = hl7_core::MllpEnvelope {
            start_block: self.envelope.start_block,
            end_data: self.envelope.end_data,
            end_block: self.envelope.end_block,
        };
        let framed = core_env.frame_outbound(item);
        dst.reserve(framed.len());
        dst.extend_from_slice(&framed);
        Ok(())
    }
}

impl Encoder<Vec<u8>> for MllpCodec {
    type Error = NetError;

    fn encode(&mut self, item: Vec<u8>, dst: &mut BytesMut) -> Result<(), Self::Error> {
        self.encode(item.as_slice(), dst)
    }
}

impl Decoder for MllpCodec {
    type Item = InboundFrame;
    type Error = NetError;

    /// Inbound framing (NET-3, NET-4):
    /// - Accumulate stream bytes until `ED + EB` terminator pair is found.
    /// - Strip leading `SB` (and any junk before `SB`).
    /// - If `SB` was missing, accept with `missing_start_block = true` (NET-4).
    /// - Abort with `NetError::FrameTooLarge` if frame exceeds `max_frame_bytes` (NET-4).
    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if src.is_empty() {
            return Ok(None);
        }

        let sb = self.envelope.start_block;
        let ed = self.envelope.end_data;
        let eb = self.envelope.end_block;

        // Search for ED + EB sequence in the buffer.
        let mut terminator_pos = None;
        for i in 0..src.len().saturating_sub(1) {
            if src[i] == ed && src[i + 1] == eb {
                terminator_pos = Some(i);
                break;
            }
        }

        let term_idx = match terminator_pos {
            Some(pos) => pos,
            None => {
                // Check if buffer exceeded max_frame_bytes without finding terminator.
                if src.len() > self.max_frame_bytes {
                    let actual = src.len();
                    let raw_preview = format_raw_byte_preview(src, MAX_RAW_PREVIEW_BYTES);
                    src.clear();
                    return Err(NetError::FrameTooLarge {
                        max_bytes: self.max_frame_bytes,
                        actual_bytes: actual,
                        raw_preview,
                    });
                }
                return Ok(None);
            }
        };

        if term_idx > self.max_frame_bytes {
            let raw_preview = format_raw_byte_preview(&src[..term_idx], MAX_RAW_PREVIEW_BYTES);
            src.advance(term_idx + 2);
            return Err(NetError::FrameTooLarge {
                max_bytes: self.max_frame_bytes,
                actual_bytes: term_idx,
                raw_preview,
            });
        }

        // Slice up to term_idx (payload + any leading junk/SB).
        let raw_frame = src.split_to(term_idx);
        // Consume the ED + EB (2 bytes).
        src.advance(2);

        // Analyze leading SB and preserve bounded leading garbage if present.
        let (payload_bytes, missing_start_block, leading_garbage) =
            match memchr::memchr(sb, &raw_frame) {
                Some(sb_pos) => {
                    let garbage = if sb_pos > 0 {
                        raw_frame[..sb_pos.min(MAX_RAW_PREVIEW_BYTES)].to_vec()
                    } else {
                        Vec::new()
                    };
                    (&raw_frame[sb_pos + 1..], false, garbage)
                }
                None => (&raw_frame[..], true, Vec::new()),
            };

        Ok(Some(InboundFrame {
            payload: payload_bytes.to_vec(),
            missing_start_block,
            leading_garbage,
        }))
    }

    fn decode_eof(&mut self, buf: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if let Some(frame) = self.decode(buf)? {
            return Ok(Some(frame));
        }
        if buf.is_empty() {
            return Ok(None);
        }
        let total_bytes = buf.len();
        let reason = classify_malformed_bytes(buf, self.envelope, "EOF");
        let raw_preview = format_raw_byte_preview(buf, MAX_RAW_PREVIEW_BYTES);
        buf.clear();
        Err(NetError::MalformedFrame {
            reason,
            raw_preview,
            total_bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_envelope_byte_formats() {
        assert_eq!(MllpEnvelope::parse_byte("0x0B"), Some(0x0B));
        assert_eq!(MllpEnvelope::parse_byte("0B"), Some(0x0B));
        assert_eq!(MllpEnvelope::parse_byte("11"), Some(11));
        assert_eq!(MllpEnvelope::parse_byte("0o13"), Some(11));
        assert_eq!(MllpEnvelope::parse_byte("13o"), Some(11));
        assert_eq!(MllpEnvelope::parse_byte("invalid"), None);
    }

    #[test]
    fn outbound_mllp_encoding_normalizes_crlf_and_lf_to_strict_cr_with_clean_trailer() {
        let mut codec = MllpCodec::new(MllpEnvelope::DEFAULT, DEFAULT_MAX_FRAME_BYTES);

        for raw in [
            b"MSH|^~\\&|A|B|C|D|20261123||ADT^A01|1|P|2.5.1\r\nPID|1||123\r\n\r\n".as_slice(),
            b"MSH|^~\\&|A|B|C|D|20261123||ADT^A01|1|P|2.5.1\nPID|1||123\n".as_slice(),
            b"MSH|^~\\&|A|B|C|D|20261123||ADT^A01|1|P|2.5.1\r\nPID|1||123\r\r\r".as_slice(),
        ] {
            let mut dst = BytesMut::new();
            codec.encode(raw, &mut dst).unwrap();
            assert_eq!(dst.first(), Some(&0x0B), "must start with <VT> (0x0B)");
            assert!(
                dst.ends_with(&[0x0D, 0x1C, 0x0D]),
                "must end cleanly with <CR><FS><CR> (0x0D 0x1C 0x0D)"
            );
            assert!(
                !dst.ends_with(&[0x0D, 0x0D, 0x1C, 0x0D]),
                "must not contain duplicate trailing <CR> before <FS><CR>"
            );
            assert!(
                !dst.contains(&0x0A),
                "wire frame must contain zero LF (0x0A) bytes"
            );
            assert_eq!(
                &dst[1..dst.len() - 2],
                b"MSH|^~\\&|A|B|C|D|20261123||ADT^A01|1|P|2.5.1\rPID|1||123\r"
            );
        }
    }
}
