//! Quartz process entry point.

#![cfg_attr(windows, windows_subsystem = "windows")]

mod bench_query;
#[cfg(windows)]
mod commands;
#[cfg(any(windows, test))]
mod core;
mod data_directory;
mod dump;
mod generate_data;
#[cfg(any(windows, test))]
mod ipc;
mod logging;
#[cfg(any(windows, test))]
mod persistence;
mod platform;
mod query;
mod rules;
mod rules_check;
mod simulation;
#[cfg_attr(
    not(windows),
    expect(
        dead_code,
        reason = "core storage and recovery run on Windows; offline rule reads are cross-platform"
    )
)]
mod store;

#[cfg(any(windows, test))]
mod instance_name;
#[cfg(any(windows, test))]
mod tray_tooltip;
#[cfg(any(windows, test))]
mod ui_state;

use std::{path::PathBuf, process::ExitCode};

use clap::{Parser, Subcommand};
#[cfg(windows)]
use snafu::ResultExt;
use snafu::Snafu;

#[cfg(windows)]
#[derive(Debug, Clone, Copy)]
enum UiLaunch {
    Open,
    Prewarm,
}
#[cfg(windows)]
impl From<bool> for UiLaunch {
    fn from(prewarm: bool) -> Self {
        if prewarm { Self::Prewarm } else { Self::Open }
    }
}

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
        #[arg(long, conflicts_with = "core_error")]
        prewarm: bool,
        #[arg(long)]
        core_error: Option<quartz_protocol::CoreErrorCode>,
        #[arg(
            long,
            requires = "core_error",
            required_if_eq("core_error", "migration_failed")
        )]
        backup_path: Vec<PathBuf>,
    },
    #[command(flatten)]
    Cli(CliCommand),
}

#[derive(Debug, PartialEq, Eq, Subcommand, strum::IntoStaticStr)]
#[strum(serialize_all = "kebab-case")]
enum CliCommand {
    /// Print intervals for a date.
    Dump {
        #[arg(long)]
        date: String,
        #[arg(long, value_enum, default_value_t = dump::DumpFormat::Text)]
        format: dump::DumpFormat,
    },
    /// Show core status.
    Status,
    /// Generate a year of data in an independent simulation database.
    GenData {
        #[arg(long)]
        db: PathBuf,
        #[arg(long, value_enum, default_value_t = generate_data::Profile::Typical)]
        profile: generate_data::Profile,
        #[arg(long, default_value_t = generate_data::DEFAULT_SEED)]
        seed: u64,
        #[arg(long, default_value_t = generate_data::DEFAULT_DAYS, value_parser = clap::value_parser!(u32).range(1..))]
        days: u32,
    },
    /// Benchmark queries against a simulation database.
    BenchQuery {
        #[arg(long)]
        db: PathBuf,
        #[arg(long, default_value_t = bench_query::DEFAULT_RUNS, value_parser = clap::value_parser!(u32).range(1..))]
        runs: u32,
    },
    /// Restore a database backup while the core is stopped.
    RestoreBackup {
        file: PathBuf,
        /// Replace the current database and discard records written after this backup.
        #[arg(long)]
        yes: bool,
    },
    /// Validate and inspect rules.
    #[command(subcommand)]
    Rules(RulesCommand),
}

#[derive(Debug, PartialEq, Eq, Subcommand, strum::IntoStaticStr)]
#[strum(serialize_all = "kebab-case")]
enum RulesCommand {
    /// Check rule files and the installed rule tree offline.
    Check {
        #[arg(required = true, num_args = 1..)]
        files: Vec<PathBuf>,
    },
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

impl std::fmt::Display for CliCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name: &'static str = self.into();
        formatter.write_str(name)?;
        if let Self::Rules(rules) = self {
            let name: &'static str = rules.into();
            write!(formatter, " {name}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Snafu)]
enum DispatchCommandError {
    #[cfg(windows)]
    #[snafu(display("{source}"))]
    Status { source: ipc::client::CallCoreError },
    #[snafu(display("{source}"))]
    Dump { source: dump::DumpDayError },
    #[snafu(display("{source}"))]
    Generate {
        source: generate_data::GenerateDataError,
    },
    #[snafu(display("{source}"))]
    Bench {
        source: bench_query::BenchQueryError,
    },
    #[snafu(display("{source}"))]
    CheckRules {
        source: rules_check::CheckRulesError,
    },
    #[snafu(display("not implemented yet: {operation}"))]
    NotImplemented { operation: CliCommand },
    #[cfg(windows)]
    #[snafu(display("could not restore database backup: {source}"))]
    Restore {
        source: commands::RestoreBackupCommandError,
    },
}

impl CliCommand {
    fn dispatch(self) -> Result<(), DispatchCommandError> {
        use snafu::ResultExt as _;
        let command = match self {
            #[cfg(windows)]
            Self::Status => {
                return ipc::client::print_status(
                    platform::windows::ipc::CoreClient::connect().context(StatusSnafu)?,
                )
                .context(StatusSnafu);
            }
            Self::Dump { date, format } => {
                return dump::DumpDay::new(&date, format).run().context(DumpSnafu);
            }
            Self::GenData {
                db,
                profile,
                seed,
                days,
            } => {
                return generate_data::GenerateData::new(&db, profile, seed, days)
                    .run()
                    .context(GenerateSnafu);
            }
            Self::BenchQuery { db, runs } => {
                return bench_query::BenchQuery::new(&db, runs)
                    .run()
                    .context(BenchSnafu);
            }
            command => command,
        };
        if let CliCommand::Rules(RulesCommand::Check { files }) = &command {
            use snafu::ResultExt as _;
            return rules_check::CheckRulesCommand::new(files)
                .run()
                .context(CheckRulesSnafu);
        }
        #[cfg(windows)]
        if let CliCommand::RestoreBackup { file, yes } = &command {
            use snafu::ResultExt as _;
            let confirmation = commands::RestoreConfirmation::from(*yes);
            return commands::RestoreBackupCommand::new(file, confirmation)
                .run()
                .context(RestoreSnafu);
        }
        NotImplementedSnafu { operation: command }.fail()
    }
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
        None => platform::CoreProcess::new().run(core::app::Autostart::from(args.autostart)),
        #[cfg(not(windows))]
        None => ExitCode::FAILURE,
        #[cfg(windows)]
        Some(Command::Ui {
            prewarm,
            core_error,
            backup_path,
        }) => {
            let core_error = match init_ui_diagnostics() {
                Ok(()) => core_error,
                Err(error) => {
                    tracing::error!(?error, "could not initialize UI diagnostics");
                    core_error.or(Some(quartz_protocol::CoreErrorCode::CoreStartFailed))
                }
            };
            let options = if let Some(code) = core_error {
                quartz_ui::RunOptions::CoreError {
                    language: platform::detect_ui_language().code(),
                    error: quartz_protocol::CoreError::new(code, backup_path),
                }
            } else {
                match UiLaunch::from(prewarm) {
                    UiLaunch::Prewarm => quartz_ui::RunOptions::Prewarm,
                    UiLaunch::Open => quartz_ui::RunOptions::Open {
                        language: platform::detect_ui_language().code(),
                    },
                }
            };
            LaunchUiCommand::new(options).run()
        }
        #[cfg(not(windows))]
        Some(Command::Ui { .. }) => ExitCode::FAILURE,
        Some(Command::Cli(command)) => {
            platform::attach_parent_console();
            if let Err(error) = logging::init() {
                eprintln!("could not initialize entry diagnostics: {error}");
                return ExitCode::FAILURE;
            }
            match command.dispatch() {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    tracing::debug!(error = %error, "command failed");
                    eprintln!("{error}");
                    ExitCode::FAILURE
                }
            }
        }
    }
}

#[cfg(windows)]
#[derive(Debug, Snafu)]
enum StartUiError {
    #[snafu(display("could not resolve UI data directory"))]
    UiDataDirectory {
        source: data_directory::ResolveDataDirectoryError,
    },
    #[snafu(display("{source}"))]
    UiInitLog { source: logging::InitFileLogError },
}

#[cfg(windows)]
fn init_ui_diagnostics() -> Result<(), StartUiError> {
    let directory = data_directory::DataDirectory::resolve().context(UiDataDirectorySnafu)?;
    directory
        .init_file_log(logging::LogTarget::Ui)
        .context(UiInitLogSnafu)
}

#[cfg(windows)]
struct LaunchUiCommand {
    options: quartz_ui::RunOptions,
}
#[cfg(windows)]
impl LaunchUiCommand {
    fn new(options: quartz_ui::RunOptions) -> Self {
        Self { options }
    }
    fn run(self) -> ExitCode {
        let options = self.options;
        tracing::info!(?options, "UI started");
        match options.run() {
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
}

#[cfg(test)]
mod tests {
    use clap::error::ErrorKind;

    use super::*;

    #[test]
    fn unsupported_command_error_keeps_the_subcommand_and_display_name() {
        let error = CliCommand::Rules(RulesCommand::Preview {
            file: "private-rules.toml".into(),
            days: 14,
        })
        .dispatch()
        .expect_err("preview remains unimplemented");
        assert_eq!(error.to_string(), "not implemented yet: rules preview");
        let DispatchCommandError::NotImplemented { operation } = error else {
            panic!("expected unsupported command error");
        };
        assert_eq!(
            operation,
            CliCommand::Rules(RulesCommand::Preview {
                file: "private-rules.toml".into(),
                days: 14,
            })
        );
    }

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
                    command: Some(Command::Ui {
                        prewarm: false,
                        core_error: None,
                        backup_path: vec![],
                    }),
                },
            ),
            (
                vec!["ui", "--prewarm"],
                Args {
                    autostart: false,
                    command: Some(Command::Ui {
                        prewarm: true,
                        core_error: None,
                        backup_path: vec![],
                    }),
                },
            ),
            (
                vec!["--autostart", "ui", "--prewarm"],
                Args {
                    autostart: true,
                    command: Some(Command::Ui {
                        prewarm: true,
                        core_error: None,
                        backup_path: vec![],
                    }),
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
                    format: dump::DumpFormat::Text,
                },
            ),
            (vec!["status"], CliCommand::Status),
            (
                vec!["gen-data", "--db", "simulation.db"],
                CliCommand::GenData {
                    db: "simulation.db".into(),
                    profile: generate_data::Profile::Typical,
                    seed: generate_data::DEFAULT_SEED,
                    days: generate_data::DEFAULT_DAYS,
                },
            ),
            (
                vec!["bench-query", "--db", "simulation.db"],
                CliCommand::BenchQuery {
                    db: "simulation.db".into(),
                    runs: bench_query::DEFAULT_RUNS,
                },
            ),
            (
                vec!["restore-backup", "backup.db"],
                CliCommand::RestoreBackup {
                    file: "backup.db".into(),
                    yes: false,
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
            if matches!(
                command,
                CliCommand::RestoreBackup { .. }
                    | CliCommand::Dump { .. }
                    | CliCommand::GenData { .. }
                    | CliCommand::BenchQuery { .. }
            ) {
                continue;
            }
            if matches!(command, CliCommand::Rules(RulesCommand::Check { .. })) {
                continue;
            }
            if matches!(command, CliCommand::Status) {
                continue;
            }
            assert!(
                matches!(command.dispatch(), Err(DispatchCommandError::NotImplemented { operation }) if operation.to_string() == args[0]),
                "dispatch operation for {args:?}"
            );
        }
    }

    #[test]
    fn restore_confirmation_is_an_explicit_flag() {
        for args in [
            vec!["quartz", "restore-backup", "backup.db", "--yes"],
            vec!["quartz", "restore-backup", "--yes", "backup.db"],
        ] {
            assert_eq!(
                Args::try_parse_from(args)
                    .expect("confirmed restore arguments")
                    .command,
                Some(Command::Cli(CliCommand::RestoreBackup {
                    file: "backup.db".into(),
                    yes: true
                }))
            );
        }
        for args in [
            vec!["quartz", "restore-backup", "--yes"],
            vec!["quartz", "restore-backup", "backup.db", "yes"],
            vec!["quartz", "restore-backup", "backup.db", "--yes=false"],
        ] {
            assert!(
                Args::try_parse_from(args).is_err(),
                "invalid confirmation syntax"
            );
        }
        let help =
            Args::try_parse_from(["quartz", "restore-backup", "--help"]).expect_err("restore help");
        assert_eq!(help.exit_code(), 0);
        assert!(help.to_string().contains("--yes"));
    }

    #[test]
    fn startup_error_arguments_are_typed_and_recovery_path_is_required() {
        let args = Args::try_parse_from([
            "quartz",
            "ui",
            "--core-error",
            "migration_failed",
            "--backup-path",
            "C:/数据/backup with spaces.db",
        ])
        .expect("structured failure arguments");
        assert!(
            matches!(args.command,Some(Command::Ui { core_error: Some(quartz_protocol::CoreErrorCode::MigrationFailed), backup_path, .. }) if backup_path == [PathBuf::from("C:/数据/backup with spaces.db")])
        );
        for args in [
            vec!["quartz", "ui", "--core-error", "unknown"],
            vec!["quartz", "ui", "--core-error", "migration_failed"],
            vec!["quartz", "ui", "--backup-path", "backup.db"],
            vec!["quartz", "ui", "--core-error", "other_session", "--prewarm"],
        ] {
            assert!(Args::try_parse_from(args).is_err());
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
            if matches!(command, CliCommand::Rules(RulesCommand::Check { .. })) {
                continue;
            }
            assert!(
                matches!(command.dispatch(), Err(DispatchCommandError::NotImplemented { operation }) if operation.to_string() == name),
                "dispatch operation for {args:?}"
            );
        }
    }

    #[test]
    fn rules_optional_arguments() {
        for (args, expected) in [
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
    fn acceptance_commands_parse_typed_options_and_reject_invalid_values() {
        let parsed = Args::try_parse_from([
            "quartz",
            "gen-data",
            "--db",
            "simulation.db",
            "--profile",
            "heavy",
            "--seed",
            "0",
            "--days",
            "3",
        ])
        .expect("typed generator options");
        assert!(matches!(
            parsed.command,
            Some(Command::Cli(CliCommand::GenData {
                profile: generate_data::Profile::Heavy,
                seed: 0,
                days: 3,
                ..
            }))
        ));
        let parsed =
            Args::try_parse_from(["quartz", "dump", "--date", "2025-01-02", "--format", "json"])
                .expect("JSON dump option");
        assert!(matches!(
            parsed.command,
            Some(Command::Cli(CliCommand::Dump {
                format: dump::DumpFormat::Json,
                ..
            }))
        ));
        for args in [
            vec!["gen-data", "--db", "simulation.db", "--profile", "invalid"],
            vec!["gen-data", "--db", "simulation.db", "--days", "0"],
            vec!["bench-query", "--db", "simulation.db", "--runs", "0"],
            vec!["dump", "--date", "2025-01-02", "--format", "yaml"],
        ] {
            assert_eq!(
                Args::try_parse_from(std::iter::once("quartz").chain(args))
                    .expect_err("invalid acceptance options")
                    .exit_code(),
                2
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
            vec!["rules", "check"],
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
        struct UiFlagCase<'arguments> {
            arguments: &'arguments [&'arguments str],
            expected: Option<ErrorKind>,
        }
        let suffixes = [
            UiFlagCase {
                arguments: &[],
                expected: None,
            },
            UiFlagCase {
                arguments: &["--"],
                expected: None,
            },
            UiFlagCase {
                arguments: &["--prewarm"],
                expected: None,
            },
            UiFlagCase {
                arguments: &["--prewarm", "--"],
                expected: None,
            },
            UiFlagCase {
                arguments: &["--help"],
                expected: Some(ErrorKind::DisplayHelp),
            },
            UiFlagCase {
                arguments: &["-h"],
                expected: Some(ErrorKind::DisplayHelp),
            },
            UiFlagCase {
                arguments: &["--version"],
                expected: Some(ErrorKind::UnknownArgument),
            },
            UiFlagCase {
                arguments: &["--unknown"],
                expected: Some(ErrorKind::UnknownArgument),
            },
            UiFlagCase {
                arguments: &["--autostart"],
                expected: Some(ErrorKind::UnknownArgument),
            },
            UiFlagCase {
                arguments: &["--prewarm", "--prewarm"],
                expected: Some(ErrorKind::ArgumentConflict),
            },
            UiFlagCase {
                arguments: &["--prewarm", "--help"],
                expected: Some(ErrorKind::DisplayHelp),
            },
            UiFlagCase {
                arguments: &["--", "--help"],
                expected: Some(ErrorKind::UnknownArgument),
            },
            UiFlagCase {
                arguments: &["--prewarm=true"],
                expected: Some(ErrorKind::TooManyValues),
            },
        ];
        for autostart_count in 0..=2 {
            for UiFlagCase {
                arguments: suffix,
                expected,
            } in &suffixes
            {
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
