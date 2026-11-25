//! Desktop GUI entry point for `pipe_wrench`.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]

fn main() -> anyhow::Result<()> {
    pipe_wrench::run_native()
}
