//! Entry-layer tracing setup.

#[cfg(any(windows, test))]
use crate::data_directory::DataDirectory;
use snafu::Snafu;
use tracing_subscriber::EnvFilter;

#[cfg(any(windows, test))]
use snafu::ResultExt;
#[cfg(any(windows, test))]
use std::{
    fs::{self, File, OpenOptions},
    path::PathBuf,
    sync::Mutex,
};

#[cfg(any(windows, test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LogTarget {
    Core,
    Ui,
}

#[cfg(any(windows, test))]
impl LogTarget {
    fn file_name(self) -> &'static str {
        match self {
            Self::Core => "core.log",
            Self::Ui => "ui.log",
        }
    }
}

#[cfg(any(windows, test))]
impl std::fmt::Display for LogTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Core => "core",
            Self::Ui => "UI",
        })
    }
}

#[cfg(any(windows, test))]
#[derive(Debug, Snafu)]
pub(crate) enum InitFileLogError {
    #[snafu(display("could not create {target} log directory {} (create_dir_all)", path.display()))]
    CreateDirectory {
        target: LogTarget,
        path: PathBuf,
        source: std::io::Error,
    },
    #[snafu(display("could not open {target} log file {} (OpenOptions::open)", path.display()))]
    OpenFile {
        target: LogTarget,
        path: PathBuf,
        source: std::io::Error,
    },
    #[snafu(display("could not initialize {target} tracing"))]
    InstallSubscriber {
        target: LogTarget,
        source: InitTracingError,
    },
}

#[cfg(any(windows, test))]
impl DataDirectory {
    /// Opens the target's append-only log and installs a synchronous subscriber.
    pub(crate) fn init_file_log(&self, target: LogTarget) -> Result<(), InitFileLogError> {
        let file = self.open_file_log(target)?;
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(filter())
            .with_ansi(false)
            .with_writer(Mutex::new(file))
            .finish();
        tracing::subscriber::set_global_default(subscriber)
            .map_err(InitTracingError::from)
            .context(InstallSubscriberSnafu { target })
    }
}

#[cfg(any(windows, test))]
impl DataDirectory {
    fn open_file_log(&self, target: LogTarget) -> Result<File, InitFileLogError> {
        let logs = self.logs();
        fs::create_dir_all(&logs).context(CreateDirectorySnafu {
            target,
            path: &logs,
        })?;
        let path = logs.join(target.file_name());
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .context(OpenFileSnafu { target, path })
    }
}

fn filter() -> EnvFilter {
    EnvFilter::builder()
        .with_regex(false)
        .with_default_directive(tracing::Level::INFO.into())
        .from_env_lossy()
}

/// Failure to install the process-wide tracing subscriber.
#[derive(Debug, Snafu)]
#[snafu(context(false), display("could not install tracing subscriber"))]
pub(crate) struct InitTracingError {
    source: tracing::subscriber::SetGlobalDefaultError,
}

/// Installs stderr diagnostics for commands that have a console.
///
/// # Errors
/// Returns an error if a global tracing subscriber is already installed.
pub(super) fn init() -> Result<(), InitTracingError> {
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(filter())
        .with_writer(std::io::stderr)
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;
    Ok(())
}

/// Logs before unwinding or aborting without including potentially sensitive panic payloads.
#[cfg(any(windows, test))]
pub(crate) fn install_core_panic_log() {
    std::panic::set_hook(Box::new(|info| {
        let thread = std::thread::current();
        tracing::error!(thread = thread.name(), location = ?info.location(), "core thread panicked");
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{error::Error as _, io::Write as _};

    #[test]
    fn repeated_initialization_preserves_subscriber_cause() {
        init().expect("first tracing initialization must succeed");
        let error = init().expect_err("second tracing initialization must fail");
        assert_eq!(error.to_string(), "could not install tracing subscriber");
        assert!(
            error
                .source()
                .and_then(
                    |source| source.downcast_ref::<tracing::subscriber::SetGlobalDefaultError>()
                )
                .is_some()
        );
        let directory = tempfile::tempdir().expect("log data directory");
        for target in [LogTarget::Core, LogTarget::Ui] {
            let error = DataDirectory::new(directory.path())
                .init_file_log(target)
                .expect_err("subscriber is already installed");
            let InitFileLogError::InstallSubscriber { source, .. } = error else {
                panic!("expected subscriber installation failure: {error}");
            };
            assert!(
                source
                    .source()
                    .and_then(|source| source
                        .downcast_ref::<tracing::subscriber::SetGlobalDefaultError>())
                    .is_some()
            );
        }
    }

    #[test]
    fn both_targets_append_to_their_own_log_files() {
        let directory = tempfile::tempdir().expect("log data directory");
        for target in [LogTarget::Core, LogTarget::Ui] {
            for text in ["first\n", "second\n"] {
                let mut file = DataDirectory::new(directory.path())
                    .open_file_log(target)
                    .expect("open target log");
                file.write_all(text.as_bytes()).expect("append diagnostic");
            }
            assert_eq!(
                fs::read_to_string(directory.path().join("logs").join(target.file_name()))
                    .expect("read target log"),
                "first\nsecond\n"
            );
        }
    }

    #[test]
    fn file_setup_errors_preserve_the_failing_path_and_io_cause() {
        let directory = tempfile::tempdir().expect("log data directory");
        let logs = directory.path().join("logs");
        fs::write(&logs, "obstruction").expect("block log directory");
        let error = DataDirectory::new(directory.path())
            .open_file_log(LogTarget::Core)
            .expect_err("log directory is a file");
        assert!(matches!(&error, InitFileLogError::CreateDirectory { path, .. } if path == &logs));
        assert!(
            error
                .source()
                .and_then(|source| source.downcast_ref::<std::io::Error>())
                .is_some()
        );
        fs::remove_file(&logs).expect("remove obstruction");
        fs::create_dir_all(logs.join("ui.log")).expect("block UI log file");
        let error = DataDirectory::new(directory.path())
            .open_file_log(LogTarget::Ui)
            .expect_err("log file is a directory");
        assert!(
            matches!(&error, InitFileLogError::OpenFile { path, .. } if path == &logs.join("ui.log"))
        );
        assert!(
            error
                .source()
                .and_then(|source| source.downcast_ref::<std::io::Error>())
                .is_some()
        );
    }
}
