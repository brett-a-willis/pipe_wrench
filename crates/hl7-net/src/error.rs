//! Network errors for `hl7-net` (NFR-5).

use thiserror::Error;

/// Errors produced during MLLP encoding/decoding and networking operations.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum NetError {
    /// Inbound MLLP frame exceeded the configured maximum size (NET-4).
    #[error(
        "MLLP frame exceeded maximum size of {max_bytes} bytes (received {actual_bytes} bytes)"
    )]
    FrameTooLarge {
        /// Configured limit.
        max_bytes: usize,
        /// Actual observed bytes before abort.
        actual_bytes: usize,
        /// Bounded, sanitized preview of the leading raw bytes.
        raw_preview: String,
    },
    /// Inbound bytes did not conform to a valid MLLP frame (missing `0x0B`, missing `0x1C 0x0D` before EOF, etc.).
    #[error("{reason} ({total_bytes} bytes)")]
    MalformedFrame {
        /// Human-readable reason describing the framing violation.
        reason: String,
        /// Bounded, sanitized preview of the raw bytes.
        raw_preview: String,
        /// Total raw bytes observed.
        total_bytes: usize,
    },
    /// An underlying I/O error occurred.
    #[error("network I/O error: {0}")]
    Io(String),
}

impl From<std::io::Error> for NetError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err.to_string())
    }
}
