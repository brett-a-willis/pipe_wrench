//! Base64 and HL7 v2 Encapsulated Data (`ED`) payload detection and binary extraction.
//!
//! Supports both structured HL7 `ED` fields (e.g., `^Application^PDF^Base64^JVBERi0x...`,
//! `^Image^JPEG^Base64^...`, `^Text^RTF^Base64^...`) and standalone raw Base64 strings
//! exceeding [`MIN_STANDALONE_BASE64_LEN`].

use crate::address::FieldAddress;
use crate::record::Record;

/// Minimum cleaned character length for detecting a standalone (non-`ED`) Base64 payload.
pub const MIN_STANDALONE_BASE64_LEN: usize = 64;

/// Lightweight metadata describing a detected Base64 / `ED` attachment without retaining
/// the decoded binary buffer in UI caches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentSummary {
    /// Lowercase file extension without leading dot (e.g. `"pdf"`, `"rtf"`, `"png"`, `"bin"`).
    pub extension: &'static str,
    /// Uppercase human-readable format label (e.g. `"PDF"`, `"RTF"`, `"PNG"`, `"BIN"`).
    pub label: &'static str,
    /// Exact decoded binary length in bytes.
    pub decoded_len: usize,
    /// `true` when detected from a structured HL7 `ED` (`^Type^Subtype^Base64^Data`) field.
    pub is_ed_format: bool,
}

impl AttachmentSummary {
    /// Format a compact UI badge label such as `"[Base64 PDF • 24.5 KB]"`.
    pub fn badge_text(&self) -> String {
        format!(
            "[Base64 {} • {}]",
            self.label,
            format_byte_size(self.decoded_len)
        )
    }
}

/// Decoded Base64 / HL7 `ED` attachment ready to be written to disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedAttachment {
    /// Lowercase file extension without leading dot (e.g. `"pdf"`, `"jpg"`, `"bin"`).
    pub extension: &'static str,
    /// Uppercase human-readable format label (e.g. `"PDF"`, `"JPEG"`, `"BIN"`).
    pub label: &'static str,
    /// Decoded raw binary payload bytes.
    pub bytes: Vec<u8>,
    /// `true` when detected from a structured HL7 `ED` (`^Type^Subtype^Base64^Data`) field.
    pub is_ed_format: bool,
}

impl DetectedAttachment {
    /// Convert into lightweight [`AttachmentSummary`] metadata.
    pub fn summary(&self) -> AttachmentSummary {
        AttachmentSummary {
            extension: self.extension,
            label: self.label,
            decoded_len: self.bytes.len(),
            is_ed_format: self.is_ed_format,
        }
    }

    /// Format a compact UI badge label such as `"[Base64 PDF • 24.5 KB]"`.
    pub fn badge_text(&self) -> String {
        self.summary().badge_text()
    }

    /// Build a filesystem-safe default filename of the form `<MSH-10>_<SEG-F>.<ext>`.
    pub fn suggested_filename(&self, control_id: &str, field_path: &str) -> String {
        let cid = sanitize_filename_token(control_id);
        let cid = if cid.is_empty() { "MSG" } else { cid.as_str() };
        let path = sanitize_filename_token(field_path);
        let path = if path.is_empty() {
            "ATTACHMENT"
        } else {
            path.as_str()
        };
        format!("{cid}_{path}.{}", self.extension)
    }
}

/// One extracted attachment located inside a parsed HL7 [`Record`].
#[derive(Debug, Clone)]
pub struct ExtractedRecordAttachment {
    /// 0-based segment index in the record.
    pub segment_index: usize,
    /// Canonical HL7 field path (e.g. `"OBX-5"`).
    pub field_path: String,
    /// Decoded attachment payload and format metadata.
    pub attachment: DetectedAttachment,
}

/// Format a byte length into a concise human-readable size (`"512 B"`, `"24.5 KB"`, `"1.2 MB"`).
pub fn format_byte_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// Detect and decode an HL7 `ED` (`^Type^Subtype^Base64^Data`) or standalone Base64 value.
///
/// Returns `None` if the value is not a valid `ED`/Base64 payload.
pub fn detect_and_decode_attachment(
    value: &str,
    component_sep: char,
) -> Option<DetectedAttachment> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    // 1. Check structured HL7 Encapsulated Data (`ED`): ED.1^ED.2^ED.3^ED.4^ED.5
    if trimmed.contains(component_sep) {
        let parts: Vec<&str> = trimmed.split(component_sep).collect();
        if parts.len() >= 5 && parts[3].trim().eq_ignore_ascii_case("base64") {
            let payload_str = parts[4].trim();
            if let Some(bytes) = decode_base64(payload_str) {
                if !bytes.is_empty() {
                    let (extension, label) =
                        detect_type_from_ed_or_magic(parts[1].trim(), parts[2].trim(), &bytes);
                    return Some(DetectedAttachment {
                        extension,
                        label,
                        bytes,
                        is_ed_format: true,
                    });
                }
            }
        }
        return None;
    }

    // 2. Check standalone raw Base64 payload (>= MIN_STANDALONE_BASE64_LEN chars, multiple of 4)
    let cleaned_len = trimmed.bytes().filter(|b| !b.is_ascii_whitespace()).count();
    if cleaned_len < MIN_STANDALONE_BASE64_LEN || !cleaned_len.is_multiple_of(4) {
        return None;
    }
    let bytes = decode_base64(trimmed)?;
    if bytes.is_empty() {
        return None;
    }
    let magic = detect_type_from_magic(&bytes);
    if magic.is_none() && !looks_like_standalone_base64(trimmed) {
        return None;
    }
    let (extension, label) = magic.unwrap_or(("bin", "BIN"));
    Some(DetectedAttachment {
        extension,
        label,
        bytes,
        is_ed_format: false,
    })
}

/// Detect an attachment and return only its lightweight [`AttachmentSummary`].
pub fn detect_attachment_summary(value: &str, component_sep: char) -> Option<AttachmentSummary> {
    detect_and_decode_attachment(value, component_sep).map(|a| a.summary())
}

/// Scan an HL7 [`Record`] for Base64 / `ED` attachments across the specified field specs.
///
/// - When `field_specs` is empty, defaults to scanning `OBX-5`.
/// - Pass `["*"]` to scan all fields of every non-header segment.
/// - Otherwise each spec is a `SEG-F` target such as `"OBX-5"` or `"TXA-12"`.
pub fn extract_record_attachments(
    record: &Record,
    field_specs: &[String],
) -> Vec<ExtractedRecordAttachment> {
    let delims = record.delimiters();
    let comp_sep = delims.component as char;
    let scan_all = field_specs.iter().any(|s| s.trim() == "*");
    let parsed_targets: Vec<([u8; 3], u16)> = if scan_all {
        Vec::new()
    } else if field_specs.is_empty() {
        vec![(*b"OBX", 5)]
    } else {
        field_specs
            .iter()
            .filter_map(|raw| {
                let addr: FieldAddress = raw.trim().parse().ok()?;
                Some((addr.segment, addr.field.get()))
            })
            .collect()
    };

    let mut results = Vec::new();
    for (seg_idx, span) in record.segments().iter().enumerate() {
        let is_header = &span.tag == b"MSH" || &span.tag == b"FHS" || &span.tag == b"BHS";
        if is_header {
            continue;
        }
        let Some(seg_bytes) = record.segment_bytes(seg_idx) else {
            continue;
        };
        if seg_bytes.len() <= 4 {
            continue;
        }
        let tag_str = String::from_utf8_lossy(&span.tag);
        let fields: Vec<&[u8]> = seg_bytes[4..].split(|&b| b == delims.field).collect();
        for (f_idx, field_bytes) in fields.iter().enumerate() {
            let field_num = (f_idx + 1) as u16;
            if !scan_all
                && !parsed_targets
                    .iter()
                    .any(|&(t, f)| t == span.tag && f == field_num)
            {
                continue;
            }
            if field_bytes.is_empty() {
                continue;
            }
            let raw_str = String::from_utf8_lossy(field_bytes);
            if let Some(attachment) = detect_and_decode_attachment(&raw_str, comp_sep) {
                results.push(ExtractedRecordAttachment {
                    segment_index: seg_idx,
                    field_path: format!("{tag_str}-{field_num}"),
                    attachment,
                });
            }
        }
    }
    results
}

/// Decode standard RFC 4648 Base64 (ignoring ASCII whitespace). Returns `None` on invalid input.
pub fn decode_base64(input: &str) -> Option<Vec<u8>> {
    let mut vals: Vec<u8> = Vec::with_capacity(input.len());
    let mut pad_count = 0usize;
    for b in input.bytes() {
        if b.is_ascii_whitespace() {
            continue;
        }
        if b == b'=' {
            pad_count += 1;
            if pad_count > 2 {
                return None;
            }
            vals.push(0xFF);
            continue;
        }
        if pad_count > 0 {
            // Non-padding character after '=' is invalid Base64.
            return None;
        }
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        vals.push(v);
    }

    if vals.is_empty() || !vals.len().is_multiple_of(4) {
        return None;
    }

    let mut out = Vec::with_capacity((vals.len() / 4) * 3);
    for chunk in vals.as_chunks::<4>().0 {
        let [a, b, c, d] = *chunk;
        if a == 0xFF || b == 0xFF {
            return None;
        }
        out.push((a << 2) | (b >> 4));
        if c != 0xFF {
            out.push((b << 4) | (c >> 2));
            if d != 0xFF {
                out.push((c << 6) | d);
            }
        } else if d != 0xFF {
            return None;
        }
    }
    Some(out)
}

fn looks_like_standalone_base64(s: &str) -> bool {
    let mut has_upper = false;
    let mut has_lower = false;
    let mut has_digit_or_sym = false;
    let mut has_pad = false;
    for b in s.bytes() {
        if b.is_ascii_uppercase() {
            has_upper = true;
        } else if b.is_ascii_lowercase() {
            has_lower = true;
        } else if b.is_ascii_digit() || b == b'+' || b == b'/' {
            has_digit_or_sym = true;
        } else if b == b'=' {
            has_pad = true;
        }
    }
    has_pad || (has_upper && has_lower && has_digit_or_sym)
}

fn detect_type_from_ed_or_magic(
    ed_type: &str,
    ed_subtype: &str,
    bytes: &[u8],
) -> (&'static str, &'static str) {
    if let Some(mapped) = map_ed_subtype(ed_subtype) {
        return mapped;
    }
    if let Some(magic) = detect_type_from_magic(bytes) {
        return magic;
    }
    if let Some(mapped) = map_ed_subtype(ed_type) {
        return mapped;
    }
    ("bin", "BIN")
}

fn map_ed_subtype(token: &str) -> Option<(&'static str, &'static str)> {
    match token.trim().to_ascii_uppercase().as_str() {
        "PDF" => Some(("pdf", "PDF")),
        "RTF" => Some(("rtf", "RTF")),
        "HTML" | "HTM" => Some(("html", "HTML")),
        "JPEG" | "JPG" => Some(("jpg", "JPEG")),
        "PNG" => Some(("png", "PNG")),
        "TIFF" | "TIF" => Some(("tif", "TIFF")),
        "GIF" => Some(("gif", "GIF")),
        "XML" => Some(("xml", "XML")),
        "TXT" | "TEXT" | "PLAIN" => Some(("txt", "TXT")),
        "ZIP" => Some(("zip", "ZIP")),
        _ => None,
    }
}

/// Detect file extension and human-readable label from leading magic bytes.
pub fn detect_type_from_magic(bytes: &[u8]) -> Option<(&'static str, &'static str)> {
    if bytes.starts_with(b"%PDF") {
        Some(("pdf", "PDF"))
    } else if bytes.starts_with(b"{\\rtf") {
        Some(("rtf", "RTF"))
    } else if bytes.starts_with(b"\x89PNG\r\n\x1A\n") {
        Some(("png", "PNG"))
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(("jpg", "JPEG"))
    } else if bytes.starts_with(b"PK\x03\x04") {
        Some(("zip", "ZIP"))
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(("gif", "GIF"))
    } else if bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*") {
        Some(("tif", "TIFF"))
    } else if bytes.starts_with(b"<?xml") {
        Some(("xml", "XML"))
    } else if bytes.len() >= 5
        && (bytes[..5].eq_ignore_ascii_case(b"<html")
            || (bytes.len() >= 9 && bytes[..9].eq_ignore_ascii_case(b"<!doctype")))
    {
        Some(("html", "HTML"))
    } else {
        None
    }
}

fn sanitize_filename_token(raw: &str) -> String {
    raw.trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{load_from_bytes, ScannerConfig};

    fn encode_base64(data: &[u8]) -> String {
        const TABLE: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
        for chunk in data.chunks(3) {
            let b0 = chunk[0];
            let b1 = if chunk.len() > 1 { chunk[1] } else { 0 };
            let b2 = if chunk.len() > 2 { chunk[2] } else { 0 };
            out.push(TABLE[(b0 >> 2) as usize] as char);
            out.push(TABLE[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
            if chunk.len() > 1 {
                out.push(TABLE[(((b1 & 0x0F) << 2) | (b2 >> 6)) as usize] as char);
            } else {
                out.push('=');
            }
            if chunk.len() > 2 {
                out.push(TABLE[(b2 & 0x3F) as usize] as char);
            } else {
                out.push('=');
            }
        }
        out
    }

    #[test]
    fn detects_ed_pdf_rtf_jpeg_and_verifies_decoding_integrity() {
        let pdf_bytes = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\n%%EOF\n";
        let pdf_b64 = encode_base64(pdf_bytes);
        let ed_pdf = format!("^Application^PDF^Base64^{pdf_b64}");

        let det = detect_and_decode_attachment(&ed_pdf, '^').unwrap();
        assert!(det.is_ed_format);
        assert_eq!(det.extension, "pdf");
        assert_eq!(det.label, "PDF");
        assert_eq!(det.bytes, pdf_bytes);
        assert_eq!(
            det.suggested_filename("MSG0001", "OBX-5"),
            "MSG0001_OBX-5.pdf"
        );
        assert!(det.badge_text().starts_with("[Base64 PDF"));

        let rtf_bytes = b"{\\rtf1\\ansi Clinical Impression: Normal sinus rhythm.}";
        let ed_rtf = format!("^Text^RTF^Base64^{}", encode_base64(rtf_bytes));
        let det_rtf = detect_and_decode_attachment(&ed_rtf, '^').unwrap();
        assert_eq!(det_rtf.extension, "rtf");
        assert_eq!(det_rtf.bytes, rtf_bytes);

        let jpg_bytes = [0xFFu8, 0xD8, 0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F'];
        let ed_jpg = format!("^Image^JPEG^Base64^{}", encode_base64(&jpg_bytes));
        let det_jpg = detect_and_decode_attachment(&ed_jpg, '^').unwrap();
        assert_eq!(det_jpg.extension, "jpg");
        assert_eq!(det_jpg.label, "JPEG");
        assert_eq!(det_jpg.bytes, jpg_bytes);
    }

    #[test]
    fn detects_standalone_base64_and_magic_bytes_with_bin_fallback() {
        // PNG magic bytes padded to >= 64 base64 chars
        let mut png_data = b"\x89PNG\r\n\x1A\n".to_vec();
        png_data.extend_from_slice(&[0u8; 48]);
        let png_b64 = encode_base64(&png_data);
        assert!(png_b64.len() >= MIN_STANDALONE_BASE64_LEN);
        let det_png = detect_and_decode_attachment(&png_b64, '^').unwrap();
        assert!(!det_png.is_ed_format);
        assert_eq!(det_png.extension, "png");
        assert_eq!(det_png.bytes, png_data);

        // ZIP magic bytes
        let mut zip_data = b"PK\x03\x04".to_vec();
        zip_data.extend_from_slice(&[1u8; 48]);
        let det_zip = detect_and_decode_attachment(&encode_base64(&zip_data), '^').unwrap();
        assert_eq!(det_zip.extension, "zip");

        // Arbitrary binary fallback -> .bin
        let raw_bin: Vec<u8> = (0u8..56).collect();
        let det_bin = detect_and_decode_attachment(&encode_base64(&raw_bin), '^').unwrap();
        assert_eq!(det_bin.extension, "bin");
        assert_eq!(det_bin.label, "BIN");
        assert_eq!(det_bin.bytes, raw_bin);

        // Short strings or plain prose must NOT false-positive
        assert!(detect_and_decode_attachment("HELLO", '^').is_none());
        assert!(detect_and_decode_attachment(
            "Patient presented with normal vital signs and no acute distress.",
            '^'
        )
        .is_none());
    }

    #[test]
    fn extracts_attachments_from_obx5_in_parsed_record() {
        let pdf_bytes = b"%PDF-1.7\nSynthetic Clinical Report\n%%EOF";
        let pdf_b64 = encode_base64(pdf_bytes);
        let raw = format!(
            "MSH|^~\\&|LAB|MAIN|EHR|MAIN|20261123||ORU^R01|MSG777|P|2.5.1\rOBX|1|ED|PDF||^Application^PDF^Base64^{pdf_b64}\r"
        );
        let out = load_from_bytes(raw.as_bytes(), ScannerConfig::default()).unwrap();
        let extracted = extract_record_attachments(&out.records[0], &[]);
        assert_eq!(extracted.len(), 1);
        assert_eq!(extracted[0].field_path, "OBX-5");
        assert_eq!(extracted[0].attachment.extension, "pdf");
        assert_eq!(extracted[0].attachment.bytes, pdf_bytes);
    }
}
