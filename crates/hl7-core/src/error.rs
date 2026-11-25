//! Error types for `hl7-core` (NFR-5).

use std::path::PathBuf;

use thiserror::Error;

/// Errors returned when constructing or validating HL7 delimiters.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DelimiterError {
    /// Two or more of the five delimiter bytes are identical (D-11).
    #[error("delimiter characters must be distinct (duplicate byte 0x{0:02X})")]
    DuplicateByte(u8),
    /// A delimiter byte is a forbidden control byte such as CR, LF, or NUL (D-11).
    #[error("invalid delimiter byte 0x{0:02X}: must be printable non-whitespace ASCII")]
    InvalidByte(u8),
}

/// Errors returned when parsing or constructing a [`crate::FieldAddress`].
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AddressError {
    /// Input address string was empty or malformed.
    #[error("invalid field address '{input}': {reason}")]
    InvalidFormat {
        /// Original input string.
        input: String,
        /// Explanation of why parsing failed.
        reason: &'static str,
    },
    /// Segment tag was not 3 ASCII uppercase alphanumeric characters.
    #[error("invalid segment tag '{tag}': must be 3 ASCII alphanumeric characters")]
    InvalidSegmentTag {
        /// The offending segment tag.
        tag: String,
    },
    /// A 1-based numeric index (field, repetition, component, or subcomponent) was zero or invalid.
    #[error("invalid 1-based index in address '{input}'")]
    InvalidIndex {
        /// Original input string.
        input: String,
    },
}

/// Errors returned by [`crate::scanner::RecordBoundaryScanner`].
#[derive(Debug, Error)]
pub enum ScanError {
    /// Underlying I/O error while reading stream bytes.
    #[error("I/O error at byte offset {offset}: {source}")]
    Io {
        /// Byte offset where the read failed.
        offset: u64,
        /// Underlying OS I/O error.
        #[source]
        source: std::io::Error,
    },
}

/// Errors returned by file loading, decompression, and saving operations (FO-*, FS-*).
#[derive(Debug, Error)]
pub enum IoError {
    /// Failed to open, read, or write a file.
    #[error("file I/O error for '{}': {source}", path.display())]
    FileIo {
        /// Path associated with the failure.
        path: PathBuf,
        /// Underlying OS I/O error.
        #[source]
        source: std::io::Error,
    },
    /// Failed to read or decompress a `.zip` archive (FO-4).
    #[error("zip archive error for '{}': {message}", path.display())]
    Zip {
        /// Path of the `.zip` archive.
        path: PathBuf,
        /// Details from the zip reader.
        message: String,
    },
    /// Unsupported `.Z` (LZW compress) file (FO-5, D-6).
    #[error("unsupported .Z (LZW compress) format in '{}'; decompress externally before opening", path.display())]
    UnsupportedCompressFormat {
        /// Path of the `.Z` file.
        path: PathBuf,
    },
    /// Duplicate or empty file filter pattern (FO-2).
    #[error("invalid or duplicate file filter pattern '{pattern}'")]
    InvalidFilterPattern {
        /// The rejected pattern.
        pattern: String,
    },
    /// Scanner error while loading records.
    #[error(transparent)]
    Scan(#[from] ScanError),
    /// Atomic save failed while persisting temp file (FS-7).
    #[error("failed to atomically save '{}': {source}", path.display())]
    AtomicPersist {
        /// Target destination path.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },
}

/// Errors returned by [`crate::edit::EditCommand`] execution and undo/redo (EDIT-*).
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum EditError {
    /// Attempted to edit `MSH-1/2`, `FHS-1/2`, or `BHS-1/2` (EDIT-3, D-9).
    #[error("Field is not editable")]
    CannotEditField {
        /// Offending field address.
        address: String,
    },
    /// User-supplied text contained a disallowed structural delimiter for the target HL7 level (EDIT-4, EDIT-5, EDIT-6).
    #[error("{message}")]
    DelimiterInValue {
        /// Disallowed delimiter characters at the target HL7 level.
        delimiters: String,
        /// Level-specific validation notice message.
        message: String,
    },
    /// Attempted to delete the `MSH` segment (EDIT-7).
    #[error("Cannot delete the MSH segment.")]
    CannotDeleteMsh,
    /// Attempted to delete segment index 0 of an envelope record (`FHS`/`BHS`/`BTS`/`FTS`) (D-10).
    #[error("Cannot delete the {tag} header segment; delete the record instead.")]
    CannotDeleteHeaderSegment {
        /// Envelope segment tag.
        tag: String,
    },
    /// Change Message Delimiters did not receive exactly five characters (EDIT-9).
    #[error("Enter exactly five delimiter characters.")]
    InvalidDelimiterLength {
        /// Actual byte count supplied.
        got: usize,
    },
    /// Empty find text in Replace (EDIT-6).
    #[error("Find text cannot be empty.")]
    EmptyFindText,
    /// Delimiter validation failed (EDIT-9, D-11).
    #[error(transparent)]
    Delimiter(#[from] DelimiterError),
    /// Record index out of bounds.
    #[error("record index {index} out of bounds (document has {len} records)")]
    RecordOutOfBounds {
        /// Requested record index.
        index: usize,
        /// Current document record count.
        len: usize,
    },
    /// Segment index out of bounds.
    #[error("segment index {index} out of bounds (record has {len} segments)")]
    SegmentOutOfBounds {
        /// Requested segment index.
        index: usize,
        /// Current record segment count.
        len: usize,
    },
}

/// Top-level error type for core HL7 record operations.
#[derive(Debug, Error)]
pub enum CoreError {
    /// Segment index out of bounds for the record.
    #[error("segment index {index} out of bounds (record has {len} segments)")]
    SegmentOutOfBounds {
        /// Requested segment index.
        index: usize,
        /// Total segment count in the record.
        len: usize,
    },
    /// Delimiter validation failed.
    #[error(transparent)]
    Delimiter(#[from] DelimiterError),
    /// Field address parsing/validation failed.
    #[error(transparent)]
    Address(#[from] AddressError),
    /// Streaming scan failed.
    #[error(transparent)]
    Scan(#[from] ScanError),
    /// File load/save failed.
    #[error(transparent)]
    Io(#[from] IoError),
    /// Edit command failed.
    #[error(transparent)]
    Edit(#[from] EditError),
    /// Dictionary parse or load error (§5.4).
    #[error("dictionary error: {0}")]
    Dictionary(String),
}
