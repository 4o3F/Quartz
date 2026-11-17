//! Quartz process entry point.

#![cfg_attr(windows, windows_subsystem = "windows")]

mod entry;
mod logging;
mod platform;

fn main() -> std::process::ExitCode {
    entry::run(std::env::args_os().skip(1).collect())
}
