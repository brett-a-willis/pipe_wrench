//! Headless CLI library for `pipe_wrench_cli` (`SCAN-14`, `NET-10..NET-34`, `RPT-1..RPT-9`, `FS-1..FS-7`).
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]

pub mod convert;
pub mod extract;
pub mod filter;
pub mod listen;
pub mod replace;
pub mod report;
pub mod scan;
pub mod scrub;
pub mod search;
pub mod send;

use clap::{Parser, Subcommand};

use convert::ConvertArgs;
use extract::ExtractArgs;
use filter::FilterArgs;
use listen::ListenArgs;
use replace::ReplaceArgs;
use report::ReportArgs;
use scan::ScanArgs;
use scrub::ScrubArgs;
use search::SearchArgs;
use send::SendArgs;

/// Top-level CLI arguments for `pipe_wrench_cli`.
#[derive(Debug, Parser)]
#[command(
    name = "pipe_wrench_cli",
    about = "Headless HL7 v2 directory scanner, MLLP/TLS client & listener, report extractor, filter, search, and file converter"
)]
pub struct Cli {
    /// Subcommand to execute.
    #[command(subcommand)]
    pub command: Commands,
}

/// Supported CLI subcommands.
#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Count and summarize HL7 v2 messages across files in a directory without loading into GUI memory (`SCAN-14`).
    Scan(ScanArgs),
    /// Load HL7 v2 files and send messages over MLLP (plaintext TCP or TLS 1.2/1.3 & mTLS) (`NET-20..NET-34`).
    Send(SendArgs),
    /// Run a multi-connection MLLP listener server (plaintext TCP or TLS 1.2/1.3 & mTLS) until Ctrl+C (`NET-10..NET-17`).
    Listen(ListenArgs),
    /// Extract tabular reports from HL7 v2 files to CSV, JSON, delimited, or fixed-width text (`RPT-1..RPT-9`).
    Report(ReportArgs),
    /// Normalize segment terminators, toggle MLLP disk framing, sort by MSH-7, or change delimiters (`FS-1..FS-7`).
    Convert(ConvertArgs),
    /// Filter HL7 v2 records across files/directories by text, regex, or field-path comparisons (`D-26`).
    Filter(FilterArgs),
    /// HL7-aware grep across files/directories with field restriction, context fields, and field-path hit summary (`D-26`, `D-27`).
    #[command(visible_alias = "find")]
    Search(SearchArgs),
    /// Granular HL7 v2 field/component/subcomponent-aware find and replace (`D-27`).
    Replace(ReplaceArgs),
    /// Deterministically de-identify HL7 v2 PHI into referentially-consistent synthetic messages.
    Scrub(ScrubArgs),
    /// Detect and extract Base64 / HL7 `ED` encapsulated attachments from HL7 v2 messages.
    Extract(ExtractArgs),
}
