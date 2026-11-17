//! Browser launch detection, command parsing, and process dispatch.

use std::{ffi::OsString, path::PathBuf, process::ExitCode};

use clap::{Parser, Subcommand};
use snafu::Snafu;

use crate::{logging, platform};

const FIREFOX_EXTENSION_ID: &str = "quartz@quartz.dev";

#[derive(Debug, PartialEq, Eq)]
enum Mode {
    Core { autostart: bool },
    Ui { prewarm: bool },
    NativeHost,
    Cli(CliCommand),
}

#[derive(Debug, Parser)]
#[command(name = "quartz", version)]
struct Args {
    /// Start from the operating system's autostart entry.
    #[arg(long)]
    autostart: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    #[command(hide = true)]
    Ui {
        #[arg(long)]
        prewarm: bool,
    },
    #[command(hide = true)]
    NativeHost,
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
#[snafu(context(false), display("invalid command line arguments"))]
struct ParseArgsError {
    source: clap::Error,
}

#[derive(Debug, Snafu)]
#[snafu(display("{}", quartz_i18n::not_implemented(operation)))]
struct DispatchModeError {
    operation: &'static str,
}

/// Parses arguments without the executable name or process side effects.
///
/// # Errors
/// Returns clap diagnostics for help, version, or invalid arguments.
fn parse(args: &[OsString]) -> Result<Mode, ParseArgsError> {
    if is_browser_launch(args) {
        return Ok(Mode::NativeHost);
    }
    let parsed = Args::try_parse_from(
        std::iter::once(OsString::from("quartz")).chain(args.iter().cloned()),
    )?;
    Ok(match parsed.command {
        None => Mode::Core {
            autostart: parsed.autostart,
        },
        Some(Command::Ui { prewarm }) => Mode::Ui { prewarm },
        Some(Command::NativeHost) => Mode::NativeHost,
        Some(Command::Cli(command)) => Mode::Cli(command),
    })
}

fn is_browser_launch(args: &[OsString]) -> bool {
    if args
        .first()
        .is_some_and(|arg| arg.as_encoded_bytes().starts_with(b"chrome-extension://"))
    {
        return true;
    }
    matches!(args, [manifest, id]
        if PathBuf::from(manifest).is_absolute()
            && manifest.as_encoded_bytes().ends_with(b".json")
            && id == FIREFOX_EXTENSION_ID)
}

fn needs_console(args: &[OsString]) -> bool {
    !is_browser_launch(args)
        && args.iter().any(|arg| arg != "--autostart")
        && !matches!(
            args.iter()
                .find(|arg| *arg != "--autostart")
                .and_then(|arg| arg.to_str()),
            Some("ui" | "native-host")
        )
}

fn dispatch(mode: Mode) -> Result<(), DispatchModeError> {
    let operation = match mode {
        // TODO(M0-B): Connect these modes to core and UI startup.
        Mode::Core { .. } => "core",
        Mode::Ui { .. } => "ui",
        // TODO(M5): Validate registered browser identities and start stdio forwarding.
        Mode::NativeHost => "native-host",
        Mode::Cli(command) => command.name(),
    };
    DispatchModeSnafu { operation }.fail()
}

/// Detects the process mode and reports startup failures with its exit code.
pub(super) fn run(args: Vec<OsString>) -> ExitCode {
    if needs_console(&args) {
        platform::attach_parent_console();
    }
    let mode = match parse(&args) {
        Ok(mode) => mode,
        Err(ParseArgsError { source }) => {
            platform::attach_parent_console();
            let code = if source.use_stderr() { 2 } else { 0 };
            let _ = source.print();
            return ExitCode::from(code);
        }
    };
    let cli = matches!(mode, Mode::Cli(_));
    if cli && let Err(error) = logging::init() {
        eprintln!("could not initialize entry diagnostics: {error}");
        return ExitCode::FAILURE;
    }
    match dispatch(mode) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if cli {
                tracing::debug!(error = %error, "command failed");
                eprintln!("{error}");
            }
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::error::ErrorKind;

    use super::*;

    fn parse_strings(args: &[&str]) -> Result<Mode, ParseArgsError> {
        parse(&args.iter().map(OsString::from).collect::<Vec<_>>())
    }

    #[test]
    fn chrome_launch_precedes_clap() {
        for args in [
            vec!["chrome-extension://example/"],
            vec!["chrome-extension://example/", "--parent-window=123"],
        ] {
            assert_eq!(
                parse_strings(&args).expect("valid command arguments"),
                Mode::NativeHost,
                "Chrome launch arguments: {args:?}"
            );
        }
    }

    #[test]
    fn firefox_requires_absolute_json_path_and_registered_id() {
        let manifest = std::env::current_dir()
            .expect("test working directory")
            .join("host.json");
        assert_eq!(
            parse(&[
                manifest.clone().into_os_string(),
                FIREFOX_EXTENSION_ID.into()
            ])
            .expect("registered Firefox launch"),
            Mode::NativeHost,
            "registered Firefox launch: {manifest:?}"
        );
        for args in [
            vec!["host.json".into(), FIREFOX_EXTENSION_ID.into()],
            vec![
                manifest.clone().into_os_string(),
                "unknown@example.com".into(),
            ],
            vec![manifest.clone().into_os_string()],
            vec![
                manifest.with_extension("txt").into_os_string(),
                FIREFOX_EXTENSION_ID.into(),
            ],
            vec![
                manifest.into_os_string(),
                FIREFOX_EXTENSION_ID.into(),
                "extra".into(),
            ],
        ] {
            assert!(
                matches!(parse(&args), Err(ParseArgsError { .. })),
                "{args:?}"
            );
        }
    }

    #[test]
    fn unknown_arguments_do_not_start_core() {
        for args in [["--unknown"], ["unknown"], ["--parent-window=123"]] {
            assert!(
                matches!(parse_strings(&args), Err(ParseArgsError { .. })),
                "unexpectedly accepted {args:?}"
            );
        }
    }

    #[test]
    fn parse_error_preserves_clap_source_and_summary() {
        let error = parse_strings(&["--unknown"]).expect_err("unknown flag must fail to parse");
        assert_eq!(
            error.to_string(),
            "invalid command line arguments",
            "entry error summary"
        );
        let source = std::error::Error::source(&error)
            .and_then(|source| source.downcast_ref::<clap::Error>())
            .expect("parse error must retain its clap source");
        assert_eq!(
            source.kind(),
            ErrorKind::UnknownArgument,
            "unknown flag diagnostic"
        );
        assert_eq!(source.exit_code(), 2, "parse failure exit code");
    }

    #[test]
    fn internal_modes_and_autostart() {
        for (args, expected) in [
            (vec![], Mode::Core { autostart: false }),
            (vec!["--autostart"], Mode::Core { autostart: true }),
            (vec!["ui"], Mode::Ui { prewarm: false }),
            (vec!["ui", "--prewarm"], Mode::Ui { prewarm: true }),
            (vec!["native-host"], Mode::NativeHost),
        ] {
            assert_eq!(
                parse_strings(&args).expect("valid command arguments"),
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
            let mode = parse_strings(&args).expect("valid command arguments");
            assert_eq!(mode, Mode::Cli(expected), "{args:?}");
            assert!(
                matches!(dispatch(mode), Err(DispatchModeError { operation }) if operation == args[0]),
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
            let mode = parse_strings(&args).expect("valid command arguments");
            assert_eq!(mode, Mode::Cli(CliCommand::Rules(expected)), "{args:?}");
            assert!(
                matches!(dispatch(mode), Err(DispatchModeError { operation }) if operation == name),
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
                parse_strings(&args).expect("valid command arguments"),
                Mode::Cli(CliCommand::Rules(expected)),
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
                matches!(parse_strings(&args), Err(ParseArgsError { .. })),
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
            let Err(ParseArgsError { source }) = parse_strings(&[arg]) else {
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
    fn console_attachment_excludes_browser_and_background_modes() {
        for args in [
            vec![],
            vec!["--autostart"],
            vec!["ui"],
            vec!["--autostart", "ui"],
            vec!["ui", "--prewarm"],
            vec!["native-host"],
            vec!["chrome-extension://example/", "--parent-window=0"],
        ] {
            assert!(
                !needs_console(&args.iter().map(OsString::from).collect::<Vec<_>>()),
                "background launch requested a console: {args:?}"
            );
        }
        for args in [
            vec!["status"],
            vec!["--help"],
            vec!["--version"],
            vec!["--unknown"],
        ] {
            assert!(
                needs_console(&args.iter().map(OsString::from).collect::<Vec<_>>()),
                "CLI arguments did not request a console: {args:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn non_unicode_arguments_return_an_error() {
        use std::os::unix::ffi::OsStringExt;

        assert!(
            matches!(
                parse(&[OsString::from_vec(vec![0xff])]),
                Err(ParseArgsError { .. })
            ),
            "non-Unicode argument must be rejected"
        );
    }

    #[cfg(unix)]
    #[test]
    fn firefox_manifest_can_contain_non_unicode_path_bytes() {
        use std::os::unix::ffi::OsStringExt;

        let manifest = OsString::from_vec(b"/tmp/\xff.json".to_vec());
        assert_eq!(
            parse(&[manifest, FIREFOX_EXTENSION_ID.into()]).expect("raw Firefox manifest path"),
            Mode::NativeHost,
            "Firefox manifest with a non-Unicode path byte"
        );
    }

    #[cfg(windows)]
    #[test]
    fn firefox_manifest_can_contain_unpaired_surrogates() {
        use std::os::windows::ffi::OsStringExt;

        let manifest =
            OsString::from_wide(&[0x43, 0x3a, 0x5c, 0xd800, 0x2e, 0x6a, 0x73, 0x6f, 0x6e]);
        assert_eq!(
            parse(&[manifest, FIREFOX_EXTENSION_ID.into()]).expect("raw Firefox manifest path"),
            Mode::NativeHost,
            "Firefox manifest with an unpaired surrogate"
        );
    }
}
