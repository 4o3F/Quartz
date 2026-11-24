//! Diagnostics available before a tracing subscriber or parent console exists.

use std::{
    error::Error,
    io::{self, Write},
};

/// Maximum size before a bootstrap log is reopened and cleared (01 §2).
const MAX_BOOTSTRAP_BYTES: u64 = 64 * 1024;

/// A complete entry-point diagnostic, including the source chain.
#[derive(Debug)]
pub(crate) struct BootstrapDiagnostic(String);
impl BootstrapDiagnostic {
    pub(crate) fn from_error(error: &dyn Error) -> Self {
        Self(snafu::Report::from_error(error).to_string())
    }
    /// Tries both sinks even when one is unavailable.
    fn write<W: Write>(
        &self,
        console: &mut impl Write,
        log: impl FnOnce() -> io::Result<W>,
    ) -> io::Result<()> {
        let console_result = writeln!(console, "{}", self.0);
        let log_result = log().and_then(|mut log| writeln!(log, "{}", self.0));
        log_result.and(console_result)
    }
    pub(crate) fn report(&self) {
        self.report_with(&mut io::stderr().lock(), Self::open_log);
    }
    fn report_with<W: Write>(&self, console: &mut impl Write, log: impl FnOnce() -> io::Result<W>) {
        if let Err(error) = self.write(console, log) {
            let _ = writeln!(
                console,
                "could not write bootstrap diagnostic to both destinations: {error}"
            );
        }
    }
    fn open_log() -> io::Result<std::fs::File> {
        let directory =
            crate::data_directory::DataDirectory::resolve().map_err(io::Error::other)?;
        Self::open_log_in(&directory.as_path().join("logs"))
    }
    fn open_log_in(logs: &std::path::Path) -> io::Result<std::fs::File> {
        std::fs::create_dir_all(logs)?;
        let path = logs.join("bootstrap.log");
        let file = Self::append_log(&path)?;
        if file.metadata()?.len() >= MAX_BOOTSTRAP_BYTES {
            drop(file);
            Self::truncate_log(&path)?;
            return Self::append_log(&path);
        }
        Ok(file)
    }
    fn truncate_log(path: &std::path::Path) -> io::Result<std::fs::File> {
        std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
    }
    fn append_log(path: &std::path::Path) -> io::Result<std::fs::File> {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Debug, snafu::Snafu)]
    #[snafu(display("could not initialize UI diagnostics"))]
    struct StartDiagnosticsError {
        source: io::Error,
    }

    #[test]
    fn bootstrap_failure_preserves_sources_when_file_logging_is_unavailable() {
        let diagnostic = BootstrapDiagnostic::from_error(&StartDiagnosticsError {
            source: io::Error::new(
                io::ErrorKind::PermissionDenied,
                "injected file access failure",
            ),
        });
        let mut stderr = Vec::new();
        let result = diagnostic.write(&mut stderr, || {
            Err::<Vec<u8>, _>(io::Error::from(io::ErrorKind::PermissionDenied))
        });
        assert!(result.is_err());
        let text = String::from_utf8(stderr).expect("diagnostic text");
        assert!(text.contains("could not initialize UI diagnostics"));
        assert!(text.contains("injected file access failure"));
    }
    #[test]
    fn bootstrap_failure_reaches_log_when_console_is_unavailable() {
        struct UnavailableConsole;
        impl Write for UnavailableConsole {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut log = Vec::new();
        let error = io::Error::new(io::ErrorKind::InvalidInput, "invalid command line");
        assert!(
            BootstrapDiagnostic::from_error(&error)
                .write(&mut UnavailableConsole, || Ok(&mut log))
                .is_err()
        );
        assert!(
            String::from_utf8(log)
                .expect("log")
                .contains("invalid command line")
        );
    }
    #[test]
    fn reporting_to_a_broken_console_does_not_panic_and_still_writes_the_log() {
        struct BrokenConsole;
        impl Write for BrokenConsole {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut log = Vec::new();
        BootstrapDiagnostic::from_error(&io::Error::other("injected startup failure"))
            .report_with(&mut BrokenConsole, || Ok(&mut log));
        assert!(
            String::from_utf8(log)
                .expect("log text")
                .contains("injected startup failure")
        );
    }
    #[test]
    fn bootstrap_log_reopen_recreates_a_file_removed_before_truncation() {
        let directory = tempfile::tempdir().expect("temporary log directory");
        let path = directory.path().join("bootstrap.log");
        std::fs::write(&path, b"old diagnostic").expect("old log");
        std::fs::remove_file(&path).expect("cleanup removes the closed log");
        let mut log = BootstrapDiagnostic::truncate_log(&path).expect("recreate at truncation");
        writeln!(log, "new diagnostic").expect("new entry");
        drop(log);
        assert_eq!(
            std::fs::read_to_string(path).expect("log"),
            "new diagnostic\n"
        );
    }
    #[test]
    fn bootstrap_log_reopen_recreates_a_file_removed_after_truncation() {
        let directory = tempfile::tempdir().expect("temporary log directory");
        let path = directory.path().join("bootstrap.log");
        std::fs::write(&path, b"old diagnostic").expect("old log");
        drop(BootstrapDiagnostic::truncate_log(&path).expect("truncate log"));
        std::fs::remove_file(&path).expect("cleanup removes the truncated log");
        let mut log = BootstrapDiagnostic::append_log(&path).expect("recreate at append");
        writeln!(log, "new diagnostic").expect("new entry");
        drop(log);
        assert_eq!(
            std::fs::read_to_string(path).expect("log"),
            "new diagnostic\n"
        );
    }
    #[test]
    fn oversized_bootstrap_log_is_cleared_and_accepts_new_diagnostics() {
        let directory = tempfile::tempdir().expect("temporary log directory");
        let path = directory.path().join("bootstrap.log");
        std::fs::write(
            &path,
            vec![b'x'; usize::try_from(MAX_BOOTSTRAP_BYTES + 1).expect("test size")],
        )
        .expect("oversized log");
        let mut log =
            BootstrapDiagnostic::open_log_in(directory.path()).expect("reopen oversized log");
        writeln!(log, "new diagnostic").expect("new entry");
        drop(log);
        assert_eq!(
            std::fs::read_to_string(path).expect("read log"),
            "new diagnostic\n"
        );
    }
}
