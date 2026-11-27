//! Windows-only helper for opening a local directory in File Explorer.

use std::path::Path;
#[cfg(not(test))]
use std::process::{Command, Stdio};

/// Returns the parent directory of a database file path so Explorer opens the containing folder.
#[must_use]
pub(crate) fn parent_directory(file_path: &str) -> &Path {
    let path = Path::new(file_path);
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(path)
}

/// Launches `explorer.exe` at `directory`. Never spawns processes during unit tests.
pub(crate) fn open_folder(directory: &Path) {
    #[cfg(not(test))]
    {
        if let Err(error) = Command::new("explorer.exe")
            .arg(directory)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            tracing::warn!(%error, "could not launch Windows Explorer for directory");
        }
    }
    #[cfg(test)]
    {
        let _ = directory;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parent_directory_returns_containing_folder_for_database_file() {
        assert_eq!(
            parent_directory(r"C:\Users\alice\AppData\Local\Quartz\quartz.db"),
            Path::new(r"C:\Users\alice\AppData\Local\Quartz")
        );
        assert_eq!(
            parent_directory("quartz.db"),
            Path::new("quartz.db"),
            "bare file name falls back to itself instead of an empty path"
        );
    }
}
