//! Entry-layer tracing setup.

use snafu::Snafu;
use tracing_subscriber::EnvFilter;

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
