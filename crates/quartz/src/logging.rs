//! Entry-layer tracing setup.

use snafu::Snafu;
use tracing_subscriber::EnvFilter;

#[cfg(windows)]
use std::{fs::File, sync::Mutex};

#[cfg(windows)]
#[derive(Debug, Snafu)]
pub(super) enum InitUiLogError {
    #[snafu(display("LOCALAPPDATA must name an absolute data directory"))]
    ResolveDataDirectory,
    #[snafu(display("could not create UI log directory {}", path.display()))]
    CreateDirectory {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    #[snafu(display("could not open UI log file {}", path.display()))]
    OpenFile {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    #[snafu(display("could not initialize UI tracing"))]
    InitializeTracing { source: InitTracingError },
}

/// Installs UI diagnostics in the same data-directory logs folder as the core.
#[cfg(windows)]
pub(super) fn init_ui() -> Result<(), InitUiLogError> {
    use snafu::{OptionExt as _, ResultExt as _};
    use std::{
        fs::{self, OpenOptions},
        path::PathBuf,
    };

    let logs = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .context(ResolveDataDirectorySnafu)?
        .join("Quartz")
        .join("logs");
    fs::create_dir_all(&logs).context(CreateDirectorySnafu { path: &logs })?;
    let path = logs.join("ui.log");
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .context(OpenFileSnafu { path })?;
    init_file(file).context(InitializeTracingSnafu)
}

/// Installs a synchronous file writer without creating a logging thread.
#[cfg(windows)]
pub(super) fn init_file(file: File) -> Result<(), InitTracingError> {
    let filter = EnvFilter::builder()
        .with_regex(false)
        .with_default_directive(tracing::Level::INFO.into())
        .from_env_lossy();
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(Mutex::new(file))
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;
    Ok(())
}

/// Failure to install the process-wide tracing subscriber.
#[derive(Debug, Snafu)]
#[snafu(context(false), display("could not install tracing subscriber"))]
pub(super) struct InitTracingError {
    source: tracing::subscriber::SetGlobalDefaultError,
}

/// Installs stderr diagnostics for commands that have a console.
///
/// # Errors
/// Returns an error if a global tracing subscriber is already installed.
pub(super) fn init() -> Result<(), InitTracingError> {
    let filter = EnvFilter::builder()
        .with_regex(false)
        .with_default_directive(tracing::Level::INFO.into())
        .from_env_lossy();
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_initialization_preserves_source_and_summary() {
        init().expect("first tracing initialization must succeed");
        let error = init().expect_err("second tracing initialization must fail");
        assert_eq!(
            error.to_string(),
            "could not install tracing subscriber",
            "tracing error summary"
        );
        assert!(
            std::error::Error::source(&error)
                .and_then(
                    |source| source.downcast_ref::<tracing::subscriber::SetGlobalDefaultError>()
                )
                .is_some(),
            "tracing initialization must retain its upstream source"
        );
    }
}
