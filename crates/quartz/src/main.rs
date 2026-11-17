//! Quartz process entry point.

#![cfg_attr(windows, windows_subsystem = "windows")]

mod logging;
mod platform;

#[cfg(any(windows, test))]
mod instance_name;
#[cfg(any(windows, test))]
mod tray_tooltip;
#[cfg(any(windows, test))]
mod ui_state;

use std::{path::PathBuf, process::ExitCode};

use clap::{Parser, Subcommand};
use snafu::Snafu;

#[derive(Debug, PartialEq, Eq, Parser)]
#[command(name = "quartz", bin_name = "quartz", version)]
struct Args {
    /// Start from the operating system's autostart entry.
    #[arg(long)]
    autostart: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
enum Command {
    #[command(hide = true)]
    Ui {
        #[arg(long)]
        prewarm: bool,
    },
    #[command(flatten)]
    Cli(CliCommand),
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
enum CliCommand {
    /// Print intervals for a date.
    Dump {
        #[arg(long)]
        date: String,
    },
    /// Show core status.
    Status,
    /// Generate a year of data in an independent simulation database.
    GenData {
        #[arg(long)]
        db: PathBuf,
    },
    /// Benchmark queries against a simulation database.
    BenchQuery {
        #[arg(long)]
        db: PathBuf,
    },
    /// Restore a database backup while the core is stopped.
    RestoreBackup { file: PathBuf },
    /// Validate and inspect rules.
    #[command(subcommand)]
    Rules(RulesCommand),
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
enum RulesCommand {
    /// Check rule files and the installed rule tree offline.
    Check { files: Vec<PathBuf> },
    /// Preview rule matches.
    Preview {
        file: PathBuf,
        #[arg(long, default_value_t = 7)]
        days: u32,
    },
    /// Compare rule results.
    Diff {
        file: PathBuf,
        #[arg(long, default_value_t = 7)]
        days: u32,
    },
    /// Explain a timestamp's classification.
    Explain {
        #[arg(long)]
        at: String,
    },
    /// Display the effective rule tree.
    Tree {
        #[arg(long)]
        kind: Option<String>,
    },
}

impl CliCommand {
    fn name(&self) -> &'static str {
        match self {
            Self::Dump { .. } => "dump",
            Self::Status => "status",
            Self::GenData { .. } => "gen-data",
            Self::BenchQuery { .. } => "bench-query",
            Self::RestoreBackup { .. } => "restore-backup",
            Self::Rules(rules) => match rules {
                RulesCommand::Check { .. } => "rules check",
                RulesCommand::Preview { .. } => "rules preview",
                RulesCommand::Diff { .. } => "rules diff",
                RulesCommand::Explain { .. } => "rules explain",
                RulesCommand::Tree { .. } => "rules tree",
            },
        }
    }
}

#[derive(Debug, Snafu)]
#[snafu(display("{}", quartz_i18n::not_implemented(operation)))]
struct DispatchCommandError {
    operation: &'static str,
}

fn dispatch(command: CliCommand) -> Result<(), DispatchCommandError> {
    DispatchCommandSnafu {
        operation: command.name(),
    }
    .fail()
}

fn main() -> ExitCode {
    let args = match Args::try_parse() {
        Ok(args) => args,
        Err(error) => {
            platform::attach_parent_console();
            error.exit();
        }
    };
    match args.command {
        #[cfg(windows)]
        None => platform::run_core(),
        #[cfg(not(windows))]
        None => ExitCode::FAILURE,
        #[cfg(windows)]
        Some(Command::Ui { prewarm }) => {
            if let Err(error) = logging::init_ui() {
                // A log-directory failure cannot be reported to ui.log itself.
                platform::attach_parent_console();
                eprintln!("could not initialize UI diagnostics: {error}");
                return ExitCode::FAILURE;
            }
            let options = if prewarm {
                quartz_ui::RunOptions::Prewarm
            } else {
                quartz_ui::RunOptions::Open {
                    language: platform::ui_language(),
                }
            };
            run_ui(options)
        }
        #[cfg(not(windows))]
        Some(Command::Ui { .. }) => ExitCode::FAILURE,
        Some(Command::Cli(command)) => {
            platform::attach_parent_console();
            if let Err(error) = logging::init() {
                eprintln!("could not initialize entry diagnostics: {error}");
                return ExitCode::FAILURE;
            }
            match dispatch(command) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    tracing::debug!(target: "quartz::entry", error = %error, "command failed");
                    eprintln!("{error}");
                    ExitCode::FAILURE
                }
            }
        }
    }
}

#[cfg(windows)]
fn run_ui(options: quartz_ui::RunOptions) -> ExitCode {
    tracing::info!(?options, "UI started");
    match quartz_ui::run(options) {
        Ok(()) => {
            tracing::info!("UI stopped");
            ExitCode::SUCCESS
        }
        Err(error) => {
            tracing::error!(?error, "UI failed");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::error::ErrorKind;

    use super::*;

    #[test]
    fn unknown_arguments_do_not_start_core() {
        for args in [
            vec!["--unknown"],
            vec!["--bogus"],
            vec!["unknown"],
            vec!["--parent-window=123"],
            vec!["native-host"],
            vec!["native-host", "--help"],
            vec!["chrome-extension://x/"],
            vec!["chrome-extension://x/", "--parent-window=1"],
        ] {
            let error = Args::try_parse_from(std::iter::once("quartz").chain(args.iter().copied()))
                .expect_err("unknown arguments must not start the core");
            assert_eq!(error.exit_code(), 2, "{args:?}");
        }
    }

    #[test]
    fn unknown_flag_retains_clap_diagnostic_and_exit_code() {
        let error = Args::try_parse_from(["quartz", "--unknown"])
            .expect_err("unknown flag must fail to parse");
        assert_eq!(
            error.kind(),
            ErrorKind::UnknownArgument,
            "unknown flag diagnostic"
        );
        assert_eq!(error.exit_code(), 2, "parse failure exit code");
        assert!(
            error.to_string().contains("--unknown"),
            "diagnostic identifies the rejected flag"
        );
    }

    #[test]
    fn internal_commands_and_autostart() {
        for (args, expected) in [
            (
                vec![],
                Args {
                    autostart: false,
                    command: None,
                },
            ),
            (
                vec!["--autostart"],
                Args {
                    autostart: true,
                    command: None,
                },
            ),
            (
                vec!["ui"],
                Args {
                    autostart: false,
                    command: Some(Command::Ui { prewarm: false }),
                },
            ),
            (
                vec!["ui", "--prewarm"],
                Args {
                    autostart: false,
                    command: Some(Command::Ui { prewarm: true }),
                },
            ),
            (
                vec!["--autostart", "ui", "--prewarm"],
                Args {
                    autostart: true,
                    command: Some(Command::Ui { prewarm: true }),
                },
            ),
        ] {
            assert_eq!(
                Args::try_parse_from(std::iter::once("quartz").chain(args.iter().copied()))
                    .expect("valid command arguments"),
                expected,
                "{args:?}"
            );
        }
    }

    #[test]
    fn user_commands_retain_arguments() {
        for (args, expected) in [
            (
                vec!["dump", "--date", "2026-11-16"],
                CliCommand::Dump {
                    date: "2026-11-16".into(),
                },
            ),
            (vec!["status"], CliCommand::Status),
            (
                vec!["gen-data", "--db", "simulation.db"],
                CliCommand::GenData {
                    db: "simulation.db".into(),
                },
            ),
            (
                vec!["bench-query", "--db", "simulation.db"],
                CliCommand::BenchQuery {
                    db: "simulation.db".into(),
                },
            ),
            (
                vec!["restore-backup", "backup.db"],
                CliCommand::RestoreBackup {
                    file: "backup.db".into(),
                },
            ),
        ] {
            let parsed =
                Args::try_parse_from(std::iter::once("quartz").chain(args.iter().copied()))
                    .expect("valid command arguments");
            assert_eq!(parsed.command, Some(Command::Cli(expected)), "{args:?}");
            let Some(Command::Cli(command)) = parsed.command else {
                panic!("expected CLI command for {args:?}");
            };
            assert!(
                matches!(dispatch(command), Err(DispatchCommandError { operation }) if operation == args[0]),
                "dispatch operation for {args:?}"
            );
        }
    }

    #[test]
    fn every_rules_command_parses_and_reports_not_implemented() {
        for (args, expected, name) in [
            (
                vec!["check", "a.yml", "b.yml"],
                RulesCommand::Check {
                    files: vec!["a.yml".into(), "b.yml".into()],
                },
                "rules check",
            ),
            (
                vec!["preview", "a.yml"],
                RulesCommand::Preview {
                    file: "a.yml".into(),
                    days: 7,
                },
                "rules preview",
            ),
            (
                vec!["diff", "a.yml"],
                RulesCommand::Diff {
                    file: "a.yml".into(),
                    days: 7,
                },
                "rules diff",
            ),
            (
                vec!["explain", "--at", "2026-11-16T12:00:00Z"],
                RulesCommand::Explain {
                    at: "2026-11-16T12:00:00Z".into(),
                },
                "rules explain",
            ),
            (
                vec!["tree"],
                RulesCommand::Tree { kind: None },
                "rules tree",
            ),
        ] {
            let args = [&["rules"][..], &args].concat();
            let parsed =
                Args::try_parse_from(std::iter::once("quartz").chain(args.iter().copied()))
                    .expect("valid command arguments");
            assert_eq!(
                parsed.command,
                Some(Command::Cli(CliCommand::Rules(expected))),
                "{args:?}"
            );
            let Some(Command::Cli(command)) = parsed.command else {
                panic!("expected rules command for {args:?}");
            };
            assert!(
                matches!(dispatch(command), Err(DispatchCommandError { operation }) if operation == name),
                "dispatch operation for {args:?}"
            );
        }
    }

    #[test]
    fn rules_optional_arguments() {
        for (args, expected) in [
            (vec!["check"], RulesCommand::Check { files: vec![] }),
            (
                vec!["preview", "a.yml", "--days", "14"],
                RulesCommand::Preview {
                    file: "a.yml".into(),
                    days: 14,
                },
            ),
            (
                vec!["diff", "a.yml", "--days", "3"],
                RulesCommand::Diff {
                    file: "a.yml".into(),
                    days: 3,
                },
            ),
            (
                vec!["tree", "--kind", "privacy"],
                RulesCommand::Tree {
                    kind: Some("privacy".into()),
                },
            ),
        ] {
            let args = [&["rules"][..], &args].concat();
            assert_eq!(
                Args::try_parse_from(std::iter::once("quartz").chain(args.iter().copied()))
                    .expect("valid command arguments")
                    .command,
                Some(Command::Cli(CliCommand::Rules(expected))),
                "rules arguments: {args:?}"
            );
        }
    }

    #[test]
    fn missing_or_invalid_values_are_errors() {
        for args in [
            vec!["dump"],
            vec!["gen-data"],
            vec!["bench-query"],
            vec!["restore-backup"],
            vec!["rules"],
            vec!["rules", "preview"],
            vec!["rules", "diff"],
            vec!["rules", "explain"],
            vec!["rules", "tree", "--unknown"],
            vec!["rules", "preview", "a.yml", "--days", "invalid"],
        ] {
            assert!(
                Args::try_parse_from(std::iter::once("quartz").chain(args.iter().copied()))
                    .is_err(),
                "{args:?}"
            );
        }
    }

    #[test]
    fn help_and_version_are_structured_clap_results() {
        for (arg, kind) in [
            ("--help", ErrorKind::DisplayHelp),
            ("--version", ErrorKind::DisplayVersion),
        ] {
            let Err(source) = Args::try_parse_from(["quartz", arg]) else {
                panic!("expected clap output for {arg}");
            };
            assert_eq!(source.kind(), kind, "clap diagnostic for {arg}");
            assert_eq!(source.exit_code(), 0, "exit code for {arg}");
            if arg == "--help" {
                let help = source.to_string();
                assert!(
                    !help.contains("native-host"),
                    "host command exposed in help: {help}"
                );
                assert!(!help.contains("prewarm"), "prewarm exposed in help: {help}");
                assert!(!help.contains("  ui"), "UI command exposed in help: {help}");
                assert!(
                    help.contains("rules"),
                    "rules command absent from help: {help}"
                );
            }
        }
    }

    #[test]
    fn core_and_ui_launch_forms_parse_without_cli_commands() {
        for args in [
            vec![],
            vec!["--autostart"],
            vec!["ui"],
            vec!["--autostart", "ui"],
            vec!["ui", "--prewarm"],
            vec!["ui", "--"],
            vec!["ui", "--prewarm", "--"],
            vec!["--"],
            vec!["--autostart", "--"],
        ] {
            let parsed =
                Args::try_parse_from(std::iter::once("quartz").chain(args.iter().copied()))
                    .expect("valid background launch");
            assert!(!matches!(parsed.command, Some(Command::Cli(_))), "{args:?}");
        }
    }

    #[test]
    fn ui_help_and_invalid_flags_produce_clap_diagnostics() {
        let suffixes: &[(&[&str], Option<ErrorKind>)] = &[
            (&[], None),
            (&["--"], None),
            (&["--prewarm"], None),
            (&["--prewarm", "--"], None),
            (&["--help"], Some(ErrorKind::DisplayHelp)),
            (&["-h"], Some(ErrorKind::DisplayHelp)),
            (&["--version"], Some(ErrorKind::UnknownArgument)),
            (&["--unknown"], Some(ErrorKind::UnknownArgument)),
            (&["--autostart"], Some(ErrorKind::UnknownArgument)),
            (
                &["--prewarm", "--prewarm"],
                Some(ErrorKind::ArgumentConflict),
            ),
            (&["--prewarm", "--help"], Some(ErrorKind::DisplayHelp)),
            (&["--", "--help"], Some(ErrorKind::UnknownArgument)),
            (&["--prewarm=true"], Some(ErrorKind::TooManyValues)),
        ];
        for autostart_count in 0..=2 {
            for (suffix, expected) in suffixes {
                let args: Vec<_> = std::iter::repeat_n("--autostart", autostart_count)
                    .chain(Some("ui"))
                    .chain(suffix.iter().copied())
                    .collect();
                let result =
                    Args::try_parse_from(std::iter::once("quartz").chain(args.iter().copied()));
                if autostart_count == 2 {
                    assert_eq!(
                        result.expect_err("duplicate autostart must fail").kind(),
                        ErrorKind::ArgumentConflict,
                        "{args:?}"
                    );
                } else if let Some(kind) = expected {
                    let error = result.expect_err("expected clap help or error");
                    assert_eq!(error.kind(), *kind, "{args:?}");
                    let exit_code = if *kind == ErrorKind::DisplayHelp {
                        0
                    } else {
                        2
                    };
                    assert_eq!(error.exit_code(), exit_code, "{args:?}");
                } else {
                    assert!(
                        matches!(
                            result.expect("valid UI flags").command,
                            Some(Command::Ui { .. })
                        ),
                        "{args:?}"
                    );
                }
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn non_unicode_arguments_return_an_error() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};

        assert!(
            Args::try_parse_from([OsString::from("quartz"), OsString::from_vec(vec![0xff])])
                .is_err(),
            "non-Unicode argument must be rejected"
        );
    }

    #[cfg(windows)]
    #[test]
    fn non_unicode_arguments_return_an_error() {
        use std::{ffi::OsString, os::windows::ffi::OsStringExt};

        assert!(
            Args::try_parse_from([OsString::from("quartz"), OsString::from_wide(&[0xd800]),])
                .is_err(),
            "non-Unicode argument must be rejected"
        );
    }
}
