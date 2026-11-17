//! Resolves and creates the per-user Quartz data directory.

#[cfg(windows)]
use snafu::ResultExt;
use snafu::{OptionExt, Snafu};
use std::{ffi::OsString, path::PathBuf};

#[derive(Debug, Snafu)]
#[snafu(display("LOCALAPPDATA must name an absolute data directory"))]
pub(crate) struct ResolveDataDirectoryError;

#[cfg(windows)]
pub(crate) fn resolve() -> Result<PathBuf, ResolveDataDirectoryError> {
    from_local_app_data(std::env::var_os("LOCALAPPDATA"))
}

#[cfg(windows)]
#[derive(Debug, Snafu)]
#[snafu(display("could not create data directory"))]
pub(crate) struct CreateDataDirectoryError {
    source: std::io::Error,
}

#[cfg(windows)]
pub(crate) fn create(path: &std::path::Path) -> Result<(), CreateDataDirectoryError> {
    std::fs::create_dir_all(path).context(CreateDataDirectorySnafu)
}

fn from_local_app_data(local: Option<OsString>) -> Result<PathBuf, ResolveDataDirectoryError> {
    local
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .context(ResolveDataDirectorySnafu)
        .map(|path| path.join("Quartz"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_missing_empty_and_relative_directories() {
        for local in [
            None,
            Some(OsString::new()),
            Some(OsString::from("relative")),
        ] {
            assert!(from_local_app_data(local).is_err());
        }
    }

    #[test]
    fn appends_quartz_to_absolute_directory() {
        let local = std::env::temp_dir().join("quartz-directory-test");
        assert_eq!(
            from_local_app_data(Some(local.clone().into_os_string()))
                .expect("absolute data directory"),
            local.join("Quartz")
        );
    }
}
