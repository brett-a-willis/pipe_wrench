//! Headless CLI entry point for `pipe_wrench_cli`.
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]

use std::io::Write;

use clap::Parser;
use pipe_wrench_cli::convert::execute_convert_command;
use pipe_wrench_cli::extract::execute_extract_command;
use pipe_wrench_cli::filter::execute_filter_command;
use pipe_wrench_cli::listen::execute_listen_command;
use pipe_wrench_cli::replace::execute_replace_command;
use pipe_wrench_cli::report::execute_report_command;
use pipe_wrench_cli::scan::execute_scan_command;
use pipe_wrench_cli::scrub::execute_scrub_command;
use pipe_wrench_cli::search::execute_search_command;
use pipe_wrench_cli::send::execute_send_command;
use pipe_wrench_cli::{Cli, Commands};

fn main() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .try_init();

    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            let _ = e.print();
            std::process::exit(1);
        }
    };

    match cli.command {
        Commands::Scan(args) => match execute_scan_command(&args) {
            Ok((bytes, code)) => {
                if args.out.is_none() && !args.quiet {
                    let _ = std::io::stdout().write_all(&bytes);
                }
                std::process::exit(code);
            }
            Err(err) => {
                let _ = writeln!(std::io::stderr(), "Error: {err}");
                std::process::exit(1);
            }
        },
        Commands::Send(args) => {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = writeln!(std::io::stderr(), "Error starting async runtime: {e}");
                    std::process::exit(1);
                }
            };
            match rt.block_on(execute_send_command(&args)) {
                Ok(code) => std::process::exit(code),
                Err(err) => {
                    let _ = writeln!(std::io::stderr(), "Error: {err}");
                    std::process::exit(1);
                }
            }
        }
        Commands::Listen(args) => {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = writeln!(std::io::stderr(), "Error starting async runtime: {e}");
                    std::process::exit(1);
                }
            };
            match rt.block_on(execute_listen_command(&args)) {
                Ok(()) => std::process::exit(0),
                Err(err) => {
                    let _ = writeln!(std::io::stderr(), "Error: {err}");
                    std::process::exit(1);
                }
            }
        }
        Commands::Report(args) => match execute_report_command(&args) {
            Ok(bytes) => {
                if args.out.is_none() {
                    let _ = std::io::stdout().write_all(&bytes);
                }
                std::process::exit(0);
            }
            Err(err) => {
                let _ = writeln!(std::io::stderr(), "Error: {err}");
                std::process::exit(1);
            }
        },
        Commands::Convert(args) => match execute_convert_command(&args) {
            Ok(_) => std::process::exit(0),
            Err(err) => {
                let _ = writeln!(std::io::stderr(), "Error: {err}");
                std::process::exit(1);
            }
        },
        Commands::Filter(args) => match execute_filter_command(&args) {
            Ok((bytes, code)) => {
                if args.out.is_none() || args.count {
                    let _ = std::io::stdout().write_all(&bytes);
                }
                std::process::exit(code);
            }
            Err(err) => {
                let _ = writeln!(std::io::stderr(), "Error: {err}");
                std::process::exit(1);
            }
        },
        Commands::Search(args) => match execute_search_command(&args) {
            Ok((bytes, code)) => {
                if args.out.is_none() {
                    let _ = std::io::stdout().write_all(&bytes);
                }
                std::process::exit(code);
            }
            Err(err) => {
                let _ = writeln!(std::io::stderr(), "Error: {err}");
                std::process::exit(1);
            }
        },
        Commands::Replace(args) => match execute_replace_command(&args) {
            Ok((bytes, _, code)) => {
                let _ = std::io::stdout().write_all(&bytes);
                std::process::exit(code);
            }
            Err(err) => {
                let _ = writeln!(std::io::stderr(), "Error: {err}");
                std::process::exit(1);
            }
        },
        Commands::Scrub(args) => match execute_scrub_command(&args) {
            Ok((count, seed)) => {
                let _ = writeln!(
                    std::io::stderr(),
                    "Scrubbed {count} message(s) to '{}' (seed: {seed}).\n{}",
                    args.out.display(),
                    hl7_core::PHI_SCRUB_DISCLAIMER
                );
                std::process::exit(0);
            }
            Err(err) => {
                let _ = writeln!(std::io::stderr(), "Error: {err}");
                std::process::exit(1);
            }
        },
        Commands::Extract(args) => match execute_extract_command(&args) {
            Ok(paths) => {
                let _ = writeln!(
                    std::io::stderr(),
                    "Extracted {} attachment(s) to '{}'.",
                    paths.len(),
                    args.out.display()
                );
                std::process::exit(0);
            }
            Err(err) => {
                let _ = writeln!(std::io::stderr(), "Error: {err}");
                std::process::exit(1);
            }
        },
    }
}
