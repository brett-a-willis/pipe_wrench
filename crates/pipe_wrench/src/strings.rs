//! Centralized user-facing UI strings.

/// Application window base title.
pub const APP_TITLE: &str = "pipe_wrench";

/// Source label for clipboard paste (FO-15).
pub const SOURCE_CLIPBOARD: &str = "Clipboard";

/// Source label for TCP/IP listener records (NET-13).
pub const SOURCE_TCPIP: &str = "TCP/IP Listener";

/// Prompt when non-HL7 bytes are detected outside records (FO-11).
pub const PROMPT_NON_HL7_RECOVERY: &str =
    "This file contains non-HL7 data. Load any HL7 records found in it?";

/// Prompt when no HL7 records are found in an opened file (FO-12).
pub const MSG_NO_HL7_RECORDS_FOUND: &str = "No HL7 records were found in the selected file(s).";

/// Banner shown when MSH-18 names an unknown charset and falls back to Windows-1252 (G-2).
pub const BANNER_UNKNOWN_CHARSET_PREFIX: &str =
    "Unknown MSH-18 charset (falling back to Windows-1252): ";

/// Warning badge when an MLLP frame arrived without a Start Block byte (NET-4).
pub const BADGE_MISSING_START_BLOCK: &str = "Missing MLLP Start Block";

/// Error when attempting to edit MSH-1/2, FHS-1/2, or BHS-1/2 (EDIT-3).
pub const ERR_CANT_EDIT_FIELD: &str = "Field is not editable";

/// Button offered in Edit Field when value contains an active delimiter (EDIT-4).
pub const BTN_ENCODE_DELIMITERS: &str = "Encode delimiters as escape sequences";

/// Button in Edit Record to insert a segment boundary (CR) at the cursor (EDIT-2).
pub const BTN_INSERT_EOS: &str = "Insert End-of-Segment";

/// Error when attempting to delete the MSH segment (EDIT-7).
pub const ERR_CANT_DELETE_MSH: &str = "Cannot delete the MSH segment.";

/// Error when Change Message Delimiters input is not 5 chars (EDIT-9).
pub const ERR_DELIMITERS_FIVE_CHARS: &str = "Enter exactly five delimiter characters.";

/// Warning shown in Change Message Delimiters dialog (EDIT-9).
pub const WARN_CHANGE_DELIMITERS: &str =
    "Warning: Existing data containing any of the new delimiter characters will become unparsable.";

/// Unsaved changes prompt on exit or new open (FS-8).
pub const PROMPT_UNSAVED_CHANGES: &str =
    "You have unsaved changes. Do you want to save them before continuing?";

/// Confirmation prompt before deleting segments (EDIT-7).
pub fn confirm_delete_segments(count: usize) -> String {
    format!("Are you sure that you want to delete {count} segment(s)?")
}

/// Confirmation prompt before deleting records (EDIT-8).
pub fn confirm_delete_records(count: usize) -> String {
    format!("Are you sure that you want to delete {count} record(s)?")
}

/// Heading for a positive ACK in the Received ACK viewer (NET-27).
pub const MSG_RECEIVED_ACK: &str = "Received ACK";

/// Heading for a NAK in the Received ACK viewer (NET-27).
pub const MSG_RECEIVED_NAK: &str = "Received NAK";

/// Warning when the configured ACK-from-file does not exist (NET-15).
pub const ERR_MISSING_ACK_FILE: &str = "Custom ACK file not found; sending null ACK frame.";

/// Confirmation prompt before binding the listener to a non-loopback interface (AGENTS §9).
pub const WARN_NON_LOOPBACK_BIND: &str =
    "Binding to a non-loopback address exposes the MLLP listener to external network hosts. Proceed?";
