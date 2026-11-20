//! Resolves the per-user data directory and names its persistent resources.

use snafu::OptionExt;
#[cfg(any(windows, test))]
use snafu::ResultExt;
use snafu::Snafu;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

#[derive(Debug, Snafu)]
#[snafu(display("LOCALAPPDATA must name an absolute data directory"))]
pub(crate) struct ResolveDataDirectoryError;

#[cfg(any(windows, test))]
#[derive(Debug, Snafu)]
#[snafu(display("could not create data directory"))]
pub(crate) struct CreateDataDirectoryError {
    source: std::io::Error,
}

/// The Quartz data root shared by storage, logging, and instance exclusion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DataDirectory(PathBuf);

impl DataDirectory {
    /// Constructs a data root supplied by a caller, such as an isolated test directory.
    pub(crate) fn new(path: impl Into<PathBuf>) -> Self {
        Self(path.into())
    }

    pub(crate) fn resolve() -> Result<Self, ResolveDataDirectoryError> {
        Self::from_local_app_data(std::env::var_os("LOCALAPPDATA"))
    }

    fn from_local_app_data(local: Option<OsString>) -> Result<Self, ResolveDataDirectoryError> {
        local
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .context(ResolveDataDirectorySnafu)
            .map(|path| Self::new(path.join("Quartz")))
    }

    pub(crate) fn as_path(&self) -> &Path {
        &self.0
    }
    #[cfg(any(windows, test))]
    pub(crate) fn create(&self) -> Result<(), CreateDataDirectoryError> {
        std::fs::create_dir_all(&self.0).context(CreateDataDirectorySnafu)
    }
    pub(crate) fn database(&self) -> PathBuf {
        self.0.join("quartz.db")
    }
    #[cfg(any(windows, test))]
    pub(crate) fn logs(&self) -> PathBuf {
        self.0.join("logs")
    }
    pub(crate) fn backups(&self) -> PathBuf {
        self.0.join("backups")
    }
    #[cfg(any(windows, test))]
    pub(crate) fn lock_file(&self) -> PathBuf {
        self.0.join("quartz.lock")
    }
    pub(crate) fn staged_restore(&self) -> PathBuf {
        self.0.join("quartz.restore.db")
    }
}

impl DataDirectory {
    pub(crate) fn available_backups(&self) -> Vec<PathBuf> {
        let entries = match std::fs::read_dir(self.backups()) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
            Err(error) => {
                tracing::warn!(%error, "could not list migration backups");
                return Vec::new();
            }
        };
        let mut files = Vec::new();
        for entry in entries {
            match entry {
                Ok(entry) => {
                    let path = entry.path();
                    if path.extension().is_some_and(|ext| ext == "db") {
                        files.push(path);
                    }
                }
                Err(error) => tracing::warn!(%error, "could not read migration backup entry"),
            }
        }
        files.sort();
        files
    }
}

impl std::fmt::Display for DataDirectory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.as_path().display().fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn creates_only_the_requested_data_root() {
        let temporary = tempfile::tempdir().expect("isolated parent directory");
        let directory = DataDirectory::new(temporary.path().join("Quartz"));
        directory.create().expect("create data root");
        directory.create().expect("existing data root is accepted");
        assert!(directory.as_path().is_dir());
        assert!(!directory.database().exists());
        assert!(!directory.logs().exists());
    }
    #[test]
    fn rejects_missing_empty_and_relative_directories() {
        for local in [
            None,
            Some(OsString::new()),
            Some(OsString::from("relative")),
        ] {
            assert!(DataDirectory::from_local_app_data(local).is_err());
        }
    }
    #[test]
    fn appends_quartz_to_absolute_directory() {
        let local = std::env::temp_dir().join("quartz-directory-test");
        let directory = DataDirectory::from_local_app_data(Some(local.clone().into_os_string()))
            .expect("absolute data directory");
        assert_eq!(directory.as_path(), local.join("Quartz"));
        assert_eq!(directory.database(), directory.as_path().join("quartz.db"));
        assert_eq!(directory.logs(), directory.as_path().join("logs"));
        assert_eq!(directory.backups(), directory.as_path().join("backups"));
        assert_eq!(
            directory.lock_file(),
            directory.as_path().join("quartz.lock")
        );
        assert_eq!(
            directory.staged_restore(),
            directory.as_path().join("quartz.restore.db")
        );
    }
}
